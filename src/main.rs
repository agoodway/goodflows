mod cache;
mod change_requests;
mod config;
mod detach;
mod expression;
mod fsutil;
mod history;
mod init;
mod last;
mod links;
mod mcp;
mod output;
mod reporting;
mod retention;
mod runner;
mod secrets;
mod skills;
mod sprite;
mod tail;

use anyhow::Result;
use clap::{Parser, Subcommand};
use config::{Config, ConfigError};
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
    version,
    after_help = "Examples:
  goodflows                          # run the default workflow
  goodflows --jobs test              # run the test job and the jobs it depends on
  goodflows run ci --json=summary    # run the ci workflow; print only its start, errors and summary
  goodflows validate --env ci        # check the config with the ci environment applied"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
    /// Config file (default: goodflows.toml, else .goodflows.toml, in the current directory)
    #[arg(short, long, global = true)]
    config: Option<PathBuf>,
    /// Apply the [environments.<NAME>] profile's settings and partition counts (no profile applies without this flag)
    #[arg(long, global = true, value_name = "NAME")]
    env: Option<String>,
    /// Maximum concurrent jobs (default: the settings' parallelism, else the logical CPU count; 0 means the CPU count)
    #[arg(short, long, global = true)]
    parallelism: Option<usize>,
    /// Stop launching jobs after a failure; running jobs finish (the default unless settings set fail_fast = false)
    #[arg(long, global = true, conflicts_with = "no_fail_fast")]
    fail_fast: bool,
    /// Keep launching jobs that do not depend on a failed one (conflicts with --fail-fast)
    #[arg(long, global = true)]
    no_fail_fast: bool,
    /// Run only these jobs, comma-separated or repeated, and the jobs they depend on (names from `goodflows list`)
    #[arg(long, global = true, value_delimiter = ',')]
    jobs: Vec<String>,
    /// Emit versioned JSON Lines events to stdout; `--json=summary` (with `=`) prints only a run's start, errors and summary
    #[arg(long, global = true, value_enum, value_name = "EVENTS", num_args = 0..=1, require_equals = true, default_missing_value = "all")]
    json: Option<output::Json>,
    /// Run text output [default: failures when stdout is not a terminal or a coding agent runs goodflows, else stream]
    #[arg(long, value_enum, value_name = "MODE")]
    output: Option<output::Mode>,
    /// Save raw per-job stdout/stderr files in a new per-run directory under this directory
    #[arg(long, global = true)]
    log_dir: Option<PathBuf>,
    /// Write a local run history file for this run, even when settings disable it (conflicts with --no-history)
    #[arg(long, global = true, conflicts_with = "no_history")]
    history: bool,
    /// Do not write the local run history file, even when settings enable it (does not stop [reporting] history uploads)
    #[arg(long, global = true)]
    no_history: bool,
    /// Write the run history file under this directory (enables history)
    #[arg(long, global = true, value_name = "DIR")]
    history_dir: Option<PathBuf>,
    /// Link the run to this ticket ID in its history, besides the one in the branch name (repeatable, at most 32)
    #[arg(long, global = true, value_name = "ID", value_parser = links::ticket_arg)]
    ticket: Vec<String>,
    /// Link the run to this agent thread URL in its history, besides the current agent thread's (repeatable, at most 32)
    #[arg(long = "thread-url", global = true, value_name = "URL", value_parser = links::thread_url_arg)]
    thread_url: Vec<String>,
    /// Log run directories to keep per workflow (default 10)
    #[arg(long, global = true, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    log_runs: Option<u64>,
    /// History files to keep per workflow (default 1000)
    #[arg(long, global = true, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    history_runs: Option<u64>,
    /// History bytes to keep per workflow (default 10485760)
    #[arg(long, global = true, value_name = "BYTES", value_parser = clap::value_parser!(u64).range(1..))]
    history_bytes: Option<u64>,
    /// How many times `--ticket` was typed, before any `--`.
    #[arg(skip)]
    tickets_typed: usize,
    /// How many times `--thread-url` was typed, before any `--`.
    #[arg(skip)]
    thread_urls_typed: usize,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a workflow's jobs; also what bare `goodflows` does
    #[command(after_help = "Examples:
  goodflows run                      # run the default workflow
  goodflows run --jobs test          # run the test job and the jobs it depends on
  goodflows run ci --json            # run the ci workflow and print JSON Lines events
  goodflows run --failed             # rerun only the jobs the last run named as failed")]
    Run {
        /// Workflow to run (default: the workflow named `default`, or the config's top-level jobs)
        workflow: Option<String>,
        /// Run the whole workflow on this caller identity's owned Sprite (see docs/sprites.md)
        #[arg(long, value_name = "IDENTITY", value_parser = sprite::naming::parse_identity)]
        sprite: Option<String>,
        /// Text output [default: failures when stdout is not a terminal or a coding agent runs goodflows, else stream]
        #[arg(long, value_enum, value_name = "MODE")]
        output: Option<output::Mode>,
        /// Rerun only the jobs the last local run's `Rerun failed:` hint names, in its workflow and environment (conflicts with --jobs and --sprite; a different WORKFLOW or --env exits 2)
        #[arg(long, conflicts_with = "sprite")]
        failed: bool,
        /// Start the run in the background and print its run id; follow it with `wait`, `status` and `cancel` (conflicts with --sprite)
        #[arg(long, conflicts_with = "sprite")]
        detach: bool,
        /// Internal: run as the background process of a detached run that owns this directory
        #[arg(long, hide = true, value_name = "DIR", requires = "detach")]
        detached_run: Option<PathBuf>,
    },
    /// Show a detached run's progress (from `run --detach`): running, passed and failed jobs
    #[command(after_help = "Examples:
  id=$(goodflows run --detach)       # start a run in the background
  goodflows status \"$id\"             # show which jobs are running, passed and failed
  goodflows status \"$id\" --json      # print one run_status event")]
    Status {
        /// The run id `run --detach` printed
        #[arg(value_parser = reporting::history_client::run_id)]
        run_id: String,
    },
    /// Wait for a detached run, then print its report and exit with its exit code (75 when the timeout passes first)
    #[command(after_help = "Examples:
  id=$(goodflows run --detach)       # start a run in the background
  goodflows wait \"$id\"               # wait for it, then print its report
  goodflows wait \"$id\" --timeout 90  # stop waiting after 90 seconds; exit 75 while it still runs
  goodflows wait \"$id\" --json        # print its summary events as JSON")]
    Wait {
        /// The run id `run --detach` printed
        #[arg(value_parser = reporting::history_client::run_id)]
        run_id: String,
        /// Stop waiting after this many seconds and exit 75 while the run goes on (default: no limit)
        #[arg(long, value_name = "SECS")]
        timeout: Option<u64>,
    },
    /// Cancel a detached run: send it SIGTERM, then report it once it has stopped (75 when still stopping after 30 seconds)
    #[command(after_help = "Examples:
  id=$(goodflows run --detach)       # start a run in the background
  goodflows cancel \"$id\"             # stop it; its running and waiting jobs are cancelled
  goodflows cancel \"$id\" --json      # print one run_status event once it has stopped")]
    Cancel {
        /// The run id `run --detach` printed
        #[arg(value_parser = reporting::history_client::run_id)]
        run_id: String,
    },
    /// Show the last local run's results and failure output again, without running anything, and exit with its exit code
    #[command(after_help = "Examples:
  goodflows last                     # show the last run's results and failure output
  goodflows last --json              # print the last run's summary events as JSON
  goodflows last --config ci.toml    # show the last run of the project configured by ci.toml
  goodflows run --failed             # then rerun only the jobs it named as failed")]
    Last,
    /// Check the config, and a workflow, --env and --jobs selection, without running anything (exit 2 on a problem)
    #[command(after_help = "Examples:
  goodflows validate                        # check every workflow under every environment
  goodflows validate ci                     # check only the ci workflow
  goodflows validate --env ci               # check with the ci environment applied
  goodflows validate ci --jobs test --json  # show the jobs `run ci --jobs test` would run")]
    Validate {
        /// Workflow to check (default: every workflow; --jobs needs one)
        workflow: Option<String>,
    },
    /// List a workflow's jobs and their dependencies, or the workflows with --workflows
    #[command(after_help = "Examples:
  goodflows list                     # list the default workflow's jobs and dependencies
  goodflows list ci                  # list the ci workflow's jobs
  goodflows list --workflows         # list the available workflows
  goodflows list ci --env ci --json  # print the ci workflow's jobs and settings under the ci environment")]
    List {
        /// Workflow whose jobs to list (default: the workflow named `default`, or the config's top-level jobs)
        #[arg(conflicts_with = "workflows")]
        workflow: Option<String>,
        /// List available workflows rather than jobs (conflicts with WORKFLOW and --jobs)
        #[arg(long, conflicts_with = "jobs")]
        workflows: bool,
    },
    /// Write a starter config with jobs for the detected project type (Rust, Elixir, Node or Go)
    #[command(after_help = "Examples:
  goodflows init                         # detect the project type and write goodflows.toml
  goodflows init --template rust         # use the Rust template instead of detection
  goodflows init --workflow ci --force   # put the jobs in a ci workflow; overwrite any existing config
  goodflows init --config ci.toml        # write the config to ci.toml")]
    Init {
        /// The project type (default: detected from Cargo.toml, mix.exs, package.json or go.mod; generic when none is found)
        #[arg(long, value_enum)]
        template: Option<init::Template>,
        /// Overwrite an existing config without prompting (without a terminal, init otherwise exits 2 with config_exists)
        #[arg(long)]
        force: bool,
        /// Generate jobs inside this named workflow (default: top-level jobs, the default workflow)
        #[arg(long)]
        workflow: Option<String>,
    },
    /// Retry uploading a run's saved test timing reports and run record to the [reporting] server
    #[command(after_help = REPORTS_EXAMPLES)]
    Reports {
        #[command(subcommand)]
        command: ReportsCommand,
    },
    /// Read run records uploaded to the [reporting] server (for this checkout's last local run, use `last`)
    #[command(after_help = "Examples:
  goodflows history list                                     # list stored runs, newest first
  goodflows history list --workflow ci --branch main         # only ci runs on main
  goodflows history list --outcome failed --limit 20 --json  # the 20 newest failed runs as JSON
  goodflows history show <RUN_ID>                            # one run with its results and test reports")]
    History {
        #[command(subcommand)]
        command: HistoryCommand,
    },
    /// Change requests: create one from a claims file (cr open), update it (cr claims), and report facts, scenario sets and verify runs
    #[command(after_help = change_requests::CR_EXAMPLES)]
    Cr {
        #[command(subcommand)]
        command: change_requests::CrCommand,
    },
    /// Read and write the project's review policy, which decides change request verdicts
    #[command(after_help = "Examples:
  goodflows policy get                       # the current policy as TOML, with its version
  goodflows policy set policy.toml --note Require-ci   # owners and admins write a new version
  goodflows policy history                   # versions, newest first
  goodflows policy calibration               # merges and defects by lane, rule and path glob")]
    Policy {
        #[command(subcommand)]
        command: change_requests::PolicyCommand,
    },
    /// Sync the verify skill's feature files to the project's feature index
    #[command(after_help = "Examples:
  goodflows features sync                    # the one .agents/skills/verify-*/features directory
  goodflows features sync --dir .claude/skills/verify-web/features   # another directory
  goodflows features sync --json             # print the version and features as JSON")]
    Features {
        #[command(subcommand)]
        command: change_requests::FeaturesCommand,
    },
    /// Check goodflows changes (goodflows/changes/<id>/), record them as verified, and archive them into goodflows/specs/
    #[command(after_help = "Examples:
  goodflows change validate                  # every change under goodflows/changes/
  goodflows change validate add-login --json # one change's errors and warnings as JSON
  goodflows change verified add-login        # record what the verified build looks like
  goodflows change archive add-login         # merge an approved, verified change's deltas and archive it")]
    Change {
        #[command(subcommand)]
        command: change_requests::ChangeCommand,
    },
    /// Read the project's event feed: change request states, verdicts, scenario sets, answers and tasks
    #[command(after_help = "Examples:
  goodflows events                           # the oldest kept events
  goodflows events --after 1042 --wait 30    # wait up to 30 seconds for events after id 1042
  goodflows events --follow --json           # stream events as JSON lines until interrupted")]
    Events(change_requests::EventsArgs),
    /// Inspect, recover, cancel or destroy a caller identity's Sprite
    #[command(after_help = "Examples:
  goodflows run ci --sprite T-1              # run the ci workflow on T-1's Sprite
  goodflows sprite status T-1                # ownership, preparation, active and recent runs
  goodflows sprite collect T-1 --run <UUID>  # download a run's results into a local bundle
  goodflows sprite destroy T-1 --yes         # destroy T-1's idle Sprite without prompting")]
    Sprite {
        #[command(subcommand)]
        command: sprite::Command,
    },
    /// Install goodflows's agent skills where the current coding agent loads them
    #[command(after_help = SKILLS_EXAMPLES)]
    Skills {
        #[command(subcommand)]
        command: SkillsCommand,
    },
    /// Print the guide for coding agents: exit codes, error codes, reading a failure, detached runs and JSON fields
    #[command(after_help = "Examples:
  goodflows agents                   # print the guide for this goodflows version
  goodflows last                     # then show the last run's failures again
  goodflows run --failed             # and rerun only the jobs that failed")]
    Agents,
    /// Serve goodflows to coding agents as an MCP server on stdin and stdout
    #[command(after_help = "Examples:
  goodflows mcp                      # serve the run, wait, status, cancel, list, validate and last tools
  goodflows mcp --config ci.toml     # tools use ci.toml unless a call passes its own config
  goodflows skills install           # also install the CI skills for this harness")]
    Mcp,
    /// Internal: the remote runner protocol used on a Sprite
    #[command(name = "__sprite-agent", hide = true)]
    SpriteAgent {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
}

