mod config;
mod expression;
mod fsutil;
mod history;
mod init;
mod output;
mod reporting;
mod retention;
mod runner;
mod sprite;
mod tail;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::Config;
use output::Output;
use runner::{Options, Status};
use serde_json::json;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
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
    Run {
        workflow: Option<String>,
        /// Run the whole workflow on this caller identity's owned Sprite (see docs/sprites.md)
        #[arg(long, value_name = "IDENTITY", value_parser = sprite::naming::parse_identity)]
        sprite: Option<String>,
    },
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
    /// Inspect, recover, cancel or destroy a caller identity's Sprite
    Sprite {
        #[command(subcommand)]
        command: sprite::Command,
    },
    /// Internal: the remote runner protocol used on a Sprite
    #[command(name = "__sprite-agent", hide = true)]
    SpriteAgent {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
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

/// Global flags that only `run` uses; `sprite` commands reject them.
const RUN_ONLY_FLAGS: [&str; 12] = [
    "env",
    "parallelism",
    "fail_fast",
    "no_fail_fast",
    "jobs",
    "log_dir",
    "history",
    "no_history",
    "history_dir",
    "log_runs",
    "history_runs",
    "history_bytes",
];

impl Cli {
    /// The parser, with run-only global flags hidden from `sprite` help.
    /// They still parse there, so passing one is rejected with a clear error
    /// rather than silently ignored.
    fn parser() -> clap::Command {
        let mut command = <Self as clap::CommandFactory>::command();
        // Globals reach subcommands only when the command is built.
        command.build();
        let hide = |command: clap::Command| {
            command.mut_args(|arg| {
                let hidden = RUN_ONLY_FLAGS.contains(&arg.get_id().as_str()) || arg.is_hide_set();
                arg.hide(hidden)
            })
        };
        command.mut_subcommand("sprite", |sprite| hide(sprite).mut_subcommands(hide))
    }

    fn parse_from(
        args: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
    ) -> std::result::Result<Self, clap::Error> {
        let matches = Self::parser().try_get_matches_from(args)?;
        <Self as clap::FromArgMatches>::from_arg_matches(&matches)
    }

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
                Commands::Run { workflow, .. }
                | Commands::List { workflow, .. }
                | Commands::Validate { workflow }
                | Commands::Init { workflow, .. },
            ) => workflow.as_deref(),
            Some(
                Commands::Reports { .. } | Commands::Sprite { .. } | Commands::SpriteAgent { .. },
            )
            | None => None,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let raw_args: Vec<_> = std::env::args_os().collect();
    let json_requested = raw_args.iter().any(|arg| arg == "--json");
    let cli = match Cli::parse_from(raw_args) {
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
    if let Some(Commands::SpriteAgent { args }) = &cli.command {
        return ExitCode::from(sprite::agent::main(args).await);
    }
    let (_signal_sender, cancel) = match if matches!(
        cli.command,
        None | Some(Commands::Run { .. } | Commands::Reports { .. } | Commands::Sprite { .. })
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
    if let Some(Commands::Sprite { command }) = &cli.command {
        return sprite::command(cli, command, output, cancel).await;
    }
    let (document, config_file) = Config::load(cli.config.as_deref())?;
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
    // Called workflows run their complete graphs; the selected profile must
    // resolve for each one before anything runs.
    let targets = document.called_workflows(&config.jobs, profile)?;
    let settings = config
        .settings
        .resolve(cli.parallelism, fail_fast_override, &history_cli)?;
    let calls: Vec<_> = config
        .jobs
        .iter()
        .filter_map(|job| Some(json!({"job":job.name, "workflow":job.workflow.as_ref()?})))
        .collect();
    match cli.command.as_ref() {
        Some(Commands::Validate { .. }) => {
            let mut event = json!({"type":"config_valid", "jobs":config.jobs.len(), "total_defined_jobs":total_defined_jobs, "requested_jobs":cli.jobs, "selected_jobs":config.jobs.iter().map(|j| &j.name).collect::<Vec<_>>(), "settings":settings});
            if !calls.is_empty() {
                event["calls"] = json!(calls);
            }
            output.emit(event).await?;
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
    if let Some(Commands::Run {
        sprite: Some(identity),
        ..
    }) = &cli.command
    {
        let request = sprite::control::Request {
            workflow: cli.workflow().map(str::to_owned),
            environment: cli.env.clone(),
            jobs: cli.jobs.clone(),
            parallelism: cli.parallelism,
            fail_fast: fail_fast_override,
            logs: cli.log_dir.is_some(),
            history: history_cli.history,
            log_runs: cli.log_runs,
            history_runs: cli.history_runs,
            history_bytes: cli.history_bytes,
        };
        return sprite::run(
            sprite::RunInput {
                document,
                config_file,
                name,
                selected: config,
                targets,
                identity: identity.clone(),
                request,
                rerun_config: cli
                    .config
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned()),
            },
            output,
            cancel,
        )
        .await;
    }
    let run = Run {
        calls: !calls.is_empty(),
        root: config,
        targets,
        profile: cli.env.clone(),
        parallelism: cli.parallelism,
        fail_fast: fail_fast_override,
        history: history_cli,
        requested_jobs: cli.jobs.clone(),
        log_dir: cli.log_dir.clone(),
        legacy_default,
        config_path: cli
            .config
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        output: output.clone(),
        cancel,
        sprite_state: None,
    };
    execute(run, name, retention::RunIdentity::new()).await
}

/// Run a whole invocation tree: the root workflow `name` with `identity`
/// and every child its calls activate. Local runs and the remote Sprite
/// runner (which preallocates the root identity) share this path. Returns
/// the root's exit code.
pub(crate) async fn execute(
    run: Run,
    name: String,
    identity: retention::RunIdentity,
) -> Result<u8> {
    let (_, finished) = invoke(Arc::new(run), name, None, identity).await;
    let finished = finished?.expect("only a called run stops before it starts");
    Ok(finished
        .signal
        .map(|s| (128 + s) as u8)
        .unwrap_or(if finished.success { 0 } else { 1 }))
}

/// What every invocation of one `goodflows run` shares: the root selection,
/// the workflows its calls reach, and the CLI overrides that apply to each.
pub(crate) struct Run {
    pub root: Config,
    /// Every workflow reachable through calls, with its complete jobs under
    /// the selected profile.
    pub targets: BTreeMap<String, Config>,
    /// The root selection contains a workflow call, so events carry
    /// invocation identities.
    pub calls: bool,
    pub profile: Option<String>,
    pub parallelism: Option<usize>,
    pub fail_fast: Option<bool>,
    pub history: config::HistoryOverrides,
    /// `--jobs`, which selects only in the root workflow.
    pub requested_jobs: Vec<String>,
    pub log_dir: Option<PathBuf>,
    pub legacy_default: bool,
    /// The explicit `--config` path, for the root rerun hint.
    pub config_path: Option<String>,
    pub output: Output,
    pub cancel: watch::Receiver<Option<i32>>,
    /// `GOODFLOWS_SPRITE_STATE_DIR` for every invocation of a remote Sprite
    /// run; `None` locally.
    pub sprite_state: Option<String>,
}

/// The invocation that activated a child, and the call job that did it.
struct Parent {
    run_id: String,
    caller_job: String,
    scope: Arc<runner::Scope>,
}

/// How one invocation ended, after its reporting, history and summary.
struct Finished {
    outcome: &'static str,
    success: bool,
    /// The signal at the end of the invocation, which sets the exit code. It
    /// may have arrived after the outcome snapshot.
    signal: Option<i32>,
    /// The outcome is passed, but a local job or call was skipped by
    /// fail-fast: only an enclosing invocation's failure can do that.
    interrupted: bool,
}

/// Run one invocation, the root (`parent` is `None`) or a called workflow,
/// through its whole lifecycle in this process. Returns its run id, which
/// exists even when setup then failed, and `None` when a called run was
/// stopped by fail-fast before it started. One identity names the log run
/// directory, the history file and the reporting workflow_run_id.
async fn invoke(
    run: Arc<Run>,
    name: String,
    parent: Option<Parent>,
    identity: retention::RunIdentity,
) -> (String, Result<Option<Finished>>) {
    let id = identity.id.clone();
    (id, invocation(&run, &name, parent, identity).await)
}

/// The activation callback for the call jobs of the invocation `run_id`.
fn activator(run: Arc<Run>, run_id: String) -> runner::Activate {
    Arc::new(move |activation: runner::Activation| {
        let parent = Parent {
            run_id: run_id.clone(),
            caller_job: activation.caller_job,
            scope: activation.scope,
        };
        let (run, target) = (run.clone(), activation.target);
        Box::pin(async move {
            let (run_id, finished) = invoke(
                run,
                target.clone(),
                Some(parent),
                retention::RunIdentity::new(),
            )
            .await;
            child_end(&target, run_id, finished)
        })
    })
}

/// Whether an invocation with `outcome`, whose local results carry the given
/// `fail_fast_skip` flags, was cut short by an enclosing fail-fast: it passed
/// by the existing rule, yet a job or call was skipped by fail-fast. Failed
/// and cancelled outcomes take precedence.
fn interrupted(outcome: &str, fail_fast_skips: impl IntoIterator<Item = bool>) -> bool {
    outcome == "passed" && fail_fast_skips.into_iter().any(|skip| skip)
}

/// The call result for a child invocation of `target` that ended with
/// `finished`.
fn child_end(target: &str, run_id: String, finished: Result<Option<Finished>>) -> runner::ChildEnd {
    let (status, error, fail_fast_skip) = match finished {
        // Stopped before its first effect: the same skip as the call's
        // own check, with no child identity.
        Ok(None) => {
            return runner::ChildEnd {
                run_id: None,
                status: Status::Skipped,
                error: Some(runner::FAIL_FAST_SKIP.into()),
                fail_fast_skip: true,
            }
        }
        // The child's outcome, from its own signal snapshot.
        Ok(Some(finished)) => match finished.outcome {
            // Cut short by an enclosing fail-fast: skipped like the work it
            // stopped, though the child's own outcome stays passed.
            "passed" if finished.interrupted => {
                (Status::Skipped, Some(runner::FAIL_FAST_SKIP.into()), true)
            }
            "passed" => (Status::Passed, None, false),
            "failed" => (
                Status::Failed,
                Some(format!("workflow '{target}' failed")),
                false,
            ),
            "cancelled" => (
                Status::Cancelled,
                Some(format!("workflow '{target}' was cancelled")),
                false,
            ),
            other => (
                Status::Failed,
                Some(format!(
                    "workflow '{target}' ended with unknown outcome '{other}'"
                )),
                false,
            ),
        },
        Err(error) => (
            Status::Failed,
            Some(format!("workflow '{target}': {error:#}")),
            false,
        ),
    };
    runner::ChildEnd {
        run_id: Some(run_id),
        status,
        error,
        fail_fast_skip,
    }
}

async fn invocation(
    run: &Arc<Run>,
    name: &str,
    parent: Option<Parent>,
    identity: retention::RunIdentity,
) -> Result<Option<Finished>> {
    let config = match &parent {
        Some(_) => &run.targets[name],
        None => &run.root,
    };
    let settings = config
        .settings
        .resolve(run.parallelism, run.fail_fast, &run.history)?;
    let mut output = run.output.with_workflow(name);
    if run.calls {
        output = output.with_invocation(
            &identity.id,
            parent
                .as_ref()
                .map(|p| (p.run_id.as_str(), p.caller_job.as_str())),
        );
    }
    let output = &output;
    let cancel = run.cancel.clone();
    let profile = run.profile.as_deref();
    let parallelism = settings.parallelism;
    let fail_fast = settings.fail_fast;
    let scope = match &parent {
        Some(parent) => runner::Scope::child(&parent.scope, parallelism, fail_fast),
        None => runner::Scope::root(parallelism, fail_fast),
    };
    // The host is sampled at run start too.
    let host = history::Host::current();
    let (graph_sha256, graph) =
        history::graph_sha256(&config.jobs, &|target| &run.targets[target].jobs);
    // An enclosing fail-fast stop can land after the call's own check; re-check
    // right before the first observable effect (reporting, events, logs).
    if parent.as_ref().is_some_and(|parent| parent.scope.stopped()) {
        return Ok(None);
    }
    let mut session = match &config.reporting {
        Some(reporting) => Some(
            reporting::Session::start(
                reporting,
                &identity.id,
                name,
                profile,
                &config.jobs,
                &settings.workdir,
                run.sprite_state.is_some(),
            )
            .await,
        ),
        None => None,
    };
    // The source the run started from, for the run record and history.
    // Reuse the reporting session's capture rather than running git twice.
    let source = match session.as_ref().and_then(reporting::Session::source) {
        Some(source) => Some(source.clone()),
        None if settings.history => Some(
            reporting::capture_source(
                &settings.workdir,
                config.reporting.as_ref().map(|r| r.token_env.as_str()),
            )
            .await,
        ),
        None => None,
    };
    // Legacy root jobs log flat; every named workflow and every called
    // workflow, `default` included, logs under its own namespace.
    let log_base = run.log_dir.as_ref().map(|dir| {
        if parent.is_none() && run.legacy_default && name == "default" {
            dir.clone()
        } else {
            dir.join(config::path_component(name))
        }
    });
    let mut options = Options {
        scope: scope.clone(),
        calls: run
            .calls
            .then(|| activator(run.clone(), identity.id.clone())),
        workdir: settings.workdir,
        log_dir: None,
        output: output.clone(),
        reporting: session
            .as_ref()
            .map(reporting::Session::runner_env)
            .unwrap_or_default(),
        environment: run.profile.clone(),
        settings_env: settings.env,
        sprite_state: run.sprite_state.clone(),
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
    // One snapshot decides the outcome in the run record, the history file
    // and the summary, the report header and a calling job's status. A
    // signal after it cancels only the run record upload.
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
    let target = history::HistoryTarget {
        dir: &settings.history_dir,
        // Legacy top-level jobs record under `default`, which is their workflow name.
        workflow: name,
        environment: profile,
        // A child always runs its complete workflow.
        requested_jobs: match &parent {
            Some(_) => &[],
            None => &run.requested_jobs,
        },
        parent: parent
            .as_ref()
            .map(|p| (p.run_id.as_str(), p.caller_job.as_str())),
        source: source.as_ref(),
        stem: &identity.stem,
        runs: settings.history_runs,
        bytes: settings.history_bytes,
        workdir: &options.workdir,
        log_dir: options.log_dir.as_deref(),
    };
    // The run record carries the outcome, so it follows the snapshot.
    if let Some(session) = session
        .as_mut()
        .filter(|session| session.uploads_run_record())
    {
        // The caller's own environment, never settings `env`.
        let ci = reporting::ci::capture(|name| std::env::var(name).ok());
        let upload = history::Upload {
            branch: true,
            ci: ci.as_ref(),
            graph: Some(&graph),
        };
        let record = history::record(&target, &fields, execute_ms, &results, upload);
        session
            .publish_run_record(&record, &identity.id, cancel.clone(), output)
            .await;
    }
    if settings.history {
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
    // `duration_ms` covers every reporting upload; `execute_ms` only the jobs.
    summary["duration_ms"] = json!(run_start.elapsed().as_millis() as u64);
    summary["execute_ms"] = json!(execute_ms);
    // Only the root report offers a rerun, which keeps the calling context.
    let config_path = match &parent {
        Some(_) => None,
        None => run.config_path.as_deref(),
    };
    output
        .emit_summary(summary, config_path, None, signal.is_some())
        .await?;
    // A signal during the run record upload leaves the outcome as
    // snapshotted but still decides the exit code.
    let exit_signal = *cancel.borrow();
    Ok(Some(Finished {
        outcome: fields.outcome,
        success,
        signal: exit_signal,
        interrupted: interrupted(fields.outcome, results.iter().map(|r| r.fail_fast_skip)),
    }))
}

type Cancellation = (watch::Sender<Option<i32>>, watch::Receiver<Option<i32>>);

pub(crate) fn cancellation() -> Result<Cancellation> {
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

    #[test]
    fn child_end_maps_how_the_child_invocation_ended() {
        let end = |finished| child_end("suite", "child".into(), finished);
        let ended = |outcome, interrupted| {
            Ok(Some(Finished {
                outcome,
                success: outcome == "passed",
                signal: None,
                interrupted,
            }))
        };
        // Stopped by fail-fast before it started: skipped, with no identity.
        let skipped = end(Ok(None));
        assert_eq!(skipped.status, Status::Skipped);
        assert_eq!(
            skipped.error.as_deref(),
            Some("fail_fast: previous job failed")
        );
        assert!(skipped.run_id.is_none() && skipped.fail_fast_skip);
        // That skip interrupts the invocation that owns the call in turn.
        assert!(interrupted("passed", [skipped.fail_fast_skip]));
        let passed = end(ended("passed", false));
        assert_eq!(
            (passed.status, passed.error, passed.run_id.as_deref()),
            (Status::Passed, None, Some("child"))
        );
        assert!(!passed.fail_fast_skip);
        let failed = end(ended("failed", false));
        assert_eq!(
            (
                failed.status,
                failed.error.as_deref(),
                failed.run_id.as_deref()
            ),
            (
                Status::Failed,
                Some("workflow 'suite' failed"),
                Some("child")
            )
        );
        // Cut short by an enclosing fail-fast: skipped, keeping the identity.
        let cut = end(ended("passed", true));
        assert_eq!(
            (cut.status, cut.error.as_deref(), cut.run_id.as_deref()),
            (Status::Skipped, Some(runner::FAIL_FAST_SKIP), Some("child"))
        );
        assert!(cut.fail_fast_skip && !failed.fail_fast_skip);
        assert!(interrupted("passed", [cut.fail_fast_skip]));
    }

    #[test]
    fn only_a_passed_child_with_fail_fast_skips_is_interrupted() {
        // Passed jobs and skips by a false condition or a dependency carry no
        // fail-fast flag, so they leave the child passed.
        let (fail_fast, other) = (true, false);
        assert!(!interrupted("passed", [other, other, other]));
        assert!(interrupted("passed", [other, fail_fast]));
        // A failed or cancelled outcome takes precedence.
        assert!(!interrupted("failed", [fail_fast]));
        assert!(!interrupted("cancelled", [fail_fast]));
    }

    /// Run `config` for real in a fresh git checkout with local history on,
    /// and return each invocation's upload record: its history file, which
    /// `history::record` built with no upload keys, plus the `graph`, `ci`
    /// and `source.branch` an upload adds. With `signal`, the run is sent
    /// SIGTERM once a job has created the file `started`.
    async fn upload_records(
        config: &str,
        workflow: Option<&str>,
        profile: Option<&str>,
        jobs: &[&str],
        signal: bool,
    ) -> Vec<serde_json::Value> {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("goodflows.toml"), config).unwrap();
        let git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
                .args(args)
                .current_dir(&root)
                .stdout(std::process::Stdio::null())
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?}");
        };
        git(&["init", "-q"]);
        git(&["checkout", "-q", "-b", "main"]);
        git(&["add", "goodflows.toml"]);
        git(&["commit", "-q", "-m", "init"]);

        let (document, _) = Config::load(Some(&root.join("goodflows.toml"))).unwrap();
        let (name, mut selected) = document.workflow(workflow, profile).unwrap();
        let jobs: Vec<String> = jobs.iter().map(|job| job.to_string()).collect();
        selected.select(&jobs).unwrap();
        let targets = document.called_workflows(&selected.jobs, profile).unwrap();
        // Each invocation's graph, exactly as `invocation` hashes it.
        let resolve = |target: &str| targets[target].jobs.as_slice();
        let mut graphs = BTreeMap::new();
        for (workflow, jobs) in std::iter::once((name.as_str(), &selected.jobs)).chain(
            targets
                .iter()
                .map(|(name, config)| (name.as_str(), &config.jobs)),
        ) {
            let (hash, graph) = history::graph_sha256(jobs, &resolve);
            graphs.insert(workflow.to_owned(), (hash, json!(graph.truncated())));
        }
        let source = reporting::capture_source(&root, None).await;

        let (sender, cancel) = watch::channel(None);
        let history_dir = root.join("history");
        let run = Run {
            calls: selected.jobs.iter().any(|job| job.workflow.is_some()),
            root: selected,
            targets,
            profile: profile.map(str::to_owned),
            parallelism: None,
            fail_fast: None,
            history: config::HistoryOverrides {
                history: Some(true),
                history_dir: Some(history_dir.clone()),
                ..Default::default()
            },
            requested_jobs: jobs,
            log_dir: None,
            legacy_default: !document.jobs.is_empty(),
            config_path: None,
            output: Output::discard(true, cancel.clone()).unwrap(),
            cancel,
            sprite_state: None,
        };
        let started = root.join("started");
        let signaller = tokio::spawn(async move {
            if signal {
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                while !started.exists() {
                    assert!(std::time::Instant::now() < deadline, "no job started");
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
                }
                sender.send(Some(15)).unwrap();
            }
            sender
        });
        let code = execute(run, name, retention::RunIdentity::new())
            .await
            .unwrap();
        drop(signaller.await.unwrap());
        assert_eq!(code, if signal { 143 } else { 0 });

        let ci = reporting::ci::capture(|key| {
            Some(
                match key {
                    "GITHUB_ACTIONS" => "true",
                    "GITHUB_EVENT_NAME" => "push",
                    "GITHUB_RUN_ID" => "123",
                    "GITHUB_RUN_ATTEMPT" => "1",
                    "GITHUB_REF" => "refs/pull/42/merge",
                    "RUNNER_NAME" => "runner-1",
                    "GITHUB_SERVER_URL" => "https://github.com",
                    "GITHUB_REPOSITORY" => "acme/app",
                    _ => return None,
                }
                .to_owned(),
            )
        });
        let mut records = Vec::new();
        for workflow in std::fs::read_dir(&history_dir).unwrap() {
            for file in std::fs::read_dir(workflow.unwrap().path()).unwrap() {
                let bytes = std::fs::read(file.unwrap().path()).unwrap();
                let mut record: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
                let (hash, graph) = &graphs[record["workflow"].as_str().unwrap()];
                assert_eq!(record["graph_sha256"], hash.as_str());
                record["source"]["branch"] = json!(source.branch.as_deref().unwrap());
                record["ci"] = json!(ci);
                record["graph"] = graph.clone();
                records.push(record);
            }
        }
        records
    }

    #[track_caller]
    fn valid(record: &serde_json::Value) {
        let bytes = serde_json::to_vec(record).unwrap();
        assert_eq!(
            reporting::run_contract::validate(record, bytes.len()),
            Ok(()),
            "{record}"
        );
    }

    #[tokio::test]
    async fn upload_records_of_real_runs_validate() {
        // A minimal workflow.
        let records = upload_records(
            "[[jobs]]\nname = 'a'\ncommand = 'true'\n",
            None,
            None,
            &[],
            false,
        )
        .await;
        assert_eq!(records.len(), 1);
        valid(&records[0]);
        assert_eq!(records[0]["source"]["branch"], "main");
        assert_eq!(records[0]["ci"]["pull_request"], 42);

        // Partitions with step and condition costs.
        let records = upload_records(
            "[[jobs]]\nname = 'test'\npartitions = 2\ncondition = { shell = 'true' }\n\
             [[jobs.steps]]\nid = 'unit'\ncommand = 'true'\ncondition = { shell = 'true' }\n\
             [[jobs.steps]]\nid = 'lint'\ncommand = 'true'\n",
            None,
            None,
            &[],
            false,
        )
        .await;
        let record = &records[0];
        valid(record);
        assert_eq!(record["results"].as_array().unwrap().len(), 2);
        let commands = record["results"][0]["commands"].as_array().unwrap();
        assert!(commands
            .iter()
            .any(|c| c["kind"] == "condition" && c.get("step").is_none()));
        assert!(commands
            .iter()
            .any(|c| c["kind"] == "condition" && c["step"] == "unit"));
        assert!(commands
            .iter()
            .any(|c| c["kind"] == "command" && c["step"] == "lint"));

        // A workflow call: the root and the child record.
        let records = upload_records(
            "[[workflows.ci.jobs]]\nname = 'prep'\ncommand = 'true'\n\
             [[workflows.ci.jobs]]\nname = 'call-suite'\nworkflow = 'suite'\ndepends_on = ['prep']\n\
             [[workflows.suite.jobs]]\nname = 'work'\ncommand = 'true'\n",
            Some("ci"),
            None,
            &[],
            false,
        )
        .await;
        assert_eq!(records.len(), 2);
        records.iter().for_each(valid);
        let root = records.iter().find(|r| r["workflow"] == "ci").unwrap();
        let child = records.iter().find(|r| r["workflow"] == "suite").unwrap();
        assert_eq!(child["parent_run_id"], root["run_id"]);
        assert_eq!(child["caller_job"], "call-suite");
        assert_eq!(root["results"][1]["child_run_id"], child["run_id"]);
        assert_eq!(
            root["graph"]["jobs"][1]["call"]["graph_sha256"],
            child["graph_sha256"]
        );

        // `--env` with `--jobs`.
        let records = upload_records(
            "[environments.staging.settings]\nparallelism = 1\n\
             [[jobs]]\nname = 'a'\ncommand = 'true'\n\
             [[jobs]]\nname = 'b'\ncommand = 'true'\ndepends_on = ['a']\n\
             [[jobs]]\nname = 'c'\ncommand = 'true'\n",
            None,
            Some("staging"),
            &["b"],
            false,
        )
        .await;
        let record = &records[0];
        valid(record);
        assert_eq!(record["environment"], "staging");
        assert_eq!(record["requested_jobs"], json!(["b"]));
        assert_eq!(record["graph"]["jobs"].as_array().unwrap().len(), 2);

        // A signal-cancelled run.
        let records = upload_records(
            "[[jobs]]\nname = 'wait'\ncommand = 'touch started; sleep 30'\n\
             [[jobs]]\nname = 'after'\ncommand = 'true'\ndepends_on = ['wait']\n",
            None,
            None,
            &[],
            true,
        )
        .await;
        let record = &records[0];
        valid(record);
        assert_eq!(record["outcome"], "cancelled");
        assert_eq!(record["results"][0]["status"], "cancelled");
    }

    fn parse(args: &[&str]) -> std::result::Result<Cli, clap::Error> {
        Cli::parse_from(std::iter::once("goodflows").chain(args.iter().copied()))
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
