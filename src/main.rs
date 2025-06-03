use anyhow::Result;
use chrono::{DateTime, Duration, Utc};
use clap::{Parser, Subcommand};
use figment::{
    Figment,
    providers::{Env, Format, Serialized, Toml},
};
use futures_util::TryStreamExt as _;
use octocrab::Octocrab;
use octocrab::models::workflows::Conclusion;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
    #[arg(short, long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,

    /// GitHub repository name
    #[arg(short, long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    repo: Option<String>,

    /// Path to config file
    #[arg(short, long)]
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug, Clone, Serialize)]
enum Commands {
    /// List all available workflow IDs
    List {
        /// Number of workflows to fetch
        #[arg(short, long, default_value = "10")]
        limit: Option<u32>,
    },
    /// Describe workflow runs
    Describe {
        /// Number of workflow runs to fetch
        #[arg(short, long, default_value = "10")]
        limit: Option<u32>,

        /// Specific workflow file to check (e.g., .github/workflows/ci.yml)
        #[arg(short, long)]
        workflow: Option<String>,
    },
    /// Analyze workflow job failure patterns
    Analyze {
        /// Number of workflow runs to analyze
        #[arg(short, long, default_value = "100")]
        limit: Option<u32>,

        /// Specific workflow file to analyze
        #[arg(short, long)]
        workflow: Option<String>,

        /// Minimum failure rate to report (0.0 to 1.0)
        #[arg(short, long, default_value = "0.1")]
        min_failure_rate: Option<f64>,

        /// Branch to analyze (e.g., main, develop)
        #[arg(short, long)]
        branch: Option<String>,
    },
}

fn default_limit() -> u32 {
    10
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
    workflow: Option<String>,
    min_failure_rate: f64,
    branch: Option<String>,
) -> Result<()> {
    println!(
        "Analyzing {} workflow runs for {}/{}...",
        limit, config.owner, config.repo
    );

    let query = octocrab.workflows(&config.owner, &config.repo);
    let mut query = query
        .list_runs(workflow.as_deref().unwrap_or(""))
        .per_page(limit as u8);

    // Add branch filter if specified
    if let Some(branch_name) = &branch {
        println!("Filtering runs for branch: {}", branch_name);
        query = query.branch(branch_name);
    }

    let mut runs = Vec::new();
    let mut paginated = Box::pin(query.send().await?.into_stream(octocrab));
    while let Some(run) = paginated.try_next().await? {
        runs.push(run);
        if runs.len() >= limit as usize {
            break;
        }
    }

    println!("Found {} workflow runs", runs.len());

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

    // Wait for all job fetches to complete
    let job_results = futures::future::join_all(job_futures).await;

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
                println!("Error fetching jobs: {}: {:?}", e, e);
            }
            Err(e) => {
                println!("Error in task: {}", e);
            }
        }
    }

    workflow_stats.failure_rate = workflow_stats.failed_runs as f64
        / (workflow_stats.total_runs - workflow_stats.skipped_runs) as f64;

    // Sort jobs by failure rate
    let mut sorted_jobs: Vec<_> = job_stats.into_iter().collect();
    sorted_jobs.sort_by(|a, b| b.1.failure_rate.partial_cmp(&a.1.failure_rate).unwrap());

    println!("\n╔════════════════════════════════════════════════════════════╗");
    println!("║                     Workflow Analysis                        ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!("\n  Workflow Statistics:");
    println!("  ───────────────────");
    println!("  • Total Runs:        {}", workflow_stats.total_runs);
    println!("  • Skipped Runs:      {}", workflow_stats.skipped_runs);
    println!("  • Failed Runs:       {}", workflow_stats.failed_runs);
    println!(
        "  • Failure Rate:      {:.1}%",
        workflow_stats.failure_rate * 100.0
    );
    if let Some(avg_duration) = workflow_stats.avg_duration {
        println!(
            "  • Avg Duration:      {} minutes",
            avg_duration.num_minutes()
        );
    }
    if let Some(min_duration) = workflow_stats.min_duration {
        println!(
            "  • Min Duration:      {} minutes",
            min_duration.num_minutes()
        );
    }
    if let Some(max_duration) = workflow_stats.max_duration {
        println!(
            "  • Max Duration:      {} minutes",
            max_duration.num_minutes()
        );
    }
    println!("  • Current Streak:    {}", workflow_stats.current_streak);
    println!("  • Longest Streak:    {}", workflow_stats.longest_streak);

    println!("\n╔════════════════════════════════════════════════════════════╗");
    println!("║                     Job Analysis Summary                     ║");
    println!("╚════════════════════════════════════════════════════════════╝");
    println!("\n  Overall Statistics:");
    println!("  ───────────────────");
    println!("  • Total Jobs:        {}", sorted_jobs.len());
    println!("  • Total Job Runs:    {}", total_runs);
    println!("  • Total Failures:    {}", total_failures);
    println!(
        "  • Overall Rate:      {:.1}%",
        overall_failure_rate * 100.0
    );
    println!(
        "  • Failed Jobs:       {}/{} ({:.1}%)",
        jobs_with_failures,
        sorted_jobs.len(),
        percent_jobs_failed
    );
    println!(
        "  • Above {:.1}% Rate:  {}/{} ({:.1}%)",
        min_failure_rate * 100.0,
        jobs_above_threshold,
        sorted_jobs.len(),
        percent_above_threshold
    );
    println!("  • Avg Duration:      {} minutes", avg_job_duration);
    println!("  • Longest Streak:    {}", max_streak);
    println!("  • Currently Failing: {}", currently_failing);

    println!(
        "\n  Detailed Job Analysis (min failure rate: {:.1}%):",
        min_failure_rate * 100.0
    );
    println!("  ────────────────────────────────────────────────");

    for (job_name, stats) in &sorted_jobs {
        if stats.failure_rate >= min_failure_rate {
            println!("\n  Job: {}", job_name);
            println!("  ───────────────────");
            println!("  • Total Runs:     {}", stats.total_runs);
            println!("  • Failures:       {}", stats.failures);
            println!("  • Failure Rate:   {:.1}%", stats.failure_rate * 100.0);
            if let Some(last_failure) = stats.last_failure {
                println!("  • Last Failure:   {}", last_failure);
            }
            if let Some(avg_duration) = stats.avg_duration {
                println!("  • Avg Duration:   {} minutes", avg_duration.num_minutes());
            }
            if let Some(max_duration) = stats.max_duration {
                println!("  • Max Duration:   {} minutes", max_duration.num_minutes());
            }
            println!("  • Current Streak: {}", stats.current_streak);
            println!("  • Longest Streak: {}", stats.longest_streak);
            println!("  • Last Status:    {}", stats.last_status);
        }
    }

    Ok(())
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
        Commands::List { limit } => {
            list_workflows(&octocrab, &config, limit.unwrap_or_else(default_limit)).await?;
        }
        Commands::Describe { limit, workflow } => {
            describe_workflow_runs(
                &octocrab,
                &config,
                limit.unwrap_or_else(default_limit),
                workflow,
            )
            .await?;
        }
        Commands::Analyze {
            limit,
            workflow,
            min_failure_rate,
            branch,
        } => {
            analyze_workflow_runs(
                &octocrab,
                &config,
                limit.unwrap_or(100),
                workflow,
                min_failure_rate.unwrap_or(0.1),
                branch,
            )
            .await?;
        }
    }

    Ok(())
}
