use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand, ValueEnum};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use futures_util::TryStreamExt as _;
use indicatif::{ProgressBar, ProgressStyle};
use octocrab::Octocrab;
use octocrab::models::workflows::Conclusion;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::File;
use std::io::Write;
use std::path::Path;

// Base configuration structure
#[derive(Debug, Serialize, Deserialize)]
struct BaseConfig {
    #[serde(default)]
    github_token: Option<String>,
    owner: String,
    repo: String,
}

// CLI arguments structure
#[derive(Parser, Debug, Clone, Serialize)]
#[command(author, version, about, long_about = None)]
struct Cli {
    /// GitHub repository owner
    #[arg(short, long, global = true)]
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,

    /// GitHub repository name
    #[arg(short, long, global = true)]
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,

    /// Path to config file
    #[arg(short, long, global = true)]
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<String>,

    /// Number of items to fetch/analyze
    #[arg(short, long, default_value = "10", global = true)]
    limit: u32,

    /// Output format
    #[arg(long, global = true)]
    format: Option<OutputFormat>,

    /// Output file (if not specified, outputs to stdout)
    /// Format will be automatically inferred from file extension (.json, .md, .txt)
    #[arg(long, global = true)]
    output: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug, Clone, Serialize)]
enum Commands {
    /// List all available workflow IDs
    List,
    /// Describe workflow runs
    Describe {
        /// Specific workflow file to check (e.g., .github/workflows/ci.yml)
        #[arg(short, long)]
        workflow: String,
    },
    /// Analyze workflow job failure patterns
    Analyze {
        /// Specific workflow file to analyze
        #[arg(short, long)]
        workflow: String,

        /// Number of workflow runs to analyze
        #[arg(short, long, default_value = "100")]
        limit: u32,

        /// Minimum failure rate to include in detailed analysis (0.0 to 1.0)
        #[arg(short, long, default_value = "0.1")]
        min_failure_rate: Option<f64>,

        /// Maximum failure rate to include in detailed analysis (0.0 to 1.0)
        #[arg(long)]
        max_failure_rate: Option<f64>,

        /// Branch to analyze (e.g., main, develop)
        #[arg(short, long)]
        branch: Option<String>,
    },
}

#[derive(Debug, Clone, ValueEnum, Serialize, PartialEq)]
enum OutputFormat {
    Text,
    Json,
    Markdown,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Build configuration from multiple sources
    let mut figment = Figment::new()
        .merge(Toml::file("config.toml"))
        .merge(Env::prefixed("GHFD_").split("_"));

    // Add config file if specified
    if let Some(config_path) = &cli.config {
        figment = figment.merge(Toml::file(config_path));
    }

    // Merge CLI arguments last (highest priority)
    let config: BaseConfig = figment.merge(Serialized::defaults(cli.clone())).extract()?;

    let octocrab = get_octocrab(&config).await?;

    match cli.command {
        Commands::List => {
            list_workflows(&octocrab, &config, cli.limit).await?;
        }
        Commands::Describe { workflow } => {
            describe_workflow_runs(&octocrab, &config, cli.limit, Some(workflow)).await?;
        }
        Commands::Analyze {
            workflow,
            limit,
            min_failure_rate,
            max_failure_rate,
            branch,
        } => {
            let analysis = analyze_workflow_runs(
                &octocrab,
                &config,
                limit,
                workflow,
                min_failure_rate,
                max_failure_rate,
                branch,
            )
            .await?;

            // Determine the output format
            let output_format = if let Some(format) = cli.format {
                format
            } else if let Some(output_path) = cli.output.as_deref() {
                infer_format_from_extension(output_path).unwrap_or(OutputFormat::Text)
            } else {
                OutputFormat::Text
            };

            let output_content = match output_format {
                OutputFormat::Json => serde_json::to_string_pretty(&analysis)?,
                OutputFormat::Markdown => {
                    let mut content = Vec::new();
                    write_workflow_analysis_markdown(&analysis, &mut content)?;
                    String::from_utf8(content)?
                }
                OutputFormat::Text => {
                    let mut content = Vec::new();
                    write_workflow_analysis(&analysis, &mut content)?;
                    String::from_utf8(content)?
                }
            };

            if let Some(output_path) = cli.output {
                let mut file = File::create(&output_path)?;
                write!(file, "{}", output_content)?;
                eprintln!("Analysis results written to: {}", output_path);
            } else {
                println!("{}", output_content);
            }
        }
    }

