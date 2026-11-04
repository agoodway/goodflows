mod config;
mod expression;
mod fsutil;
mod history;
mod init;
mod output;
mod reporting;
mod retention;
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
    name = "goodflows",
    about = "Run project checks with dependency-aware parallelism",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Config file (default: goodflows.toml or .goodflows.toml)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    /// Apply a named environment's settings and partition counts (no automatic selection)
    #[arg(long, global = true, value_name = "NAME")]
    env: Option<String>,
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
    /// Save raw per-job stdout/stderr files in a new per-run directory under this directory
    #[arg(long, global = true)]
    log_dir: Option<PathBuf>,
    /// Write a run history file for this run
    #[arg(long, global = true, conflicts_with = "no_history")]
    history: bool,
    /// Do not write a run history file, even when settings enable it
    #[arg(long, global = true)]
    no_history: bool,
    /// Write the run history file under this directory (enables history)
    #[arg(long, global = true, value_name = "DIR")]
    history_dir: Option<PathBuf>,
    /// Log run directories to keep per workflow (default 10)
    #[arg(long, global = true, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    log_runs: Option<u64>,
    /// History files to keep per workflow (default 1000)
    #[arg(long, global = true, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    history_runs: Option<u64>,
    /// History bytes to keep per workflow (default 10485760)
    #[arg(long, global = true, value_name = "BYTES", value_parser = clap::value_parser!(u64).range(1..))]
    history_bytes: Option<u64>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run checks (default)
    Run { workflow: Option<String> },
    /// Validate config and selections without executing commands
    Validate { workflow: Option<String> },
    /// List jobs and dependencies
    List {
        #[arg(conflicts_with = "workflows")]
        workflow: Option<String>,
        /// List available workflows rather than jobs
        #[arg(long, conflicts_with = "jobs")]
        workflows: bool,
    },
    /// Generate a project config
    Init {
        /// Override project detection
        #[arg(long, value_enum)]
        template: Option<init::Template>,
        /// Overwrite an existing config without prompting
        #[arg(long)]
        force: bool,
        /// Generate jobs inside this named workflow
        #[arg(long)]
        workflow: Option<String>,
    },
    /// Manage saved test timing reports
    Reports {
        #[command(subcommand)]
        command: ReportsCommand,
    },
}

#[derive(Subcommand)]
enum ReportsCommand {
    /// Retry publishing a saved invocation's finalized reports without running jobs
    Upload {
        /// Run directory under the reporting spool (contains manifest.json)
        run_directory: PathBuf,
        /// Total publishing budget in seconds (default 30), for large reports on slow links
        #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..=3600))]
        budget: Option<u64>,
    },
}

impl Cli {
    /// History and retention flags. `--history-dir` resolves from the
    /// invocation directory, like `--log-dir`.
    fn history_overrides(&self) -> Result<config::HistoryOverrides> {
        let history_dir = match &self.history_dir {
            Some(dir) => Some(std::env::current_dir()?.join(dir)),
            None => None,
        };
        Ok(config::HistoryOverrides {
            history: if self.no_history {
                Some(false)
            } else if self.history || history_dir.is_some() {
                Some(true)
            } else {
                None
            },
            history_dir,
            log_runs: self.log_runs,
            history_runs: self.history_runs,
            history_bytes: self.history_bytes,
        })
    }