const REPORTS_EXAMPLES: &str = "Examples:
  goodflows reports upload .goodflows/reports/<RUN_ID>               # retry publishing one run's reports
  goodflows reports upload .goodflows/reports/<RUN_ID> --budget 120  # allow 120 seconds for slow links
  goodflows reports upload .goodflows/reports/<RUN_ID> --json        # print the upload events as JSON";

#[derive(Subcommand)]
enum ReportsCommand {
    /// Retry publishing a saved invocation's finalized reports and run record without running jobs; exits 1 unless the server stored them all
    #[command(after_help = REPORTS_EXAMPLES)]
    Upload {
        /// Run directory under the reporting spool (default spool .goodflows/reports; it contains manifest.json)
        run_directory: PathBuf,
        /// Total publishing budget in seconds (default 30), for large reports on slow links
        #[arg(long, value_name = "SECONDS", value_parser = clap::value_parser!(u64).range(1..=3600))]
        budget: Option<u64>,
    },
}

const SKILLS_EXAMPLES: &str = "Examples:
  goodflows skills install                    # install into the directory this harness loads
  goodflows skills install --harness claude   # install into .claude/skills
  goodflows skills install --json             # print the files written as a JSON event";

#[derive(Subcommand)]
enum SkillsCommand {
    /// Write create-ci-skill, maintain-ci-skill and gf-change-explore into the harness's skills directory, replacing earlier copies
    #[command(after_help = SKILLS_EXAMPLES)]
    Install {
        /// The harness to install for (default: detected from its environment variables; .agents/skills when none is)
        #[arg(long, value_enum)]
        harness: Option<skills::Harness>,
    },
}