    Ok(())
}

// Analysis structs

#[derive(Debug, Serialize, Deserialize)]
struct WorkflowRun {
    id: u64,
    name: String,
    status: String,
    conclusion: Option<String>,
    created_at: String,
    updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct JobStats {
    total_runs: u32,
    failures: u32,
    last_failure: Option<DateTime<Utc>>,
    failure_rate: f64,
    avg_duration: Option<Duration>,
    min_duration: Option<Duration>,
    max_duration: Option<Duration>,
    consecutive_failures: u32,
    current_streak: u32,
    longest_streak: u32,
    last_status: String,
    branch: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkflowStats {
    total_runs: i32,
    skipped_runs: i32,
    failed_runs: i32,
    failure_rate: f64,
    avg_duration: Option<Duration>,
    min_duration: Option<Duration>,
    max_duration: Option<Duration>,
    consecutive_failures: i32,
    current_streak: i32,
    longest_streak: i32,
    last_status: String,
}

#[derive(Debug, Serialize, Deserialize)]
struct JobAnalysisSummary {
    total_jobs: usize,
    total_job_runs: u32,
    total_failures: u32,
    overall_failure_rate: f64,
    jobs_with_failures: usize,
    jobs_never_fail: usize,
    percent_jobs_failed: f64,
    jobs_above_threshold: usize,
    percent_above_threshold: f64,
    jobs_below_threshold: usize,
    percent_below_threshold: f64,
    avg_job_duration_minutes: i64,
    max_streak: u32,
    currently_failing: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkflowAnalysisSummary {
    total_runs: i32,
    skipped_runs: i32,
    failed_runs: i32,
    failure_rate: f64,
    avg_duration_minutes: Option<i64>,
    min_duration_minutes: Option<i64>,
    max_duration_minutes: Option<i64>,
    consecutive_failures: i32,
    current_streak: i32,
    longest_streak: i32,
}

#[derive(Debug, Serialize, Deserialize)]
struct WorkflowAnalysis {
    job_stats: Vec<(String, JobStats)>,
    workflow_stats: WorkflowStats,
    job_summary: JobAnalysisSummary,
    workflow_summary: WorkflowAnalysisSummary,
    analysis_params: AnalysisParams,
}

#[derive(Debug, Serialize, Deserialize)]
struct AnalysisParams {
    limit: u32,
    workflow: String,
    min_failure_rate: Option<f64>,
    max_failure_rate: Option<f64>,
    branch: Option<String>,
}

async fn get_octocrab(config: &BaseConfig) -> Result<Octocrab> {
    if let Some(token) = &config.github_token {
        Ok(Octocrab::builder().personal_token(token.clone()).build()?)
    } else {
        println!("No GitHub token provided. Using unauthenticated requests (rate limited).");
        Ok(Octocrab::default())
    }
}

async fn list_workflows(octocrab: &Octocrab, config: &BaseConfig, limit: u32) -> Result<()> {
    println!("Listing workflows for {}/{}...", config.owner, config.repo);

    let workflows = octocrab
        .workflows(&config.owner, &config.repo)
        .list()
        .per_page(limit as u8)
        .send()
        .await?;

    for workflow in workflows.items {
        println!("ID: {}", workflow.id);
        println!("Name: {}", workflow.name);
        println!("Path: {}", workflow.path);
        println!("State: {}", workflow.state);
        println!("---");
    }

    Ok(())
}

async fn describe_workflow_runs(
    octocrab: &Octocrab,
    config: &BaseConfig,
    limit: u32,
    workflow: Option<String>,
) -> Result<()> {
    println!(
        "Fetching {} workflow runs for {}/{}...",
        limit, config.owner, config.repo
    );

    let runs = octocrab
        .workflows(&config.owner, &config.repo)
        .list_runs(workflow.as_deref().unwrap_or(""))
        .per_page(limit as u8)
        .send()
        .await?;

    for run in runs.items {
        println!("Workflow: {} (ID: {})", run.name, run.id);
        println!("Status: {}", run.status);
        println!(
            "Conclusion: {}",
            run.conclusion.unwrap_or_else(|| "None".to_string())
        );
        println!("Created at: {}", run.created_at);
        println!("Updated at: {}", run.updated_at);
        println!("---");
    }

    Ok(())
}

async fn analyze_workflow_runs(
    octocrab: &Octocrab,
    config: &BaseConfig,
    limit: u32,
    workflow: String,
    min_failure_rate: Option<f64>,
    max_failure_rate: Option<f64>,
    branch: Option<String>,
) -> Result<WorkflowAnalysis> {
    eprintln!(
        "Analyzing {} workflow runs for {}/{}...",
        limit, config.owner, config.repo
    );

    let query = octocrab.workflows(&config.owner, &config.repo);
    let mut query = query.list_runs(&workflow).per_page(limit as u8);

    // Add branch filter if specified
    if let Some(branch_name) = &branch {
        eprintln!("Filtering runs for branch: {}", branch_name);
        query = query.branch(branch_name);
    }

    let mut runs = Vec::new();
    let mut paginated = Box::pin(query.send().await?.into_stream(octocrab));

    // Create progress bar for workflow collection
    let progress_bar = ProgressBar::new(limit as u64);
    progress_bar.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.cyan/blue} {pos:>7}/{len:7} {msg}")
            .unwrap()
            .progress_chars("█░"),
    );
    progress_bar.set_message("Collecting workflow runs...");

    while let Some(run) = paginated.try_next().await? {
        runs.push(run);
        progress_bar.inc(1);
        if runs.len() >= limit as usize {
            break;
        }
    }
    progress_bar.finish_with_message("Workflow runs collected");

    eprintln!("Found {} workflow runs", runs.len());

    // Create a vector of futures for fetching jobs
    let mut job_futures = Vec::new();
    for run in runs {
        let octocrab = octocrab.clone();
        let owner = config.owner.clone();
        let repo = config.repo.clone();
        let run_id = run.id;

        job_futures.push(tokio::spawn(async move {
            let job_stream = octocrab
                .workflows(&owner, &repo)
                .list_jobs(run_id)
                .send()
                .await?
                .into_stream(&octocrab);
            let mut jobs = Vec::new();
            let mut paginated = Box::pin(job_stream);
            while let Some(job) = paginated.try_next().await? {
                jobs.push(job);
            }
            Ok::<_, anyhow::Error>(jobs)
        }));
    }

    // Create progress bar for job fetching
    let job_progress_bar = ProgressBar::new(job_futures.len() as u64);
    job_progress_bar.set_style(
        ProgressStyle::default_bar()
            .template("[{elapsed_precise}] {bar:40.green/yellow} {pos:>7}/{len:7} {msg}")
            .unwrap()
            .progress_chars("█░"),
    );
    job_progress_bar.set_message("Fetching job details...");

    // Wait for all job fetches to complete
    let mut job_results = Vec::new();
    for future in job_futures {
        let result = future.await;
        job_results.push(result);
        job_progress_bar.inc(1);
    }
    job_progress_bar.finish_with_message("Job details fetched");

    let mut job_stats: HashMap<String, JobStats> = HashMap::new();
    let mut workflow_stats = WorkflowStats {
        total_runs: 0,
        failed_runs: 0,
        failure_rate: 0.0,
        skipped_runs: 0,
        avg_duration: None,
        min_duration: None,
        max_duration: None,
        consecutive_failures: 0,
        current_streak: 0,
        longest_streak: 0,
        last_status: "unknown".to_string(),
    };

    // Process all job results
    for result in job_results {
        match result {
            Ok(Ok(jobs)) => {
                workflow_stats.total_runs += 1;
                // if there were no jobs that means it was cancelled
                let workflow_skipped = jobs.is_empty();
                let mut workflow_failed = false;
                let mut workflow_duration: Option<Duration> = None;

                for job in jobs {
                    let stats = job_stats.entry(job.name.clone()).or_insert(JobStats {
                        total_runs: 0,
                        failures: 0,
                        last_failure: None,
                        failure_rate: 0.0,
                        avg_duration: None,
                        min_duration: None,
                        max_duration: None,
                        consecutive_failures: 0,
                        current_streak: 0,
                        longest_streak: 0,
                        last_status: job
                            .conclusion
                            .as_ref()
                            .map_or("unknown".to_string(), |c| format!("{:?}", c)),
                        branch: branch.clone(),
                    });

                    stats.total_runs += 1;

                    // Update duration statistics
                    if let (started_at, Some(completed_at)) = (job.started_at, job.completed_at) {
                        let duration = completed_at.signed_duration_since(started_at);
                        stats.avg_duration = Some(match stats.avg_duration {
                            Some(avg) => {
                                (avg * (stats.total_runs - 1) as i32 + duration)
                                    / stats.total_runs as i32
                            }
                            None => duration,
                        });
                        stats.min_duration = Some(match stats.min_duration {
                            Some(min) => min.min(duration),
                            None => duration,
                        });
                        stats.max_duration = Some(match stats.max_duration {
                            Some(max) => max.max(duration),
                            None => duration,
                        });

                        // Track workflow duration (use the longest job duration)
                        workflow_duration = Some(match workflow_duration {
                            Some(d) => d.max(duration),
                            None => duration,
                        });
                    }

                    // Update failure statistics
                    if matches!(
                        job.conclusion,
                        Some(Conclusion::Failure | Conclusion::Cancelled | Conclusion::TimedOut)
                    ) {
                        stats.failures += 1;
                        stats.last_failure = Some(job.completed_at.unwrap_or_else(Utc::now));
                        stats.consecutive_failures += 1;
                        stats.current_streak = stats.consecutive_failures;
                        stats.longest_streak = stats.longest_streak.max(stats.current_streak);
                        workflow_failed = true;
                    } else {
                        stats.consecutive_failures = 0;
                    }

                    stats.failure_rate = stats.failures as f64 / stats.total_runs as f64;
                    stats.last_status = job
                        .conclusion
                        .map_or("unknown".to_string(), |c| format!("{:?}", c));
                }

                // Update workflow statistics
                if workflow_failed {
                    workflow_stats.failed_runs += 1;
                    workflow_stats.consecutive_failures += 1;
                    workflow_stats.current_streak = workflow_stats.consecutive_failures;
                    workflow_stats.longest_streak = workflow_stats
                        .longest_streak
                        .max(workflow_stats.current_streak);
                } else if workflow_skipped {
                    workflow_stats.skipped_runs += 1;
                } else {
                    workflow_stats.consecutive_failures = 0;
                }

                // Update workflow duration statistics
                if let Some(duration) = workflow_duration {
                    workflow_stats.avg_duration = Some(match workflow_stats.avg_duration {
                        Some(avg) => {
                            (avg * (workflow_stats.total_runs - 1) + duration)
                                / workflow_stats.total_runs
                        }
                        None => duration,
                    });
                    workflow_stats.min_duration = Some(match workflow_stats.min_duration {
                        Some(min) => min.min(duration),
                        None => duration,
                    });
                    workflow_stats.max_duration = Some(match workflow_stats.max_duration {
                        Some(max) => max.max(duration),
                        None => duration,
                    });
                }
            }
            Ok(Err(e)) => {
                eprintln!("Error fetching jobs: {}: {:?}", e, e);
            }
            Err(e) => {
                eprintln!("Error in task: {}", e);
            }
        }
    }

    workflow_stats.failure_rate = workflow_stats.failed_runs as f64
        / (workflow_stats.total_runs - workflow_stats.skipped_runs) as f64;

    // Sort jobs by failure rate
    let mut sorted_jobs: Vec<_> = job_stats.into_iter().collect();
    sorted_jobs.sort_by(|a, b| b.1.failure_rate.partial_cmp(&a.1.failure_rate).unwrap());

    // Calculate job statistics
    let total_runs: u32 = sorted_jobs.iter().map(|(_, stats)| stats.total_runs).sum();
    let total_failures: u32 = sorted_jobs.iter().map(|(_, stats)| stats.failures).sum();
    let overall_failure_rate = total_failures as f64 / total_runs as f64;

    let jobs_with_failures = sorted_jobs
        .iter()
        .filter(|(_, stats)| stats.failures > 0)
        .count();
    let jobs_never_fail = sorted_jobs.len() - jobs_with_failures;
    let percent_jobs_failed = (jobs_with_failures as f64 / sorted_jobs.len() as f64) * 100.0;

    let jobs_above_threshold = min_failure_rate.map_or(0, |min| {
        sorted_jobs
            .iter()
            .filter(|(_, stats)| stats.failure_rate >= min)
            .count()
    });
    let percent_above_threshold = (jobs_above_threshold as f64 / sorted_jobs.len() as f64) * 100.0;
    let jobs_below_threshold = max_failure_rate.map_or(0, |max| {
        sorted_jobs
            .iter()
            .filter(|(_, stats)| stats.failure_rate <= max)
            .count()
    });
    let percent_below_threshold = (jobs_below_threshold as f64 / sorted_jobs.len() as f64) * 100.0;

    let avg_durations: Vec<Duration> = sorted_jobs
        .iter()
        .filter_map(|(_, stats)| stats.avg_duration)
        .collect();
    let avg_job_duration = if !avg_durations.is_empty() {
        let total_minutes: i64 = avg_durations.iter().map(|d| d.num_minutes()).sum();
        total_minutes / avg_durations.len() as i64
    } else {
        0
    };

    let max_streak = sorted_jobs
        .iter()
        .map(|(_, stats)| stats.longest_streak)
        .max()
        .unwrap_or(0);

    let currently_failing = sorted_jobs
        .iter()
        .filter(|(_, stats)| stats.current_streak > 0)
        .count();

    let job_summary = JobAnalysisSummary {
        total_jobs: sorted_jobs.len(),
        total_job_runs: total_runs,
        total_failures,
        overall_failure_rate,
        jobs_with_failures,
        jobs_never_fail,
        percent_jobs_failed,
        jobs_above_threshold,
        percent_above_threshold,
        jobs_below_threshold,
        percent_below_threshold,
        avg_job_duration_minutes: avg_job_duration,
        max_streak,
        currently_failing,
    };

    let workflow_summary = WorkflowAnalysisSummary {
        total_runs: workflow_stats.total_runs,
        skipped_runs: workflow_stats.skipped_runs,
        failed_runs: workflow_stats.failed_runs,
        failure_rate: workflow_stats.failure_rate,
        avg_duration_minutes: workflow_stats.avg_duration.map(|d| d.num_minutes()),
        min_duration_minutes: workflow_stats.min_duration.map(|d| d.num_minutes()),
        max_duration_minutes: workflow_stats.max_duration.map(|d| d.num_minutes()),
        consecutive_failures: workflow_stats.consecutive_failures,
        current_streak: workflow_stats.current_streak,
        longest_streak: workflow_stats.longest_streak,
    };

    let analysis_params = AnalysisParams {
        limit,
        workflow,
        min_failure_rate,
        max_failure_rate,
        branch,
    };

    Ok(WorkflowAnalysis {
        job_stats: sorted_jobs,
        workflow_stats,
        job_summary,
        workflow_summary,
        analysis_params,
    })
}

fn create_separator(text: &str) -> String {
    let text_len = text.len();
    let width = term_size::dimensions().map(|(w, _)| w).unwrap_or(text_len);

    let separator_len = std::cmp::min(width, text_len) - 2;
    "─".repeat(separator_len)
}

fn write_workflow_analysis(analysis: &WorkflowAnalysis, writer: &mut impl Write) -> Result<()> {
    let params = &analysis.analysis_params;

    writeln!(writer, "  ────────────────────────────────────────────────")?;
    writeln!(
        writer,
        "  Detailed Job Analysis (min failure rate: {:.1}%, max failure rate: {})",
        params
            .min_failure_rate
            .map_or_else(|| "none".to_string(), |r| format!("{:.1}%", r * 100.0)),
        params
            .max_failure_rate
            .map_or_else(|| "none".to_string(), |r| format!("{:.1}%", r * 100.0))
    )?;
    writeln!(writer, "  ────────────────────────────────────────────────")?;

    for (job_name, stats) in &analysis.job_stats {
        if params
            .min_failure_rate
            .is_none_or(|min| stats.failure_rate >= min)
            && params
                .max_failure_rate
                .is_none_or(|max| stats.failure_rate <= max)
        {
            write_job_details(job_name, stats, writer)?;
        }
    }

    write_job_summary(&analysis.job_summary, writer)?;
    write_workflow_summary(&analysis.workflow_summary, writer)?;
    Ok(())
}

fn write_workflow_analysis_markdown(
    analysis: &WorkflowAnalysis,
    writer: &mut impl Write,
) -> Result<()> {
    let params = &analysis.analysis_params;

    writeln!(writer, "# GitHub Workflow Analysis")?;
    writeln!(writer)?;

    // Analysis parameters
    writeln!(writer, "## Analysis Parameters")?;
    writeln!(writer)?;
    writeln!(writer, "- **Workflow Runs Analyzed**: {}", params.limit)?;
    writeln!(writer, "- **Workflow**: {}", params.workflow)?;
    if let Some(branch) = &params.branch {
        writeln!(writer, "- **Branch**: {}", branch)?;
    }
    writeln!(writer)?;

    // Job details
    writeln!(writer, "## Job Analysis")?;
    writeln!(writer)?;

    let filtered_jobs: Vec<_> = analysis
        .job_stats
        .iter()
        .filter(|(_, stats)| {
            params
                .min_failure_rate
                .is_none_or(|min| stats.failure_rate >= min)
                && params
                    .max_failure_rate
                    .is_none_or(|max| stats.failure_rate <= max)
        })
        .collect();

    if !filtered_jobs.is_empty() {
        writeln!(writer, "### Detailed Job Statistics")?;
        writeln!(writer)?;
        if let Some(min_rate) = params.min_failure_rate {
            writeln!(
                writer,
                "- **Minimum Failure Rate**: {:.1}%",
                min_rate * 100.0
            )?;
        }
        if let Some(max_rate) = params.max_failure_rate {
            writeln!(
                writer,
                "- **Maximum Failure Rate**: {:.1}%",
                max_rate * 100.0
            )?;
        }
        writeln!(writer)?;
        writeln!(
            writer,
            "| Job Name | Total Runs | Failures | Failure Rate | Avg Duration | Current Failure Streak | Longest Failure Streak | Last Status |"
        )?;
        writeln!(
            writer,
            "|----------|------------|----------|--------------|--------------|------------------------|------------------------|-------------|"
        )?;

        for (job_name, stats) in filtered_jobs {
            let avg_duration = stats
                .avg_duration
                .map(|d| format!("{}m", d.num_minutes()))
                .unwrap_or_else(|| "N/A".to_string());

            writeln!(
                writer,
                "| {} | {} | {} | {:.1}% | {} | {} | {} | {} |",
                job_name,
                stats.total_runs,
                stats.failures,
                stats.failure_rate * 100.0,
                avg_duration,
                stats.current_streak,
                stats.longest_streak,
                stats.last_status
            )?;
        }
        writeln!(writer)?;
    }

    write_job_summary_markdown(&analysis.job_summary, writer)?;
    write_workflow_summary_markdown(&analysis.workflow_summary, writer)?;
    Ok(())
}

fn write_job_details(job_name: &str, stats: &JobStats, writer: &mut impl Write) -> Result<()> {
    let job_header = format!("  Job: {}", job_name);
    writeln!(writer, "\n{}", job_header)?;
    writeln!(writer, "  {}", create_separator(&job_header))?;
    writeln!(writer, "  • Total Runs:     {}", stats.total_runs)?;
    writeln!(writer, "  • Failures:       {}", stats.failures)?;
    writeln!(
        writer,
        "  • Failure Rate:   {:.1}%",
        stats.failure_rate * 100.0
    )?;
    if let Some(last_failure) = stats.last_failure {
        writeln!(writer, "  • Last Failure:   {}", last_failure)?;
    }
    if let Some(avg_duration) = stats.avg_duration {
        writeln!(
            writer,
            "  • Avg Duration:   {} minutes",
            avg_duration.num_minutes()
        )?;
    }
    if let Some(max_duration) = stats.max_duration {
        writeln!(
            writer,
            "  • Max Duration:   {} minutes",
            max_duration.num_minutes()
        )?;
    }
    writeln!(
        writer,
        "  • Current Failure Streak: {}",
        stats.current_streak
    )?;
    writeln!(
        writer,
        "  • Longest Failure Streak: {}",
        stats.longest_streak
    )?;
    writeln!(writer, "  • Last Status:    {}", stats.last_status)?;
    Ok(())
}

fn write_job_summary(summary: &JobAnalysisSummary, writer: &mut impl Write) -> Result<()> {
    writeln!(
        writer,
        "\n╔════════════════════════════════════════════════════════════╗"
    )?;
    writeln!(
        writer,
        "║                     Job Analysis Summary                   ║"
    )?;
    writeln!(
        writer,
        "╚════════════════════════════════════════════════════════════╝"
    )?;
    writeln!(writer)?;
    writeln!(writer, "  • Total Jobs:        {}", summary.total_jobs)?;
    writeln!(writer, "  • Total Job Runs:    {}", summary.total_job_runs)?;
    writeln!(writer, "  • Total Failures:    {}", summary.total_failures)?;
    writeln!(
        writer,
        "  • Overall Rate:      {:.1}%",
        summary.overall_failure_rate * 100.0
    )?;
    writeln!(
        writer,
        "  • Failed Jobs:       {}/{} ({:.1}%)",
        summary.jobs_with_failures, summary.total_jobs, summary.percent_jobs_failed
    )?;
    writeln!(
        writer,
        "  • Never Failed:      {}/{} ({:.1}%)",
        summary.jobs_never_fail,
        summary.total_jobs,
        (summary.jobs_never_fail as f64 / summary.total_jobs as f64) * 100.0
    )?;
    if summary.jobs_above_threshold > 0 {
        writeln!(
            writer,
            "  • Above Threshold:   {}/{} ({:.1}%)",
            summary.jobs_above_threshold, summary.total_jobs, summary.percent_above_threshold
        )?;
    }
    if summary.jobs_below_threshold > 0 {
        writeln!(
            writer,
            "  • Below Threshold:   {}/{} ({:.1}%)",
            summary.jobs_below_threshold, summary.total_jobs, summary.percent_below_threshold
        )?;
    }
    writeln!(
        writer,
        "  • Avg Duration:      {} minutes",
        summary.avg_job_duration_minutes
    )?;
    writeln!(writer, "  • Longest Failure Streak: {}", summary.max_streak)?;
    writeln!(
        writer,
        "  • Currently Failing: {}",
        summary.currently_failing
    )?;
    Ok(())
}

fn write_workflow_summary(
    summary: &WorkflowAnalysisSummary,
    writer: &mut impl Write,
) -> Result<()> {
    writeln!(
        writer,
        "\n╔════════════════════════════════════════════════════════════╗"
    )?;
    writeln!(
        writer,
        "║                     Workflow Analysis                      ║"
    )?;
    writeln!(
        writer,
        "╚════════════════════════════════════════════════════════════╝"
    )?;
    writeln!(writer)?;
    writeln!(writer, "  • Total Runs:        {}", summary.total_runs)?;
    writeln!(writer, "  • Skipped Runs:      {}", summary.skipped_runs)?;
    writeln!(writer, "  • Failed Runs:       {}", summary.failed_runs)?;
    writeln!(
        writer,
        "  • Failure Rate:      {:.1}%",
        summary.failure_rate * 100.0
    )?;
    if let Some(avg_duration) = summary.avg_duration_minutes {
        writeln!(writer, "  • Avg Duration:      {} minutes", avg_duration)?;
    }
    if let Some(min_duration) = summary.min_duration_minutes {
        writeln!(writer, "  • Min Duration:      {} minutes", min_duration)?;
    }
    if let Some(max_duration) = summary.max_duration_minutes {
        writeln!(writer, "  • Max Duration:      {} minutes", max_duration)?;
    }
    writeln!(
        writer,
        "  • Current Failure Streak: {}",
        summary.current_streak
    )?;
    writeln!(
        writer,
        "  • Longest Failure Streak: {}",
        summary.longest_streak
    )?;
    Ok(())
}

fn write_job_summary_markdown(summary: &JobAnalysisSummary, writer: &mut impl Write) -> Result<()> {
    writeln!(writer, "## Job Summary")?;
    writeln!(writer)?;

    writeln!(writer, "### Key Metrics")?;
    writeln!(writer)?;
    writeln!(writer, "- **Total Jobs**: {}", summary.total_jobs)?;
    writeln!(writer, "- **Total Job Runs**: {}", summary.total_job_runs)?;
    writeln!(writer, "- **Total Failures**: {}", summary.total_failures)?;
    writeln!(
        writer,
        "- **Overall Failure Rate**: {:.1}%",
        summary.overall_failure_rate * 100.0
    )?;
    writeln!(writer)?;

    writeln!(writer, "### Job Categories")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "- **Jobs with Failures**: {}/{} ({:.1}%)",
        summary.jobs_with_failures, summary.total_jobs, summary.percent_jobs_failed
    )?;
    writeln!(
        writer,
        "- **Jobs Never Failed**: {}/{} ({:.1}%)",
        summary.jobs_never_fail,
        summary.total_jobs,
        (summary.jobs_never_fail as f64 / summary.total_jobs as f64) * 100.0
    )?;