    fn workflow(&self) -> Option<&str> {
        match &self.command {
            Some(
                Commands::Run { workflow }
                | Commands::List { workflow, .. }
                | Commands::Validate { workflow }
                | Commands::Init { workflow, .. },
            ) => workflow.as_deref(),
            Some(Commands::Reports { .. }) | None => None,
        }
    }
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
    let (_signal_sender, cancel) = match if matches!(
        cli.command,
        None | Some(Commands::Run { .. } | Commands::Reports { .. })
    ) {
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
    let output = match Output::new(cli.json, cancel.clone()) {
        Ok(output) => output,
        Err(error) => {
            eprintln!("failed to initialize output: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mut output = if let Some(name) = cli.workflow() {
        output.with_workflow(name)
    } else {
        output
    };
    if let Some(name) = &cli.env {
        output = output.with_environment(name);
    }
    match run(&cli, &mut output, cancel.clone()).await {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            let _ = output
                .emit(json!({"type":"error", "message":format!("{error:#}")}))
                .await;
            ExitCode::from(cancel.borrow().map(|s| (128 + s) as u8).unwrap_or(1))
        }
    }
}

async fn run(cli: &Cli, output: &mut Output, cancel: watch::Receiver<Option<i32>>) -> Result<u8> {
    if let Some(Commands::Init {
        template,
        force,
        workflow,
    }) = &cli.command
    {
        if cli.env.is_some() {
            anyhow::bail!(
                "--env cannot be used with init; define environment profiles in the config"
            );
        }
        init::generate(
            cli.config.as_deref(),
            *template,
            *force,
            workflow.as_deref(),
            output,
        )
        .await?;
        return Ok(0);
    }
    if let Some(Commands::Reports {
        command: ReportsCommand::Upload {
            run_directory,
            budget,
        },
    }) = &cli.command
    {
        for (set, flag) in [
            (cli.config.is_some(), "--config"),
            (cli.env.is_some(), "--env"),
            (cli.parallelism.is_some(), "--parallelism"),
            (cli.fail_fast, "--fail-fast"),
            (cli.no_fail_fast, "--no-fail-fast"),
            (!cli.jobs.is_empty(), "--jobs"),
            (cli.log_dir.is_some(), "--log-dir"),
            (cli.history, "--history"),
            (cli.no_history, "--no-history"),
            (cli.history_dir.is_some(), "--history-dir"),
            (cli.log_runs.is_some(), "--log-runs"),
            (cli.history_runs.is_some(), "--history-runs"),
            (cli.history_bytes.is_some(), "--history-bytes"),
        ] {
            if set {
                anyhow::bail!("{flag} cannot be used with reports upload");
            }
        }
        let budget = budget.map(std::time::Duration::from_secs);
        return reporting::replay::upload(run_directory, output, cancel, budget).await;
    }
    let (document, _) = Config::load(cli.config.as_deref())?;
    let history_cli = cli.history_overrides()?;
    let profile = cli.env.as_deref();
    // Report an unknown --env before the --jobs check for all-workflow commands;
    // workflow() repeats this lookup.
    document.environment(profile)?;
    let fail_fast_override = if cli.fail_fast {
        Some(true)
    } else if cli.no_fail_fast {
        Some(false)
    } else {
        None
    };
    let aggregate_validation = matches!(cli.command, Some(Commands::Validate { workflow: None }))
        && !document.workflows.is_empty();
    let list_workflows = matches!(
        cli.command,
        Some(Commands::List {
            workflows: true,
            ..
        })
    );
    if aggregate_validation || list_workflows {
        if !cli.jobs.is_empty() {
            anyhow::bail!("--jobs requires an explicit workflow for aggregate validation");
        }
        if aggregate_validation && profile.is_none() {
            // Without --env, check that every profile resolves for every
            // workflow, so `validate` passes only if each `run <workflow>
            // --env <profile>` would get past configuration.
            for environment in document.environments.keys() {
                for name in document.workflow_names() {
                    document.workflow(Some(&name), Some(environment))?;
                }
            }
        }
        let mut workflows = Vec::new();
        let mut total = 0;
        for name in document.workflow_names() {
            let (_, config) = document.workflow(Some(&name), profile)?;
            total += config.jobs.len();
            workflows.push(json!({"name":name, "jobs":config.jobs.len(), "selected_jobs":config.jobs.iter().map(|j| &j.name).collect::<Vec<_>>(), "requested_jobs":[], "total_defined_jobs":config.jobs.len(), "settings":config.settings.resolve(cli.parallelism, fail_fast_override, &history_cli)?}));
        }
        output.emit(if list_workflows {
            json!({"type":"workflows", "workflows":workflows, "default_workflow":document.default_workflow()})
        } else {
            json!({"type":"config_valid", "workflows":workflows, "jobs":total, "total_defined_jobs":total, "requested_jobs":[], "selected_jobs":[]})
        }).await?;
        return Ok(0);
    }
    let legacy_default = !document.jobs.is_empty();
    let (name, mut config) = document.workflow(cli.workflow(), profile)?;
    *output = output.with_workflow(&name);
    let total_defined_jobs = config.jobs.len();
    config.select(&cli.jobs)?;
    let settings = config
        .settings
        .resolve(cli.parallelism, fail_fast_override, &history_cli)?;
    match cli.command.as_ref() {
        Some(Commands::Validate { .. }) => {
            output
                .emit(json!({"type":"config_valid", "jobs":config.jobs.len(), "total_defined_jobs":total_defined_jobs, "requested_jobs":cli.jobs, "selected_jobs":config.jobs.iter().map(|j| &j.name).collect::<Vec<_>>(), "settings":settings}))
                .await?;
            return Ok(0);
        }
        Some(Commands::List { .. }) => {
            let mut event = json!({"type":"jobs", "jobs":config.jobs, "settings":settings});
            if let Some(reporting) = &config.reporting {
                // Only nonsecret configuration: env variable names, never their values.
                event["reporting"] = json!(reporting);
            }
            output.emit(event).await?;
            return Ok(0);
        }
        _ => {}
    }
    let parallelism = settings.parallelism;
    let fail_fast = settings.fail_fast;
    // One identity names the log run directory, the history file and the
    // reporting workflow_run_id. The host is sampled at run start too.
    let identity = retention::RunIdentity::new();
    let host = history::Host::current();
    let graph_sha256 = history::graph_sha256(&config.jobs);
    let mut session = match &config.reporting {
        Some(reporting) => Some(
            reporting::Session::start(
                reporting,
                &identity.id,
                &name,
                cli.env.as_deref(),
                &config.jobs,
                &settings.workdir,
            )
            .await,
        ),
        None => None,
    };
    // The commit and dirty state the run started from, for history only.
    // Reuse the reporting session's capture rather than running git twice.
    let source = if settings.history {
        match session.as_ref().and_then(reporting::Session::source) {
            Some(source) => Some(source.clone()),
            None => Some(
                reporting::capture_source(
                    &settings.workdir,
                    config.reporting.as_ref().map(|r| r.token_env.as_str()),
                )
                .await,
            ),
        }
    } else {
        None
    };
    let log_base = cli.log_dir.as_ref().map(|dir| {
        if legacy_default && name == "default" {
            dir.clone()
        } else {
            dir.join(config::path_component(&name))
        }
    });
    let mut options = Options {
        parallelism,
        fail_fast,
        workdir: settings.workdir,
        log_dir: None,
        output: output.clone(),
        reporting: session
            .as_ref()
            .map(reporting::Session::runner_env)
            .unwrap_or_default(),
        environment: cli.env.clone(),
        settings_env: settings.env,
    };
    output.emit(json!({"type":"run_started", "parallelism":parallelism, "fail_fast":fail_fast, "jobs":config.jobs.iter().map(config::Job::instances).sum::<usize>(), "workdir":options.workdir})).await?;
    if let Some(session) = &session {
        session.announce(output).await;
    }
    // Held until this function returns, so a concurrent prune skips this run.
    let _log_run = match &log_base {
        Some(base) => match retention::LogRun::create(base, &identity.stem, settings.log_runs) {
            Ok((log_run, pruned)) => {
                if let Some(error) = pruned {
                    eprintln!("goodflows: warning: {error:#}");
                }
                options.log_dir = Some(log_run.dir.clone());
                Some(log_run)
            }
            Err(error) => {
                if let Some(session) = &mut session {
                    session.abort();
                }
                return Err(error);
            }
        },
        None => None,
    };
    // After log setup and pruning: `duration_ms` runs from just before
    // `runner::execute` until the summary, and `execute_ms` until it returns.
    let run_start = std::time::Instant::now();
    let execution = match runner::execute(&config.jobs, &options, cancel.clone()).await {
        Ok(execution) => execution,
        Err(error) => {
            if let Some(session) = &mut session {
                session.abort();
            }
            return Err(error);
        }
    };
    let execute_ms = run_start.elapsed().as_millis() as u64;
    let results = execution.results;
    // Reporting runs after every job finished and before the summary, which stays last.
    if let Some(session) = &mut session {
        session.finish(&results, cancel.clone(), output).await;
    }
    let count = |status| results.iter().filter(|r| r.status == status).count();
    let success = results
        .iter()
        .all(|r| matches!(r.status, Status::Passed | Status::Skipped));
    // `started` and the optimizer fields are projected only here; live
    // job_finished events never carry them.
    let summary_results: Vec<_> = results.iter().map(runner::JobResult::summary).collect();
    let mut summary = json!({"type":"summary", "success":success, "passed":count(Status::Passed), "failed":count(Status::Failed), "timed_out":count(Status::TimedOut), "skipped":count(Status::Skipped), "cancelled":count(Status::Cancelled), "results":summary_results});
    if let Some(dir) = &options.log_dir {
        // A non-UTF-8 path cannot be a JSON string; show it lossily.
        summary["log_dir"] = json!(dir.to_string_lossy());
    }
    summary["duration_ms"] = json!(run_start.elapsed().as_millis() as u64);
    summary["execute_ms"] = json!(execute_ms);
    // One snapshot decides the outcome in the history file and the summary,
    // the report header and the exit code.
    let signal = *cancel.borrow();
    let fields = history::RunFields {
        run_id: identity.id.clone(),
        started_at: identity.started_at.clone(),
        outcome: history::outcome(&results, signal.is_some()),
        parallelism,
        graph_sha256,
        max_in_flight: execution.max_in_flight,
        host,
    };
    if let serde_json::Value::Object(run_fields) = json!(fields) {
        for (key, value) in run_fields {
            summary[key] = value;
        }
    }
    if settings.history {
        let target = history::HistoryTarget {
            dir: &settings.history_dir,
            // Legacy top-level jobs record under `default`, which is their workflow name.
            workflow: &name,
            environment: cli.env.as_deref(),
            requested_jobs: &cli.jobs,
            source: source.as_ref(),
            stem: &identity.stem,
            runs: settings.history_runs,
            bytes: settings.history_bytes,
            workdir: &options.workdir,
            log_dir: options.log_dir.as_deref(),
        };
        // History covers the jobs only, not reporting uploads after them.
        match history::write(&target, &fields, execute_ms, &results) {
            Ok(written) => {
                // A prune failure still leaves the new file in place.
                if let Some(error) = written.pruned {
                    eprintln!("goodflows: warning: run history: {error:#}");
                }
                summary["history_file"] = json!(written.path.to_string_lossy());
            }
            Err(error) => eprintln!("goodflows: warning: run history: {error:#}"),
        }
    }
    let config_path = cli.config.as_ref().map(|path| path.to_string_lossy());
    output
        .emit_summary(summary, config_path.as_deref(), signal.is_some())
        .await?;
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

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once("goodflows").chain(args.iter().copied()))
    }