#[derive(Subcommand)]
enum HistoryCommand {
    /// List stored runs, newest first
    #[command(after_help = "Examples:
  goodflows history list                                     # list stored runs, newest first
  goodflows history list --workflow ci --branch main         # only ci runs on main
  goodflows history list --outcome failed --limit 20 --json  # the 20 newest failed runs as JSON")]
    List(Box<reporting::history_client::Filters>),
    /// Show one stored run with its results and linked test reports
    #[command(after_help = "Examples:
  goodflows history show <RUN_ID>          # the run's results and linked test reports
  goodflows history show <RUN_ID> --json   # the stored run record as JSON
  goodflows history list --json            # find run ids to show")]
    Show {
        /// The run's id (a lowercase canonical UUID)
        #[arg(value_parser = reporting::history_client::run_id)]
        run_id: String,
    },
}

/// Global flags that only `run` uses; `init`, `reports upload`, `history`,
/// `last`, `status`, `wait`, `cancel`, `sprite`, `skills install`, `agents` and `mcp` commands
/// reject them. `validate`
/// and `list` resolve all of them except `--log-dir` into their settings.
const RUN_ONLY_FLAGS: [&str; 14] = [
    "env",
    "parallelism",
    "fail_fast",
    "no_fail_fast",
    "jobs",
    "log_dir",
    "history",
    "no_history",
    "history_dir",
    "ticket",
    "thread_url",
    "log_runs",
    "history_runs",
    "history_bytes",
];

impl Cli {
    /// The parser, with each command's help showing only the global flags
    /// it uses. The others still parse there, so passing one is rejected
    /// with a clear error rather than silently ignored. `run` and bare
    /// `goodflows` show them all.
    fn parser() -> clap::Command {
        let mut command = <Self as clap::CommandFactory>::command();
        // Globals reach subcommands only when the command is built.
        command.build();
        fn hide(command: clap::Command, flags: &[&str]) -> clap::Command {
            command.mut_args(|arg| {
                let hidden = flags.contains(&arg.get_id().as_str()) || arg.is_hide_set();
                arg.hide(hidden)
            })
        }
        let run_only = |command| hide(command, &RUN_ONLY_FLAGS);
        // A command that runs no jobs has no run events to choose between,
        // so its `--json` takes no value.
        fn plain_json(command: clap::Command, help: &'static str) -> clap::Command {
            // `mut_args` keeps the built command's argument order.
            command.mut_args(|arg| match arg.get_id().as_str() {
                "json" => arg.num_args(0).help(help),
                // Not "see a summary with '-h'": no summary is in sight.
                "help" => arg
                    .help("Print help")
                    .long_help(clap::builder::Resettable::<clap::builder::StyledStr>::Reset),
                _ => arg,
            })
        }
        /// Applies `f` to a command and all its subcommands.
        fn every(
            command: clap::Command,
            f: &dyn Fn(clap::Command) -> clap::Command,
        ) -> clap::Command {
            f(command).mut_subcommands(|sub| every(sub, f))
        }
        const ONE_EVENT: &str = "Print the result as a JSON event";
        let single = |command| plain_json(run_only(command), ONE_EVENT);
        // `reports upload` and `skills install` reject `--config` too.
        let configless = |command| hide(run_only(command), &["config"]);
        let unlogged = |command| hide(command, &["log_dir"]);
        /// Shows a command's own `--ticket` and `--thread-url`, which share
        /// their ids with the hidden run flags.
        fn own_links(command: clap::Command) -> clap::Command {
            command.mut_args(|arg| {
                let own = !arg.is_global_set()
                    && ["ticket", "thread_url"].contains(&arg.get_id().as_str());
                if own {
                    arg.hide(false)
                } else {
                    arg
                }
            })
        }
        command
            .mut_subcommand("init", |init| {
                // `mut_arg` would rebuild the global `--config` without its value.
                single(init).mut_args(|arg| match arg.get_id().as_str() {
                    "config" => arg.help("Where to write the config (default: goodflows.toml)"),
                    _ => arg,
                })
            })
            .mut_subcommand("validate", unlogged)
            .mut_subcommand("list", unlogged)
            .mut_subcommand("reports", |r| configless(r).mut_subcommands(configless))
            .mut_subcommand("sprite", |sprite| {
                run_only(sprite).mut_subcommands(run_only)
            })
            .mut_subcommand("history", |history| {
                // `list` has its own `--ticket` and `--thread-url` filters.
                run_only(history)
                    .mut_subcommands(run_only)
                    .mut_subcommand("list", own_links)
            })
            .mut_subcommand("cr", |cr| {
                every(cr, &single).mut_subcommand("defect", own_links)
            })
            .mut_subcommand("policy", |policy| every(policy, &single))
            .mut_subcommand("features", |features| every(features, &single))
            .mut_subcommand("events", |events| {
                // `--follow` prints one event per page.
                plain_json(
                    run_only(events),
                    "Print each page of events as a JSON event",
                )
            })
            .mut_subcommand("last", |last| {
                // A run that called workflows has a summary for each.
                plain_json(run_only(last), "Print the result as JSON events")
            })
            .mut_subcommand("status", single)
            .mut_subcommand("cancel", single)
            .mut_subcommand("wait", run_only)
            .mut_subcommand("skills", |s| {
                let configless = |s| plain_json(configless(s), ONE_EVENT);
                configless(s).mut_subcommands(configless)
            })
            // `agents` prints one fixed text.
            .mut_subcommand("agents", |agents| hide(configless(agents), &["json"]))
            // `mcp` speaks MCP on stdout; `--config` is its tools' default.
            .mut_subcommand("mcp", |mcp| hide(run_only(mcp), &["json"]))
    }

    /// The first run-only flag on the command line, as the user typed it, for
    /// commands that reject them all ([`RUN_ONLY_FLAGS`]).
    fn run_only_flag(&self) -> Option<&'static str> {
        self.run_only_flag_besides(&[])
    }

    /// [`Cli::run_only_flag`], ignoring the flags in `skip`.
    fn run_only_flag_besides(&self, skip: &[&str]) -> Option<&'static str> {
        [
            (self.env.is_some(), "--env"),
            (self.parallelism.is_some(), "--parallelism"),
            (self.fail_fast, "--fail-fast"),
            (self.no_fail_fast, "--no-fail-fast"),
            (!self.jobs.is_empty(), "--jobs"),
            (self.log_dir.is_some(), "--log-dir"),
            (self.history, "--history"),
            (self.no_history, "--no-history"),
            (self.history_dir.is_some(), "--history-dir"),
            (!self.ticket.is_empty(), "--ticket"),
            (!self.thread_url.is_empty(), "--thread-url"),
            (self.log_runs.is_some(), "--log-runs"),
            (self.history_runs.is_some(), "--history-runs"),
            (self.history_bytes.is_some(), "--history-bytes"),
            (self.output.is_some(), "--output"),
        ]
        .into_iter()
        .find_map(|(set, flag)| (set && !skip.contains(&flag)).then_some(flag))
    }

    fn parse_from(
        args: impl IntoIterator<Item = impl Into<std::ffi::OsString> + Clone>,
    ) -> std::result::Result<Self, clap::Error> {
        let args: Vec<std::ffi::OsString> = args.into_iter().map(Into::into).collect();
        let typed = |flag: &str| {
            let joined = format!("{flag}=");
            args.iter()
                .take_while(|arg| *arg != "--")
                .filter(|arg| *arg == flag || arg.to_string_lossy().starts_with(&joined))
                .count()
        };
        let (tickets_typed, thread_urls_typed) = (typed("--ticket"), typed("--thread-url"));
        let matches = Self::parser().try_get_matches_from(args.iter())?;
        let cli = <Self as clap::FromArgMatches>::from_arg_matches(&matches)?;
        // `cr tasks -n` is global so it can follow `done` too, and clap keeps
        // only the last of a global flag given at both levels.
        if let Some(Commands::Cr {
            command: change_requests::CrCommand::Tasks { .. },
        }) = &cli.command
        {
            let numbers = args
                .iter()
                .map(|arg| arg.to_string_lossy())
                .take_while(|arg| arg != "--")
                .filter(|arg| {
                    (arg.starts_with("-n") && !arg.starts_with("--"))
                        || *arg == "--number"
                        || arg.starts_with("--number=")
                })
                .count();
            if numbers > 1 {
                let mut parser = Self::parser();
                let tasks = parser
                    .find_subcommand_mut("cr")
                    .and_then(|cr| cr.find_subcommand_mut("tasks"))
                    .expect("cr tasks exists");
                return Err(tasks.error(
                    clap::error::ErrorKind::ArgumentConflict,
                    "the argument '--number <N>' cannot be used multiple times",
                ));
            }
        }
        Ok(Self {
            tickets_typed,
            thread_urls_typed,
            ..cli
        })
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

    /// The text output mode. Only a run streams job output, so every other
    /// command renders in full.
    fn output_mode(&self) -> output::Mode {
        match &self.command {
            None => self.output,
            Some(Commands::Run { output, .. }) => output.or(self.output),
            Some(_) => return output::Mode::Stream,
        }
        .unwrap_or_else(output::Mode::detect)
    }

    /// Whether the command runs a workflow. `run --detach` only starts one
    /// in the background, which runs it.
    fn runs(&self) -> bool {
        matches!(
            self.command,
            None | Some(
                Commands::Run { detach: false, .. }
                    | Commands::Run {
                        detached_run: Some(_),
                        ..
                    }
            )
        )
    }

    fn workflow(&self) -> Option<&str> {
        match &self.command {
            Some(
                Commands::Run { workflow, .. }
                | Commands::List { workflow, .. }
                | Commands::Validate { workflow }
                | Commands::Init { workflow, .. },
            ) => workflow.as_deref(),
            // `history list --workflow` is a filter, not the workflow events belong to.
            Some(
                Commands::Reports { .. }
                | Commands::History { .. }
                | Commands::Cr { .. }
                | Commands::Policy { .. }
                | Commands::Features { .. }
                | Commands::Events(_)
                | Commands::Last
                | Commands::Status { .. }
                | Commands::Wait { .. }
                | Commands::Cancel { .. }
                | Commands::Sprite { .. }
                | Commands::Skills { .. }
                | Commands::Change { .. }
                | Commands::Agents
                | Commands::Mcp
                | Commands::SpriteAgent { .. },
            )
            | None => None,
        }
    }
}

