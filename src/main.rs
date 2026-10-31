mod config;
mod init;
mod output;
mod runner;
mod tail;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use output::Output;
use runner::{Options, Status};
use serde_json::json;
use std::path::PathBuf;
use std::process::ExitCode;
use tokio::sync::watch;

#[derive(Parser)]
#[command(
    name = "goodci",
    about = "Run project checks with dependency-aware parallelism",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Config file (default: goodci.toml or .goodci.toml)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    /// Maximum concurrent jobs (0 uses logical CPU count)
    #[arg(short, long, global = true)]
    parallelism: Option<usize>,
    /// Stop launching jobs after a failure; running jobs finish
    #[arg(long, global = true, conflicts_with = "no_fail_fast")]
    fail_fast: bool,
    /// Continue unrelated jobs after failures
    #[arg(long, global = true)]
    no_fail_fast: bool,
    /// Run selected jobs and their transitive dependencies
    #[arg(long, global = true, value_delimiter = ',')]
    jobs: Vec<String>,
    /// Emit versioned JSON Lines events to stdout
    #[arg(long, global = true)]
    json: bool,
    /// Save raw per-job stdout/stderr files in this directory
    #[arg(long, global = true)]
    log_dir: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run checks (default)
    Run,
    /// Validate config and selections without executing commands
    Validate,
    /// List jobs and dependencies
    List,
    /// Generate a project config
    Init {
        /// Override project detection
        #[arg(long, value_enum)]
        template: Option<init::Template>,
        /// Overwrite an existing config without prompting
        #[arg(long)]
        force: bool,
    },
}

#[tokio::main]
async fn main() -> ExitCode {
    let raw_args: Vec<_> = std::env::args_os().collect();
    let json_requested = raw_args.iter().any(|arg| arg == "--json");
    let cli = match Cli::try_parse_from(raw_args) {
        Ok(cli) => cli,
        Err(error) => {
            if matches!(
                error.kind(),
                clap::error::ErrorKind::DisplayHelp | clap::error::ErrorKind::DisplayVersion
            ) {
                let _ = error.print();
                return ExitCode::SUCCESS;
            }
            let (_sender, cancel) = watch::channel(None);
            if let Ok(output) = Output::new(json_requested, cancel) {
                let _ = output
                    .emit(json!({"type":"error", "message":error.to_string()}))
                    .await;
            }
            return ExitCode::from(2);
        }
    };
    let (_signal_sender, cancel) = match if matches!(cli.command, None | Some(Commands::Run)) {
        cancellation()
    } else {
        // Keep the OS default signal behavior for commands that do not spawn jobs,
        // including init's synchronous interactive confirmation.
        Ok(watch::channel(None))
    } {
        Ok(signals) => signals,
        Err(error) => {
            eprintln!("failed to register signal handlers: {error}");
            return ExitCode::FAILURE;
        }
    };
    let output = match Output::new(json_requested, cancel.clone()) {
        Ok(output) => output,
        Err(error) => {
            eprintln!("failed to initialize output: {error}");
            return ExitCode::FAILURE;
        }
    };
    match run(&cli, &output, cancel.clone()).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = output
                .emit(json!({"type":"error", "message":format!("{error:#}")}))
                .await;
            ExitCode::from(cancel.borrow().map(|s| (128 + s) as u8).unwrap_or(1))
        }
    }
}

async fn run(cli: &Cli, output: &Output, cancel: watch::Receiver<Option<i32>>) -> Result<u8> {
    if let Some(Commands::Init { template, force }) = &cli.command {
        init::generate(cli.config.as_deref(), *template, *force, output).await?;
        return Ok(0);
    }
    let (mut config, _) = Config::load(cli.config.as_deref())?;
    let total_defined_jobs = config.jobs.len();
    config.select(&cli.jobs)?;
    let settings = config.settings.resolve(
        cli.parallelism,
        if cli.fail_fast {
            Some(true)
        } else if cli.no_fail_fast {
            Some(false)
        } else {
            None
        },
    )?;
    match cli.command.as_ref().unwrap_or(&Commands::Run) {
        Commands::Validate => {
            output
                .emit(json!({"type":"config_valid", "jobs":config.jobs.len(), "total_defined_jobs":total_defined_jobs, "requested_jobs":cli.jobs, "selected_jobs":config.jobs.iter().map(|j| &j.name).collect::<Vec<_>>()}))
                .await?;
            return Ok(0);
        }
        Commands::List => {
            output
                .emit(json!({"type":"jobs", "jobs":config.jobs, "settings":settings}))
                .await?;
            return Ok(0);
        }
        _ => {}
    }
    let parallelism = settings.parallelism;
    let fail_fast = settings.fail_fast;
    let options = Options {
        parallelism,
        fail_fast,
        workdir: settings.workdir,
        log_dir: cli.log_dir.clone(),
        output: output.clone(),
    };
    output.emit(json!({"type":"run_started", "parallelism":parallelism, "fail_fast":fail_fast, "jobs":config.jobs.len(), "workdir":options.workdir})).await?;
    let results = runner::execute(&config.jobs, &options, cancel.clone()).await?;
    let count = |status| results.iter().filter(|r| r.status == status).count();
    let success = results.iter().all(|r| r.status == Status::Passed);
    output.emit(json!({"type":"summary", "success":success, "passed":count(Status::Passed), "failed":count(Status::Failed), "timed_out":count(Status::TimedOut), "skipped":count(Status::Skipped), "cancelled":count(Status::Cancelled), "results":results})).await?;
    let signal = *cancel.borrow();
    Ok(signal
        .map(|s| (128 + s) as u8)
        .unwrap_or(if success { 0 } else { 1 }))
}

type Cancellation = (watch::Sender<Option<i32>>, watch::Receiver<Option<i32>>);

fn cancellation() -> Result<Cancellation> {
    let (sender, receiver) = watch::channel(None);
    let notify = sender.clone();
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        tokio::spawn(async move {
            let number = tokio::select! {
                _ = interrupt.recv() => libc::SIGINT,
                _ = terminate.recv() => libc::SIGTERM,
            };
            notify.send_replace(Some(number));
        });
    }
    #[cfg(not(unix))]
    tokio::spawn(async move {
        let _ = tokio::signal::ctrl_c().await;
        notify.send_replace(Some(2));
    });
    Ok((sender, receiver))
}
