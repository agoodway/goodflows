use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use colored::*;
use serde::Deserialize;
use std::collections::{HashMap, HashSet, VecDeque};
use std::io::{self, Write};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::time::timeout;

#[derive(Parser)]
#[command(name = "goodci", about = "Mini CI for Ampcode orbs", version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Path to config file (default: goodci.toml or .goodci.toml)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,

    /// Override max parallelism from config
    #[arg(short, long, global = true)]
    parallelism: Option<usize>,

    /// Stop on first failure (overrides config)
    #[arg(long, global = true)]
    fail_fast: bool,

    /// Continue on failure (overrides config)
    #[arg(long, global = true)]
    no_fail_fast: bool,

    /// Only run these jobs (comma-separated names)
    #[arg(long, global = true, value_delimiter = ',')]
    jobs: Vec<String>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run the CI jobs (default)
    Run,
    /// List jobs defined in the config
    List,
    /// Write an example goodci.toml
    Init,
}

#[derive(Debug, Deserialize, Clone)]
struct Config {
    #[serde(default)]
    settings: Settings,
    #[serde(default)]
    jobs: Vec<Job>,
}

#[derive(Debug, Deserialize, Clone, Default)]
struct Settings {
    /// Maximum concurrent jobs. 0 or omitted = number of logical CPUs.
    #[serde(default)]
    parallelism: Option<usize>,
    /// Stop the whole run on the first failed job.
    #[serde(default = "default_fail_fast")]
    fail_fast: bool,
    /// Working directory for all jobs (relative to config file location).
    workdir: Option<PathBuf>,
}

fn default_fail_fast() -> bool {
    true
}

#[derive(Debug, Deserialize, Clone)]
struct Job {
    name: String,
    command: String,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    timeout_secs: Option<u64>,
    #[serde(default)]
    env: HashMap<String, String>,
    /// If true, the command is passed to `sh -c`. Default true.
    #[serde(default = "default_shell")]
    shell: bool,
}

fn default_shell() -> bool {
    true
}

#[derive(Clone)]
struct JobResult {
    name: String,
    success: bool,
    duration: Duration,
    exit_code: Option<i32>,
    error: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command.as_ref().unwrap_or(&Commands::Run) {
        Commands::Init => {
            if std::path::Path::new("goodci.toml").try_exists()? {
                print!("goodci.toml already exists. Overwrite? [y/N] ");
                io::stdout()
                    .flush()
                    .context("failed to display overwrite prompt")?;
                let mut answer = String::new();
                io::stdin()
                    .read_line(&mut answer)
                    .context("failed to read overwrite confirmation")?;
                if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
                    println!("Kept existing goodci.toml");
                    return Ok(());
                }
            }
            write_example_config()?;
            println!("{}", "Wrote goodci.toml".green().bold());
            Ok(())
        }
        Commands::List => {
            let (config, _path) = load_config(cli.config.as_deref())?;
            list_jobs(&config);
            Ok(())
        }
        Commands::Run => {
            let (config, config_path) = load_config(cli.config.as_deref())?;
            run(config, config_path, &cli).await
        }
    }
}

fn write_example_config() -> Result<()> {
    let example = r#"# goodci.toml — Mini CI for Ampcode orbs
# Tunable parallelism so you can match the orb size
# (a1.tiny=1, a1.small=2, a1.medium=4, a1.large=8, a1.xxlarge=16)

[settings]
# Max concurrent jobs. Omit or set to 0 to use the number of logical CPUs.
parallelism = 4
# Stop everything on the first failure
fail_fast = true
# Optional working directory (relative to this file)
# workdir = "."

[[jobs]]
name = "fmt"
command = "cargo fmt --check"

[[jobs]]
name = "clippy"
command = "cargo clippy --all-targets -- -D warnings"
depends_on = ["fmt"]

[[jobs]]
name = "test"
command = "cargo test --all"
depends_on = ["fmt"]
# timeout_secs = 300

[[jobs]]
name = "doc"
command = "cargo doc --no-deps"
depends_on = ["fmt"]

# Example with environment variables
# [[jobs]]
# name = "integration"
# command = "cargo test --test integration"
# depends_on = ["test"]
# env = { RUST_LOG = "info", DATABASE_URL = "postgres://localhost/test" }
"#;
    std::fs::write("goodci.toml", example).context("failed to write goodci.toml")?;
    Ok(())
}