#[tokio::main]
async fn main() -> ExitCode {
    let raw_args: Vec<_> = std::env::args_os().collect();
    let json_requested = raw_args
        .iter()
        .any(|arg| arg == "--json" || arg.to_string_lossy().starts_with("--json="));
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
            if let Ok(output) = Output::new(json_requested, output::Mode::Stream, cancel) {
                let _ = output
                    .emit(json!({"type":"error", "code":"usage", "message":error.to_string()}))
                    .await;
            }
            return ExitCode::from(2);
        }
    };
    if let Some(Commands::SpriteAgent { args }) = &cli.command {
        return ExitCode::from(sprite::agent::main(args).await);
    }
    // The background process of a detached run holds the run's lock until it exits.
    let background = match &cli.command {
        Some(Commands::Run {
            detached_run: Some(dir),
            ..
        }) => match detach::Background::start(dir) {
            Ok(background) => Some(background),
            Err(error) => {
                eprintln!("goodflows: detached run: {error:#}");
                return ExitCode::FAILURE;
            }
        },
        _ => None,
    };
    let (_signal_sender, cancel) = match if matches!(
        cli.command,
        None | Some(
            Commands::Run { .. }
                | Commands::Reports { .. }
                | Commands::History { .. }
                | Commands::Cr { .. }
                | Commands::Policy { .. }
                | Commands::Features { .. }
                | Commands::Events(_)
                | Commands::Sprite { .. }
                | Commands::Mcp
        )
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
    let output = match Output::new(cli.json.is_some(), cli.output_mode(), cancel.clone()) {
        // Only a run has progress events to leave out.
        Ok(output) if cli.json == Some(output::Json::Summary) && cli.runs() => {
            output.summary_only()
        }
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
    let code = match run(&cli, &mut output, cancel.clone()).await {
        Ok(code) => code,
        Err(error) => {
            // A usage or config error exits 2; anything else stopped a run
            // that the caller asked for correctly, and exits 1 like a failure.
            let config = error.downcast_ref::<ConfigError>();
            let secret = error.downcast_ref::<secrets::Unavailable>();
            let code = match (config, secret) {
                (Some(config), _) => config.code,
                (None, Some(_)) => "secret_unavailable",
                (None, None) => "runtime",
            };
            let mut event = json!({"type":"error", "code":code, "message":format!("{error:#}")});
            if let Some(secret) = secret {
                event["secret"] = json!(secret.secret);
            }
            if let Some(hint) = config.and_then(|e| e.hint.as_ref()) {
                event["hint"] = json!(hint);
            }
            if let Some(config) = config.filter(|e| !e.problems.is_empty()) {
                event["problems"] = json!(config.problems);
            }
            let _ = output.emit(event).await;
            let code = if config.is_some() { 2 } else { 1 };
            cancel.borrow().map(|s| (128 + s) as u8).unwrap_or(code)
        }
    };
    if let Some(background) = background {
        background.finish(code);
    }
    ExitCode::from(code)
}

/// What `goodflows agents` prints after its version line.
const AGENTS_GUIDE: &str = include_str!("agents_guide.md");

/// A command line that parsed but asks for something goodflows does not do.
fn usage(message: impl Into<String>) -> anyhow::Error {
    ConfigError::new("usage", message).into()
}

async fn run(cli: &Cli, output: &mut Output, cancel: watch::Receiver<Option<i32>>) -> Result<u8> {
    if let Some(Commands::Init {
        template,
        force,
        workflow,
    }) = &cli.command
    {
        if cli.env.is_some() {
            return Err(usage(
                "--env cannot be used with init; define environment profiles in the config",
            ));
        }
        if let Some(flag) = cli.run_only_flag() {
            return Err(usage(format!("{flag} cannot be used with init")));
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
        let config = cli.config.is_some().then_some("--config");
        if let Some(flag) = config.or_else(|| cli.run_only_flag()) {
            return Err(usage(format!("{flag} cannot be used with reports upload")));
        }
        let budget = budget.map(std::time::Duration::from_secs);
        return reporting::replay::upload(run_directory, output, cancel, budget).await;
    }
    if let Some(Commands::History { command }) = &cli.command {
        // `history list`'s `--ticket` and `--thread-url` filters share their
        // ids with the run flags, and clap merges every occurrence, before
        // `history` or after `list`, into one value. Typed once, the flag is
        // the filter; typed more often, it is also a run flag.
        let mut filters = Vec::new();
        if let HistoryCommand::List(_) = command {
            for (typed, flag) in [
                (cli.tickets_typed, "--ticket"),
                (cli.thread_urls_typed, "--thread-url"),
            ] {
                if typed == 1 {
                    filters.push(flag);
                }
            }
        }
        if let Some(flag) = cli.run_only_flag_besides(&filters) {
            return Err(usage(format!(
                "{flag} cannot be used with history commands"
            )));
        }
        let (document, _) = Config::load(cli.config.as_deref())?;
        let reporting = document.reporting.as_ref();
        return match command {
            HistoryCommand::List(filters) => {
                reporting::history_client::list(reporting, filters, output, cancel).await
            }
            HistoryCommand::Show { run_id } => {
                reporting::history_client::show(reporting, run_id, output, cancel).await
            }
        };
    }
    if let Some(
        command @ (Commands::Cr { .. }
        | Commands::Policy { .. }
        | Commands::Features { .. }
        | Commands::Events(_)),
    ) = &cli.command
    {
        let name = match command {
            Commands::Cr { .. } => "cr",
            Commands::Policy { .. } => "policy",
            Commands::Features { .. } => "features",
            _ => "events",
        };
        // `cr defect --ticket` shares its id with the run flag, as `history
        // list --ticket` does: typed once, it is the defect's ticket.
        let defect = matches!(
            command,
            Commands::Cr {
                command: change_requests::CrCommand::Defect { .. }
            }
        );
        let own: &[&str] = if defect && cli.tickets_typed == 1 {
            &["--ticket"]
        } else {
            &[]
        };
        if let Some(flag) = cli.run_only_flag_besides(own) {
            return Err(usage(format!("{flag} cannot be used with {name} commands")));
        }
        // Validating files needs no server, so no config either.
        let local = matches!(
            command,
            Commands::Cr {
                command: change_requests::CrCommand::Validate { .. }
            } | Commands::Features {
                command: change_requests::FeaturesCommand::Validate { .. }
            }
        );
        let document = if local {
            None
        } else {
            Some(Config::load(cli.config.as_deref())?.0)
        };
        let reporting = document.as_ref().and_then(|d| d.reporting.as_ref());
        return match command {
            Commands::Cr { command } => {
                change_requests::cr(reporting, command, output, cancel).await
            }
            Commands::Policy { command } => {
                change_requests::policy(reporting, command, output, cancel).await
            }
            Commands::Features { command } => {
                change_requests::features(reporting, command, output, cancel).await
            }
            Commands::Events(args) => {
                change_requests::events(reporting, args, output, cancel).await
            }
            _ => unreachable!(),
        };
    }
    if let Some(Commands::Sprite { command }) = &cli.command {
        return sprite::command(cli, command, output, cancel).await;
    }
    if let Some(Commands::Change { command }) = &cli.command {
        let config = cli.config.is_some().then_some("--config");
        if let Some(flag) = config.or_else(|| cli.run_only_flag()) {
            return Err(usage(format!("{flag} cannot be used with change commands")));
        }
        return change_requests::change(command, output).await;
    }
    if let Some(Commands::Skills {
        command: SkillsCommand::Install { harness },
    }) = &cli.command
    {
        let config = cli.config.is_some().then_some("--config");
        if let Some(flag) = config.or_else(|| cli.run_only_flag()) {
            return Err(usage(format!("{flag} cannot be used with skills install")));
        }
        return skills::install(*harness, output).await;
    }
    if let Some(Commands::Agents) = &cli.command {
        let rejected = [
            (cli.config.is_some(), "--config"),
            (cli.json.is_some(), "--json"),
        ];
        let flag = rejected
            .into_iter()
            .find_map(|(set, flag)| set.then_some(flag));
        if let Some(flag) = flag.or_else(|| cli.run_only_flag()) {
            return Err(usage(format!("{flag} cannot be used with agents")));
        }
        print!(
            "goodflows {}: running checks and reading a failure\n\n{AGENTS_GUIDE}",
            env!("CARGO_PKG_VERSION")
        );
        return Ok(0);
    }
    if let Some(Commands::Mcp) = &cli.command {
        let json = cli.json.is_some().then_some("--json");
        if let Some(flag) = json.or_else(|| cli.run_only_flag()) {
            return Err(usage(format!("{flag} cannot be used with mcp")));
        }
        return mcp::serve(cli.config.clone(), cancel).await;
    }
    if let Some(Commands::Last) = &cli.command {
        if let Some(flag) = cli.run_only_flag() {
            return Err(usage(format!("{flag} cannot be used with last")));
        }
        return last::show(&Config::locate(cli.config.as_deref())?, output).await;
    }
    if let Some(
        Commands::Status { run_id } | Commands::Wait { run_id, .. } | Commands::Cancel { run_id },
    ) = &cli.command
    {
        let command = match cli.command {
            Some(Commands::Status { .. }) => "status",
            Some(Commands::Cancel { .. }) => "cancel",
            _ => "wait",
        };
        if let Some(flag) = cli.run_only_flag() {
            return Err(usage(format!("{flag} cannot be used with {command}")));
        }
        let config_file = Config::locate(cli.config.as_deref())?;
        return match &cli.command {
            Some(Commands::Wait { timeout, .. }) => {
                detach::wait(
                    &config_file,
                    run_id,
                    *timeout,
                    cli.config.as_deref(),
                    output,
                )
                .await
            }
            Some(Commands::Cancel { .. }) => {
                detach::cancel(&config_file, run_id, cli.config.as_deref(), output).await
            }
            _ => detach::status(&config_file, run_id, output).await,
        };
    }
    if let Some(Commands::Validate { .. } | Commands::List { .. }) = &cli.command {
        if cli.log_dir.is_some() {
            let command = if matches!(cli.command, Some(Commands::List { .. })) {
                "list"
            } else {
                "validate"
            };
            return Err(usage(format!("--log-dir cannot be used with {command}")));
        }
    }
    // clap keeps only the values given on one side of the subcommand.
    for (typed, values, flag) in [
        (cli.tickets_typed, &cli.ticket, "--ticket"),
        (cli.thread_urls_typed, &cli.thread_url, "--thread-url"),
    ] {
        if typed != values.len() {
            return Err(usage(format!(
                "{flag} must be given on one side of the subcommand"
            )));
        }
        if values.len() > links::MAX_ENTRIES {
            return Err(usage(format!(
                "{flag} can be given at most {} times",
                links::MAX_ENTRIES
            )));
        }
    }
    let link_flags = links::Links {
        threads: cli.thread_url.clone(),
        tickets: cli.ticket.clone(),
    };
    let rerun_failed = matches!(cli.command, Some(Commands::Run { failed: true, .. }));
    if rerun_failed && !cli.jobs.is_empty() {
        return Err(usage("--jobs cannot be used with run --failed"));
    }
    let (document, config_file) = Config::load(cli.config.as_deref())?;
    // `run --failed` takes its workflow, environment and jobs from the last run.
    let failed = rerun_failed
        .then(|| last::failed(&config_file, cli.workflow(), cli.env.as_deref()))
        .transpose()?;
    let (workflow, profile, jobs) = match &failed {
        Some(failed) => {
            if let Some(environment) = &failed.environment {
                *output = output.with_environment(environment);
            }
            (
                Some(failed.workflow.as_str()),
                failed.environment.as_deref(),
                &failed.jobs,
            )
        }
        None => (cli.workflow(), cli.env.as_deref(), &cli.jobs),
    };
    let history_cli = cli.history_overrides()?;
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
            return Err(usage(
                "--jobs requires an explicit workflow for aggregate validation",
            ));
        }
        // Every workflow and profile that does not resolve, reported together.
        let mut problems = Vec::new();
        if aggregate_validation && profile.is_none() {
            // Without --env, check that every profile resolves for every
            // workflow, so `validate` passes only if each `run <workflow>
            // --env <profile>` would get past configuration.
            for environment in document.environments.keys() {
                for name in document.workflow_names() {
                    if let Err(error) = document.workflow(Some(&name), Some(environment)) {
                        problems.push(error);
                    }
                }
            }
        }
        let mut workflows = Vec::new();
        let mut total = 0;
        for name in document.workflow_names() {
            let config = match document.workflow(Some(&name), profile) {
                Ok((_, config)) => config,
                Err(error) => {
                    problems.push(error);
                    continue;
                }
            };
            total += config.jobs.len();
            workflows.push(json!({"name":name, "jobs":config.jobs.len(), "selected_jobs":config.jobs.iter().map(|j| &j.name).collect::<Vec<_>>(), "requested_jobs":[], "total_defined_jobs":config.jobs.len(), "settings":config.settings.resolve(cli.parallelism, fail_fast_override, &history_cli)?}));
        }
        ConfigError::check(problems)?;
        output.emit(if list_workflows {
            json!({"type":"workflows", "workflows":workflows, "default_workflow":document.default_workflow()})
        } else {
            json!({"type":"config_valid", "workflows":workflows, "jobs":total, "total_defined_jobs":total, "requested_jobs":[], "selected_jobs":[]})
        }).await?;
        return Ok(0);
    }
    let legacy_default = !document.jobs.is_empty();
    let (name, mut config) = document.workflow(workflow, profile)?;
    *output = output.with_workflow(&name);
    let total_defined_jobs = config.jobs.len();
    config.select(jobs)?;
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
                link_flags,
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
    let detached_run = match &cli.command {
        Some(Commands::Run {
            detached_run: Some(dir),
            ..
        }) => Some(dir),
        Some(Commands::Run { detach: true, .. }) => {
            return detach::start(&config_file, &name, profile, cli.config.as_deref(), output)
                .await;
        }
        _ => None,
    };
    let needed = secrets::needed(&config, &targets);
    if !needed.is_empty() {
        crate::reporting::protect_process();
    }
    let secrets = secrets::resolve(&needed, &settings.workdir, cancel.clone()).await?;
    let mut last_run = last::Recorder::new(&config_file);
    let mut identity = retention::RunIdentity::new();
    if let Some(dir) = detached_run {
        // The root invocation's run id is the one `run --detach` printed.
        identity = retention::RunIdentity::with_id(detach::run_id(dir)?);
        last_run = last_run.also(detach::last_record(dir));
    }
    let run = Run {
        calls: !calls.is_empty(),
        root: config,
        targets,
        profile: profile.map(str::to_owned),
        parallelism: cli.parallelism,
        fail_fast: fail_fast_override,
        history: history_cli,
        requested_jobs: jobs.clone(),
        log_dir: cli.log_dir.clone(),
        legacy_default,
        config_path: cli
            .config
            .as_ref()
            .map(|path| path.to_string_lossy().into_owned()),
        output: output.clone(),
        cancel,
        sprite_state: None,
        provenance: None,
        link_flags,
        links: Default::default(),
        last_run: Some(last_run),
        config_dir: config_file.parent().map(PathBuf::from),
        caches: Default::default(),
        cache_forwarded: None,
        secrets: Arc::new(secrets),
    };
    execute(run, name, identity).await
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
    /// A remote Sprite run's snapshot provenance, the `source` of the root
    /// and every child invocation; `None` locally.
    pub provenance: Option<reporting::Source>,
    /// `--ticket` and `--thread-url`, the first links collected.
    pub link_flags: links::Links,
    /// The run's links, collected by the first invocation that records and
    /// shared by every invocation; `None` when there are none. A remote
    /// Sprite run starts with the controller's.
    pub links: tokio::sync::OnceCell<Option<links::Links>>,
    /// Collects every invocation's summary for the last-run record; `None`
    /// on a Sprite, whose local run is the controller's.
    pub last_run: Option<last::Recorder>,
    /// The config file's directory, which names the local cache store
    /// outside Git; `None` on a Sprite, whose store is in its state directory.
    pub config_dir: Option<PathBuf>,
    /// The run's cache session, started by the first invocation with caches.
    pub caches: tokio::sync::OnceCell<Arc<cache::Session>>,
    /// A remote Sprite run's minted cache key from its controller.
    pub cache_forwarded: Option<cache::Forwarded>,
    /// The run's resolved secrets, read by every invocation.
    pub secrets: Arc<secrets::Secrets>,
}