    if summary.jobs_above_threshold > 0 {
        writeln!(
            writer,
            "- **Jobs Above Threshold**: {}/{} ({:.1}%)",
            summary.jobs_above_threshold, summary.total_jobs, summary.percent_above_threshold
        )?;
    }
    if summary.jobs_below_threshold > 0 {
        writeln!(
            writer,
            "- **Jobs Below Threshold**: {}/{} ({:.1}%)",
            summary.jobs_below_threshold, summary.total_jobs, summary.percent_below_threshold
        )?;
    }
    writeln!(writer)?;

    writeln!(writer, "### Performance Metrics")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "- **Average Job Duration**: {} minutes",
        summary.avg_job_duration_minutes
    )?;
    writeln!(
        writer,
        "- **Longest Failure Streak**: {}",
        summary.max_streak
    )?;
    writeln!(
        writer,
        "- **Currently Failing Jobs**: {}",
        summary.currently_failing
    )?;
    writeln!(writer)?;
    Ok(())
}

fn write_workflow_summary_markdown(
    summary: &WorkflowAnalysisSummary,
    writer: &mut impl Write,
) -> Result<()> {
    writeln!(writer, "## Workflow Summary")?;
    writeln!(writer)?;

    writeln!(writer, "### Run Statistics")?;
    writeln!(writer)?;
    writeln!(writer, "- **Total Runs**: {}", summary.total_runs)?;
    writeln!(writer, "- **Skipped Runs**: {}", summary.skipped_runs)?;
    writeln!(writer, "- **Failed Runs**: {}", summary.failed_runs)?;
    writeln!(
        writer,
        "- **Failure Rate**: {:.1}%",
        summary.failure_rate * 100.0
    )?;
    writeln!(writer)?;

    writeln!(writer, "### Duration Statistics")?;
    writeln!(writer)?;
    if let Some(avg_duration) = summary.avg_duration_minutes {
        writeln!(writer, "- **Average Duration**: {} minutes", avg_duration)?;
    }
    if let Some(min_duration) = summary.min_duration_minutes {
        writeln!(writer, "- **Minimum Duration**: {} minutes", min_duration)?;
    }
    if let Some(max_duration) = summary.max_duration_minutes {
        writeln!(writer, "- **Maximum Duration**: {} minutes", max_duration)?;
    }
    writeln!(writer)?;

    writeln!(writer, "### Failure Patterns")?;
    writeln!(writer)?;
    writeln!(
        writer,
        "- **Current Failure Streak**: {}",
        summary.current_streak
    )?;
    writeln!(
        writer,
        "- **Longest Failure Streak**: {}",
        summary.longest_streak
    )?;
    writeln!(
        writer,
        "- **Consecutive Failures**: {}",
        summary.consecutive_failures
    )?;
    Ok(())
}

fn infer_format_from_extension(output_path: &str) -> Option<OutputFormat> {
    let path = Path::new(output_path);
    if let Some(extension) = path.extension() {
        match extension.to_str()?.to_lowercase().as_str() {
            "json" => Some(OutputFormat::Json),
            "md" | "markdown" => Some(OutputFormat::Markdown),
            "txt" | "text" => Some(OutputFormat::Text),
            _ => None,
        }
    } else {
        None
    }
}