fn load_config(explicit: Option<&std::path::Path>) -> Result<(Config, PathBuf)> {
    let path = if let Some(p) = explicit {
        p.to_path_buf()
    } else {
        let candidates = ["goodci.toml", ".goodci.toml"];
        candidates
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .context("no config found (looked for goodci.toml and .goodci.toml)")?
    };

    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let config: Config =
        toml::from_str(&content).with_context(|| format!("failed to parse {}", path.display()))?;
    Ok((config, path))
}

fn list_jobs(config: &Config) {
    println!("{}", "Jobs".bold());
    for job in &config.jobs {
        let deps = if job.depends_on.is_empty() {
            "none".dimmed().to_string()
        } else {
            job.depends_on.join(", ")
        };
        println!("  {}  depends_on: {}", job.name.cyan().bold(), deps);
        println!("    {}", job.command.dimmed());
    }
}

async fn run(mut config: Config, config_path: PathBuf, cli: &Cli) -> Result<()> {
    if let Some(p) = cli.parallelism {
        config.settings.parallelism = Some(p);
    }
    if cli.fail_fast {
        config.settings.fail_fast = true;
    }
    if cli.no_fail_fast {
        config.settings.fail_fast = false;
    }

    let workdir = config.settings.workdir.clone().unwrap_or_else(|| {
        config_path
            .parent()
            .unwrap_or(std::path::Path::new("."))
            .to_path_buf()
    });

    let parallelism = match config.settings.parallelism {
        Some(0) | None => std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1),
        Some(n) => n.max(1),
    };

    let jobs: Vec<Job> = if cli.jobs.is_empty() {
        config.jobs
    } else {
        let wanted: HashSet<_> = cli.jobs.iter().cloned().collect();
        config
            .jobs
            .into_iter()
            .filter(|j| wanted.contains(&j.name))
            .collect()
    };

    if jobs.is_empty() {
        anyhow::bail!("no jobs to run");
    }

    let names: HashSet<_> = jobs.iter().map(|j| j.name.as_str()).collect();
    for job in &jobs {
        for dep in &job.depends_on {
            if !names.contains(dep.as_str()) {
                anyhow::bail!("job '{}' depends on unknown job '{}'", job.name, dep);
            }
        }
    }

    println!(
        "{}  parallelism={}  fail_fast={}  jobs={}",
        "goodci".bold().cyan(),
        parallelism.to_string().yellow(),
        config.settings.fail_fast.to_string().yellow(),
        jobs.len().to_string().yellow()
    );
    println!("workdir: {}", workdir.display().to_string().dimmed());
    println!();

    let results = execute_jobs(&jobs, parallelism, config.settings.fail_fast, &workdir).await?;

    println!();
    println!("{}", "Summary".bold().underline());
    let mut failed = 0;
    for r in &results {
        let status = if r.success {
            "ok".green().bold()
        } else {
            failed += 1;
            "FAIL".red().bold()
        };
        let code = r
            .exit_code
            .map(|c| format!(" exit={c}"))
            .unwrap_or_default();
        println!(
            "  {:<20} {}  {}{}",
            r.name.cyan(),
            status,
            format_duration(r.duration).dimmed(),
            code.dimmed()
        );
        if let Some(err) = &r.error {
            println!("    {}", err.red());
        }
    }

    println!();
    if failed == 0 {
        println!("{}", "All jobs passed".green().bold());
        Ok(())
    } else {
        println!("{} job(s) failed", failed.to_string().red().bold());
        std::process::exit(1);
    }
}