    fn overrides(args: &[&str]) -> config::HistoryOverrides {
        parse(args).unwrap().history_overrides().unwrap()
    }

    #[test]
    fn history_flags_parse_into_overrides() {
        let none = overrides(&["run"]);
        assert_eq!(none.history, None);
        assert!(none.history_dir.is_none());
        assert_eq!(overrides(&["--history"]).history, Some(true));
        assert_eq!(overrides(&["--no-history"]).history, Some(false));
        let dir = overrides(&["--history-dir", "runs"]);
        assert_eq!(dir.history, Some(true));
        assert_eq!(
            dir.history_dir,
            Some(std::env::current_dir().unwrap().join("runs"))
        );
        // --no-history beats --history-dir.
        assert_eq!(
            overrides(&["--history-dir", "runs", "--no-history"]).history,
            Some(false)
        );
        let caps = overrides(&[
            "--log-runs",
            "2",
            "--history-runs",
            "3",
            "--history-bytes",
            "4",
        ]);
        assert_eq!(
            (caps.log_runs, caps.history_runs, caps.history_bytes),
            (Some(2), Some(3), Some(4))
        );
    }

    #[test]
    fn history_flags_reject_conflicts_and_zero_caps() {
        let error = parse(&["--history", "--no-history"]).err().unwrap();
        assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
        for flag in ["--log-runs", "--history-runs", "--history-bytes"] {
            let error = parse(&[flag, "0"]).err().unwrap();
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ValueValidation,
                "{flag}"
            );
            assert!(parse(&[flag, "1"]).is_ok(), "{flag}");
        }
        // No upper bound on the byte cap.
        assert!(parse(&["--history-bytes", "18446744073709551615"]).is_ok());
    }
}
