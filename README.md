# GitHub Flake Detector

A CLI tool for analyzing GitHub Actions workflow failures and identifying flaky
jobs.

Written mostly by AI, provided in case it's useful.

## Installation

### From Source

Requires [cargo and rust](https://doc.rust-lang.org/cargo/getting-started/installation.html).

```bash
git clone <repository-url>
cd gh-flake-detector
cargo build --release
```

The binary will be available at `target/release/gh-flake-detector`.

### From github

```bash
cargo install --git https://github.com/quodlibetor/gh-flake-detector
```

## Configuration

> ⚠️ GitHub Authentication
>
> For best performance and to avoid rate limiting, it's recommended to use a
> GitHub personal access token.

All cli options can also be specified in a `config.toml` file as snake_case or
as env vars with the options in SCREAMING_SNAKE_CASE prefixed with `GHFD_`.

Examples of setting the github_token option in particular:

- In a `config.toml` file in your project directory

  ```toml
  github_token = "ghp_your_token_here"
  ```

- Environment Variable

  ```bash
  export GHFD_GITHUB_TOKEN="ghp_your_token_here"
  ```

- Command Line (not recommended for the github token since it's a secret)

  ```bash
  gh-flake-detector analyze --owner your-org --repo your-repo --github-token ghp_your_token_here
  ```

## Usage

### Workflow Discovery

Before analyzing workflows, you need to discover what workflows are available in your repository:

```bash
# List all workflows (default limit is 10)
gh-flake-detector list --owner your-org --repo your-repo

# List more workflows
gh-flake-detector list --owner your-org --repo your-repo --limit 50
```


### Workflow Analysis

Analyze workflow failure patterns with comprehensive statistics:

```bash
# Analyze specific workflow (workflow is required, default: 100 runs)
gh-flake-detector analyze --owner your-org --repo your-repo --workflow ID_DISCOVERED_BY_LIST
```

### Workflow Description

Get a summary of recent workflow runs:

```bash
gh-flake-detector describe --owner your-org --repo your-repo --workflow ID_DISCOVERED_BY_LIST
```

### Output format

One of json, markdown, or text can be provided to the `--format` option.

When using the `--output` flag, gh-flake-detector will infer the
output format from the file extension if no explicit `--format` flag is
provided:

- `.json` → JSON format
- `.md` or `.markdown` → Markdown format
- `.txt` or `.text` → Text format

## Example Workflow

Here's a typical workflow for analyzing a repository:

1. **Discover workflows**:
   ```bash
   gh-flake-detector list --owner myorg --repo myproject
   ```

3. **Analyze failure patterns**:
   ```bash
   gh-flake-detector analyze --owner myorg --repo myproject --workflow .github/workflows/ci.yml --format markdown --output ci_analysis.md
   ```
## Output Example


> # GitHub Workflow Analysis
>
> ## Analysis Parameters
> - **Workflow Runs Analyzed**: 100
> - **Workflow**: .github/workflows/ci.yml
> - **Branch**: main
>
> ## Job Analysis
>
> ### Detailed Job Statistics
>
> | Job Name | Total Runs | Failures | Failure Rate | Avg Duration | Current Failure Streak | Longest Failure Streak | Last Status |
> |----------|------------|----------|--------------|--------------|----------------------|----------------------|-------------|
> | test-integration | 50 | 8 | 16.0% | 12m | 0 | 3 | Success |
> | build | 50 | 2 | 4.0% | 8m | 0 | 1 | Success |
> | lint | 50 | 1 | 2.0% | 3m | 0 | 1 | Success |
>
> ## Job Summary
>
> ### Key Metrics
> - **Total Jobs**: 3
> - **Total Job Runs**: 150
> - **Total Failures**: 11
> - **Overall Failure Rate**: 7.3%
>
> ### Job Categories
> - **Jobs with Failures**: 3/3 (100.0%)
> - **Jobs Never Failed**: 0/3 (0.0%)
>
> ### Performance Metrics
> - **Average Job Duration**: 8 minutes
> - **Longest Failure Streak**: 3
> - **Currently Failing Jobs**: 0
>
> ## Workflow Summary
>
> ### Run Statistics
> - **Total Runs**: 50
> - **Skipped Runs**: 0
> - **Failed Runs**: 4
> - **Failure Rate**: 8.0%
>
> ### Duration Statistics
> - **Average Duration**: 23 minutes
> - **Minimum Duration**: 18 minutes
> - **Maximum Duration**: 35 minutes
>
> ### Failure Patterns
> - **Current Failure Streak**: 0
> - **Longest Failure Streak**: 2
> - **Consecutive Failures**: 0

## Tips

1. **Use GitHub tokens**: Always use a personal access token to avoid rate limiting
2. **Start with list**: Use the `list` command to discover available workflows first
3. **Filter appropriately**: Use failure rate filters to focus on problematic jobs

## Contributing

Contributions are welcome! Please feel free to submit a Pull Request.

## License

License is granted to use this software under the terms of the Apache License,
2.0 or MIT license, at your option.