/// Run jobs in topological levels, with a concurrency limit inside each level.
async fn execute_jobs(
    jobs: &[Job],
    parallelism: usize,
    fail_fast: bool,
    workdir: &std::path::Path,
) -> Result<Vec<JobResult>> {
    let n = jobs.len();
    let mut name_to_idx: HashMap<&str, usize> = HashMap::new();
    for (i, job) in jobs.iter().enumerate() {
        name_to_idx.insert(&job.name, i);
    }

    // Kahn's algorithm to compute levels
    let mut indegree = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![vec![]; n];
    for (i, job) in jobs.iter().enumerate() {
        for dep in &job.depends_on {
            let di = name_to_idx[dep.as_str()];
            adj[di].push(i);
            indegree[i] += 1;
        }
    }

    let mut levels: Vec<Vec<usize>> = Vec::new();
    let mut queue: VecDeque<usize> = indegree
        .iter()
        .enumerate()
        .filter(|(_, &d)| d == 0)
        .map(|(i, _)| i)
        .collect();

    while !queue.is_empty() {
        let mut level = Vec::new();
        let level_size = queue.len();
        for _ in 0..level_size {
            let u = queue.pop_front().unwrap();
            level.push(u);
            for &v in &adj[u] {
                indegree[v] -= 1;
                if indegree[v] == 0 {
                    queue.push_back(v);
                }
            }
        }
        if !level.is_empty() {
            levels.push(level);
        }
    }

    // Detect cycles
    if levels.iter().map(|l| l.len()).sum::<usize>() != n {
        anyhow::bail!("dependency cycle detected");
    }

    let mut all_results = vec![None; n];
    let mut any_failed = false;

    for level in levels {
        if fail_fast && any_failed {
            for &idx in &level {
                all_results[idx] = Some(JobResult {
                    name: jobs[idx].name.clone(),
                    success: false,
                    duration: Duration::ZERO,
                    exit_code: None,
                    error: Some("skipped (fail_fast)".into()),
                });
            }
            continue;
        }

        let sem = Arc::new(Semaphore::new(parallelism));
        let mut handles = Vec::new();

        for &idx in &level {
            let sem = sem.clone();
            let job = jobs[idx].clone();
            let workdir = workdir.to_path_buf();

            handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();
                let start = Instant::now();
                println!("{} {}", "→".blue().bold(), job.name.cyan().bold());

                let result = run_job(&job, &workdir).await;
                let duration = start.elapsed();

                let success = result.is_ok();
                let jr = JobResult {
                    name: job.name.clone(),
                    success,
                    duration,
                    exit_code: result.as_ref().ok().and_then(|r| r.1),
                    error: result.as_ref().err().map(|e| e.to_string()),
                };

                if success {
                    println!(
                        "{} {}  {}",
                        "✓".green().bold(),
                        job.name.cyan(),
                        format_duration(duration).dimmed()
                    );
                } else {
                    println!(
                        "{} {}  {}",
                        "✗".red().bold(),
                        job.name.cyan(),
                        format_duration(duration).dimmed()
                    );
                }

                (idx, jr)
            }));
        }

        for handle in handles {
            let (idx, jr) = handle.await?;
            if !jr.success {
                any_failed = true;
            }
            all_results[idx] = Some(jr);
        }
    }

    Ok(all_results.into_iter().flatten().collect())
}

async fn run_job(job: &Job, workdir: &std::path::Path) -> Result<(bool, Option<i32>)> {
    let mut cmd = if job.shell {
        let mut c = Command::new("sh");
        c.arg("-c").arg(&job.command);
        c
    } else {
        let mut parts = job.command.split_whitespace();
        let prog = parts.next().unwrap_or("true");
        let mut c = Command::new(prog);
        c.args(parts);
        c
    };

    cmd.current_dir(workdir)
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    for (k, v) in &job.env {
        cmd.env(k, v);
    }

    let mut child = cmd
        .spawn()
        .with_context(|| format!("failed to spawn '{}'", job.name))?;

    let fut = child.wait();
    let status = if let Some(secs) = job.timeout_secs {
        match timeout(Duration::from_secs(secs), fut).await {
            Ok(res) => res?,
            Err(_) => {
                let _ = child.kill().await;
                anyhow::bail!("timed out after {}s", secs);
            }
        }
    } else {
        fut.await?
    };

    let code = status.code();
    if status.success() {
        Ok((true, code))
    } else {
        Err(anyhow::anyhow!(
            "exited with {}",
            code.map(|c| c.to_string())
                .unwrap_or_else(|| "signal".into())
        ))
    }
}

fn format_duration(d: Duration) -> String {
    let secs = d.as_secs_f64();
    if secs < 1.0 {
        format!("{:.0}ms", d.as_millis())
    } else if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        format!("{}m{:.0}s", d.as_secs() / 60, secs % 60.0)
    }
}