/// The invocation that activated a child, and the call job that did it.
struct Parent {
    run_id: String,
    caller_job: String,
    scope: Arc<runner::Scope>,
    /// Ends the call job early, once the child's results are final.
    early: tokio::sync::oneshot::Sender<runner::ChildEnd>,
    /// Set when `early` fired.
    fired: Arc<std::sync::atomic::AtomicBool>,
    /// Where a child that already ended its call job reports a later error,
    /// for the parent invocation to fail with.
    late: Late,
}

/// Errors of children after their call jobs ended early.
type Late = Arc<std::sync::Mutex<Vec<String>>>;

/// How many child invocations of one invocation are still finishing (cache
/// saves, reporting, summary) after their call jobs ended. The invocation
/// waits for none to remain before its own reporting and summary.
type Children = Arc<watch::Sender<usize>>;

/// Counts one finishing child until dropped.
struct Child(Children);

impl Drop for Child {
    fn drop(&mut self) {
        self.0.send_modify(|n| *n -= 1);
    }
}

/// Ends a call job as soon as its child's results are final, while the
/// child's cache saves still run: saves never hold back dependents, and the
/// child keeps the snapshot that decided the call for its own summary.
struct Early {
    sender: Option<tokio::sync::oneshot::Sender<runner::ChildEnd>>,
    fired: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// The signal snapshot the call's status was decided from.
    signal: Option<Option<i32>>,
    target: String,
    run_id: String,
}

impl Early {
    fn fire(&mut self, results: &[runner::JobResult], signal: Option<i32>) {
        let Some(sender) = self.sender.take() else {
            return;
        };
        self.signal = Some(signal);
        if let Some(fired) = &self.fired {
            fired.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let outcome = history::outcome(results, signal.is_some());
        let finished = Finished {
            outcome,
            success: results
                .iter()
                .all(|r| matches!(r.status, Status::Passed | Status::Skipped)),
            signal,
            interrupted: interrupted(outcome, results.iter().map(|r| r.fail_fast_skip)),
        };
        let _ = sender.send(child_end(
            &self.target,
            self.run_id.clone(),
            Ok(Some(finished)),
        ));
    }
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
    let late = parent.as_ref().map(|p| (p.fired.clone(), p.late.clone()));
    let result = invocation(&run, &name, parent, identity).await;
    if let (Err(error), Some((fired, late))) = (&result, late) {
        // The call job already ended with the child's results; the error
        // still fails the run, through the parent, as it would at the root.
        if fired.load(std::sync::atomic::Ordering::SeqCst) {
            late.lock()
                .unwrap_or_else(|e| e.into_inner())
                .push(format!("workflow '{name}': {error:#}"));
        }
    }
    (id, result)
}

/// The activation callback for the call jobs of the invocation `run_id`,
/// whose finishing children `children` counts.
fn activator(run: Arc<Run>, run_id: String, children: Children, late: Late) -> runner::Activate {
    Arc::new(move |activation: runner::Activation| {
        let (early, ended) = tokio::sync::oneshot::channel();
        let parent = Parent {
            run_id: run_id.clone(),
            caller_job: activation.caller_job,
            scope: activation.scope,
            early,
            fired: Default::default(),
            late: late.clone(),
        };
        let (run, target) = (run.clone(), activation.target);
        children.send_modify(|n| *n += 1);
        let child = Child(children.clone());
        Box::pin(async move {
            let identity = retention::RunIdentity::new();
            let id = identity.id.clone();
            let name = target.clone();
            let mut task = tokio::spawn(async move {
                let _child = child;
                invoke(run, name, Some(parent), identity).await
            });
            let joined = move |joined: Result<(String, Result<Option<Finished>>), _>| match joined {
                Ok((run_id, finished)) => (run_id, finished),
                Err(error) => (id.clone(), Err(anyhow::anyhow!("{error}"))),
            };
            tokio::select! {
                done = &mut task => {
                    let (run_id, finished) = joined(done);
                    child_end(&target, run_id, finished)
                }
                Ok(end) = ended => {
                    // The child finishes on its own; its parent waits for it.
                    tokio::spawn(async move {
                        // Errors reach the parent through `late`; only a
                        // panicked child is left to report here.
                        if let Err(error) = task.await {
                            eprintln!("goodflows: warning: workflow '{target}': {error}");
                        }
                    });
                    end
                }
            }
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
    mut parent: Option<Parent>,
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
    // A remote invocation's source tree has no `.git`; it uses the
    // snapshot's provenance instead.
    let origin = match &run.sprite_state {
        Some(_) => reporting::Origin::Snapshot(run.provenance.as_ref()),
        None => reporting::Origin::Checkout(&settings.workdir),
    };
    let mut session = match &config.reporting {
        Some(reporting) => Some(
            reporting::Session::start(reporting, &identity.id, name, profile, &config.jobs, origin)
                .await,
        ),
        None => None,
    };
    // The source the run started from, for the run record and history.
    // Reuse the reporting session's capture rather than running git twice.
    let source = match session.as_ref().and_then(reporting::Session::source) {
        Some(source) => Some(source.clone()),
        None if settings.history => {
            origin
                .source(config.reporting.as_ref().map(|r| r.token_env.as_str()))
                .await
        }
        None => None,
    };
    // Only an invocation that builds a record needs the run's links; the
    // first one collects them for every invocation.
    let records = settings.history
        || session
            .as_ref()
            .is_some_and(reporting::Session::sends_run_record);
    let links = if records {
        run.links
            .get_or_init(|| async {
                let links = links::collect(
                    &run.link_flags,
                    &run.root.settings.ticket_prefixes,
                    source.as_ref().and_then(|s| s.branch.as_deref()),
                    &settings.workdir,
                    config.reporting.as_ref().map(|r| r.token_env.as_str()),
                    // The caller's own environment, never settings `env`.
                    &|name| std::env::var(name).ok(),
                    &mut |warning| eprintln!("goodflows: warning: {warning}"),
                )
                .await;
                Some(links).filter(|links| !links.is_empty())
            })
            .await
            .as_ref()
    } else {
        None
    };
    let remote = origin.remote();
    // Legacy root jobs log flat; every named workflow and every called
    // workflow, `default` included, logs under its own namespace.
    let log_base = run.log_dir.as_ref().map(|dir| {
        if parent.is_none() && run.legacy_default && name == "default" {
            dir.clone()
        } else {
            dir.join(config::path_component(name))
        }
    });
    let children: Children = Arc::new(watch::channel(0).0);
    let late: Late = Default::default();
    let early = Arc::new(std::sync::Mutex::new(Early {
        sender: parent
            .as_mut()
            .map(|p| std::mem::replace(&mut p.early, tokio::sync::oneshot::channel().0)),
        fired: parent.as_ref().map(|p| p.fired.clone()),
        signal: None,
        target: name.to_owned(),
        run_id: identity.id.clone(),
    }));
    let fire = {
        let (early, cancel) = (early.clone(), cancel.clone());
        move |results: &[runner::JobResult]| {
            let signal = *cancel.borrow();
            early
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .fire(results, signal);
        }
    };
    let mut options = Options {
        scope: scope.clone(),
        calls: run.calls.then(|| {
            activator(
                run.clone(),
                identity.id.clone(),
                children.clone(),
                late.clone(),
            )
        }),
        workdir: settings.workdir,
        log_dir: None,
        output: output.clone(),
        reporting: {
            let mut env = session
                .as_ref()
                .map(reporting::Session::runner_env)
                .unwrap_or_default();
            // The cache key is a credential too: no command or expression sees it.
            if let Some(cache) = &config.cache {
                env.scrub.push(cache.token_env.clone());
            }
            Arc::new(env)
        },
        environment: run.profile.clone(),
        settings_env: settings.env,
        settings_secrets: settings.secrets,
        secrets: run.secrets.clone(),
        sprite_state: run.sprite_state.clone(),
        caches: None,
        // Only cache saves still running end a call job early.
        done: std::sync::Mutex::new(Some(Box::new(fire.clone()))),
    };
    output.emit(json!({"type":"run_started", "parallelism":parallelism, "fail_fast":fail_fast, "jobs":config.jobs.iter().map(config::Job::instances).sum::<usize>(), "workdir":options.workdir})).await?;
    if let Some(session) = &session {
        session.announce(output).await;
    }
    if config.jobs.iter().any(|job| !job.caches.is_empty()) {
        // One session per run, so run-wide warnings and a disabled remote
        // store hold across called workflows.
        let caches = run
            .caches
            .get_or_init(|| {
                cache::Session::start(
                    cache::Start {
                        cache: run.root.cache.as_ref(),
                        reporting: run.root.reporting.as_ref(),
                        workdir: &options.workdir,
                        config_dir: run.config_dir.as_deref(),
                        sprite_state: run.sprite_state.as_deref(),
                        forwarded: run.cache_forwarded.as_ref(),
                    },
                    output,
                )
            })
            .await;
        options.caches = Some(Arc::new(cache::Invocation::new(
            caches.clone(),
            config::CacheConfig::effective(config.cache.as_ref()),
        )));
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
    // Children whose call jobs ended early still finish their saves and
    // summaries, which come before this invocation's.
    let mut finishing = children.subscribe();
    if *finishing.borrow() != 0 {
        fire(&results);
        let _ = finishing.wait_for(|n| *n == 0).await;
    }
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
    let early_signal = early.lock().unwrap_or_else(|e| e.into_inner()).signal;
    let signal = early_signal.unwrap_or(*cancel.borrow());
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
        links,
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
        // A remote history file keeps the snapshot's branch, and has none
        // when the snapshot has none: there is no fallback such as `main`.
        match history::write(&target, &fields, execute_ms, &results, remote) {
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
    if let Some(recorder) = &run.last_run {
        let invocation = run.calls.then(|| {
            let parent = parent
                .as_ref()
                .map(|p| (p.run_id.as_str(), p.caller_job.as_str()));
            (identity.id.as_str(), parent)
        });
        recorder.add(name, profile, invocation, signal.is_some(), summary.clone());
        // The root's summary is last; called workflows finished before it.
        if parent.is_none() {
            // The exit code `execute` gives, for `last` to exit with.
            let code = cancel
                .borrow()
                .map(|s| (128 + s) as u8)
                .unwrap_or(if success { 0 } else { 1 });
            if let Err(error) = recorder.write(run.config_path.as_deref(), code) {
                eprintln!("goodflows: warning: last run record: {error:#}");
            }
        }
    }
    output
        .emit_summary(summary, config_path, None, signal.is_some())
        .await?;
    // A child that failed after its call job had ended fails this
    // invocation like an error in its own reporting or summary.
    if let Some(error) = late.lock().unwrap_or_else(|e| e.into_inner()).first() {
        anyhow::bail!("{error}");
    }
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
        let source = reporting::Origin::Checkout(&root)
            .source(None)
            .await
            .unwrap();

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
            provenance: None,
            link_flags: Default::default(),
            links: Default::default(),
            last_run: None,
            config_dir: None,
            caches: Default::default(),
            cache_forwarded: None,
            secrets: Default::default(),
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

    #[test]
    fn history_arguments_are_checked_while_parsing() {
        let id = "fb7d2711-6b67-4f5a-9104-492032a9e1e5";
        let commit = "5a4a9715f1cd0a9795766bcd9294a77549b5b474";
        for bad in [
            &["history", "list", "--outcome", "green"][..],
            &["history", "list", "--limit", "0"],
            &["history", "list", "--limit", "201"],
            &["history", "list", "--commit", "HEAD"],
            &["history", "list", "--commit", &commit.to_uppercase()],
            &["history", "list", "--graph-sha256", commit],
            &["history", "list", "--workflow", " "],
            &["history", "list", "--event", &"e".repeat(65)],
            &["history", "list", "--cursor", "a+b="],
            &["history", "show", &id.to_uppercase()],
            &["history", "show", "fb7d27116b674f5a9104492032a9e1e5"],
        ] {
            let error = parse(bad).err().unwrap_or_else(|| panic!("{bad:?} parsed"));
            assert!(
                matches!(
                    error.kind(),
                    clap::error::ErrorKind::ValueValidation | clap::error::ErrorKind::InvalidValue
                ),
                "{bad:?}: {:?}",
                error.kind()
            );
            assert_eq!(error.exit_code(), 2, "{bad:?}");
        }
        let cli = parse(&[
            "history",
            "list",
            "--outcome",
            "failed",
            "--limit",
            "200",
            "--commit",
            commit,
            "--branch",
            "main",
        ])
        .unwrap();
        let Some(Commands::History {
            command: HistoryCommand::List(filters),
        }) = &cli.command
        else {
            panic!("not history list");
        };
        assert_eq!(filters.limit, Some(200));
        assert_eq!(filters.commit.as_deref(), Some(commit));
        assert!(cli.workflow().is_none());
        assert!(parse(&["history", "show", id]).is_ok());
    }

    #[test]
    fn run_flags_parse_before_and_after_run() {
        for args in [&["--jobs", "test"][..], &["run", "--jobs", "test"]] {
            let cli = parse(args).unwrap();
            assert_eq!(cli.jobs, ["test"], "{args:?}");
            assert!(cli.runs(), "{args:?}");
        }
    }

    #[test]
    fn cr_tasks_takes_one_number_before_or_after_done() {
        for args in [
            &["cr", "tasks", "-n", "42", "done", "5b1f"][..],
            &["cr", "tasks", "done", "5b1f", "-n", "42"],
            &["cr", "tasks", "-n", "42"],
        ] {
            assert!(parse(args).is_ok(), "{args:?}");
        }
        for args in [
            &["cr", "tasks", "-n", "1", "done", "5b1f", "-n", "2"][..],
            &["cr", "tasks", "-n1", "done", "5b1f", "--number=2"],
            &[
                "cr", "tasks", "--state", "done", "done", "5b1f", "-n", "1", "-n", "2",
            ],
        ] {
            let error = parse(args).err().unwrap_or_else(|| panic!("{args:?}"));
            assert_eq!(
                error.kind(),
                clap::error::ErrorKind::ArgumentConflict,
                "{args:?}"
            );
        }
    }

    #[test]
    fn workflow_arguments_are_described() {
        let parser = Cli::parser();
        for name in ["run", "validate", "list"] {
            let command = parser.find_subcommand(name).unwrap();
            let workflow = command
                .get_arguments()
                .find(|arg| arg.get_id() == "workflow")
                .unwrap();
            assert!(workflow.is_positional(), "{name}");
            assert!(workflow.get_help().is_some(), "{name}");
        }
    }

    /// The path of every visible command below `command`, which is at `path`.
    fn command_paths(command: &clap::Command, path: Vec<String>, paths: &mut Vec<Vec<String>>) {
        paths.push(path.clone());
        for sub in command.get_subcommands() {
            if !sub.is_hide_set() && sub.get_name() != "help" {
                let mut path = path.clone();
                path.push(sub.get_name().to_owned());
                command_paths(sub, path, paths);
            }
        }
    }

    #[test]
    fn every_command_help_ends_with_examples_that_parse() {
        let run_id = "fb7d2711-6b67-4f5a-9104-492032a9e1e5";
        let mut paths = Vec::new();
        command_paths(&Cli::parser(), Vec::new(), &mut paths);
        assert!(paths.len() > 10, "{paths:?}");
        for command in [&["skills", "install"][..], &["mcp"]] {
            assert!(paths.iter().any(|path| path == command), "{command:?}");
        }
        for path in paths {
            let args = std::iter::once("goodflows")
                .chain(path.iter().map(String::as_str))
                .chain(["--help"]);
            let error = Cli::parser().try_get_matches_from(args).err().unwrap();
            assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
            let help = error.render().to_string();
            let (_, examples) = help
                .rsplit_once("\nExamples:\n")
                .unwrap_or_else(|| panic!("{path:?} has no examples:\n{help}"));
            let lines: Vec<_> = examples.lines().collect();
            assert!((3..=4).contains(&lines.len()), "{path:?}:\n{help}");
            for line in lines {
                let (example, comment) = line.split_once(" # ").unwrap_or((line, ""));
                assert!(!comment.trim().is_empty(), "{line}");
                let example = example.trim();
                let example = example
                    .strip_prefix("id=$(")
                    .and_then(|e| e.strip_suffix(')'))
                    .unwrap_or(example);
                let words: Vec<_> = example
                    .split_whitespace()
                    .map(|word| match word {
                        "\"$id\"" | "<RUN_ID>" | "<UUID>" => run_id,
                        word => word,
                    })
                    .collect();
                assert_eq!(words.first(), Some(&"goodflows"), "{line}");
                if let Err(error) = parse(&words[1..]) {
                    panic!("{path:?}: {line}: {error}");
                }
            }
        }
    }

    #[test]
    fn commands_that_run_no_jobs_take_a_plain_json_flag() {
        for path in [
            &["init"][..],
            &["skills", "install"],
            &["last"],
            &["status"],
        ] {
            let args = std::iter::once("goodflows")
                .chain(path.iter().copied())
                .chain(["--help"]);
            let error = Cli::parser().try_get_matches_from(args).err().unwrap();
            let help = error.render().to_string();
            assert!(help.contains("      --json\n"), "{path:?}:\n{help}");
            // `last`'s examples may name the summary events it prints.
            let (options, _) = help.split_once("\nExamples:").unwrap();
            assert!(!options.contains("summary"), "{path:?}:\n{help}");
        }
        let run_id = "fb7d2711-6b67-4f5a-9104-492032a9e1e5";
        for args in [
            &["init"][..],
            &["skills", "install"],
            &["last"],
            &["status", run_id],
        ] {
            let json = parse(&[args, &["--json"]].concat()).unwrap().json;
            assert_eq!(json, Some(output::Json::All), "{args:?}");
            let error = parse(&[args, &["--json=summary"]].concat()).err();
            assert!(error.is_some(), "{args:?}");
        }
        // A run, and `wait`, which replays one, still choose their events.
        assert_eq!(
            parse(&["wait", run_id, "--json=summary"]).unwrap().json,
            Some(output::Json::Summary)
        );
        let help = Cli::parser()
            .try_get_matches_from(["goodflows", "run", "--help"])
            .err()
            .unwrap()
            .render()
            .to_string();
        assert!(help.contains("--json=summary"), "{help}");
    }

    #[test]
    fn history_commands_name_a_rejected_run_only_flag() {
        for (args, flag) in [
            (&["history", "list", "--env", "ci"][..], "--env"),
            (&["history", "list", "--no-history"], "--no-history"),
            (&["--jobs", "a", "history", "list"], "--jobs"),
            (
                &["history", "list", "--history-bytes", "1"],
                "--history-bytes",
            ),
            (
                &[
                    "history",
                    "show",
                    "fb7d2711-6b67-4f5a-9104-492032a9e1e5",
                    "--ticket",
                    "X",
                ],
                "--ticket",
            ),
            (
                &[
                    "--thread-url",
                    "https://h/x",
                    "history",
                    "show",
                    "fb7d2711-6b67-4f5a-9104-492032a9e1e5",
                ],
                "--thread-url",
            ),
            (&["last", "--ticket", "X"], "--ticket"),
        ] {
            assert_eq!(parse(args).unwrap().run_only_flag(), Some(flag), "{args:?}");
        }
        // `history list`'s own filters take the value; clap also copies it
        // to the run flag of the same id.
        let cli = parse(&["history", "list", "--ticket", "GOO-1"]).unwrap();
        let Some(Commands::History {
            command: HistoryCommand::List(filters),
        }) = &cli.command
        else {
            panic!("not history list");
        };
        assert_eq!(filters.ticket.as_deref(), Some("GOO-1"));
        assert_eq!(cli.ticket, ["GOO-1"]);
        // Both flags are values, each checked by the link rules.
        assert!(parse(&["--ticket", "a,b"]).is_err());
        assert!(parse(&["--thread-url", "https://h/x?token=1"]).is_err());
        let cli = parse(&[
            "--ticket",
            "A",
            "--ticket",
            "B",
            "--thread-url",
            "HTTPS://H/x",
        ])
        .unwrap();
        assert_eq!(cli.ticket, ["A", "B"]);
        assert_eq!(cli.thread_url, ["https://h/x"]);
        assert_eq!(
            parse(&["history", "list", "--json"])
                .unwrap()
                .run_only_flag(),
            None
        );
    }
}
