//! The opt-in, immutable per-run history records, and the run-level fields
//! they share with the `summary` event. Names, locks and retention live in
//! `retention`.

use crate::config::{Condition, Job, Partition, Partitions};
use crate::fsutil::Dir;
use crate::reporting::Source;
use crate::runner::{JobResult, Scheduling, Status};
use anyhow::{Context, Result};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// History records keep at most this many bytes of a command or error.
const MAX_TEXT_BYTES: usize = 1024;

/// The run outcome shared by the history file, `summary.outcome`, the report
/// header and the exit code. `signalled` is the one signal snapshot.
pub fn outcome(results: &[JobResult], signalled: bool) -> &'static str {
    let has = |status: Status| results.iter().any(|r| r.status == status);
    if has(Status::Failed) || has(Status::TimedOut) {
        "failed"
    } else if has(Status::Cancelled) || signalled {
        "cancelled"
    } else {
        "passed"
    }
}

/// The machine the run started on, sampled once before jobs run.
#[derive(Debug, Clone, Serialize)]
pub struct Host {
    /// `std::env::consts::OS`, for example `linux` or `macos`.
    pub os: &'static str,
    /// `std::env::consts::ARCH`, for example `x86_64` or `aarch64`.
    pub arch: &'static str,
    pub logical_cpus: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_total_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mem_available_bytes: Option<u64>,
}

impl Host {
    pub fn current() -> Self {
        let (mem_total_bytes, mem_available_bytes) = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|text| parse_meminfo(&text))
            .unzip();
        Self {
            os: std::env::consts::OS,
            arch: std::env::consts::ARCH,
            logical_cpus: std::thread::available_parallelism()
                .map(usize::from)
                .unwrap_or(1),
            mem_total_bytes,
            mem_available_bytes,
        }
    }
}

/// Linux `MemTotal` and `MemAvailable` in bytes from `/proc/meminfo` text;
/// `None` unless both are present.
fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(name)?.strip_prefix(':'))
            .and_then(|rest| rest.trim().strip_suffix("kB"))
            .and_then(|kb| kb.trim().parse::<u64>().ok())
            .map(|kb| kb * 1024)
    };
    Some((field("MemTotal")?, field("MemAvailable")?))
}

#[derive(Serialize)]
struct GraphDocument<'a> {
    jobs: Vec<GraphJob<'a>>,
}

#[derive(Serialize)]
struct GraphJob<'a> {
    name: &'a str,
    command: Option<&'a str>,
    depends_on: &'a [String],
    partitions: Option<Partitions>,
    timeout_secs: Option<u64>,
    shell: bool,
    condition: Option<GraphCondition<'a>>,
    steps: Vec<GraphStep<'a>>,
}

#[derive(Serialize)]
struct GraphStep<'a> {
    id: &'a str,
    command: &'a str,
    shell: Option<bool>,
    condition: Option<GraphCondition<'a>>,
}

#[derive(Serialize)]
struct GraphCondition<'a> {
    kind: &'static str,
    text: &'a str,
}

impl<'a> GraphCondition<'a> {
    fn of(condition: Option<&'a Condition>) -> Option<Self> {
        condition.map(|condition| match condition {
            Condition::Expression { expression } => Self {
                kind: "expression",
                text: expression,
            },
            // Legacy and table shell conditions hash identically.
            Condition::LegacyShell(shell) | Condition::Shell { shell } => Self {
                kind: "shell",
                text: shell,
            },
        })
    }
}

/// Lowercase hex SHA-256 of the canonical selected-job document. Covers only
/// the graph shape and commands, never env values or paths.
pub fn graph_sha256(jobs: &[Job]) -> String {
    let document = GraphDocument {
        jobs: jobs
            .iter()
            .map(|job| GraphJob {
                name: &job.name,
                command: job.command.as_deref(),
                depends_on: &job.depends_on,
                partitions: job.partitions,
                timeout_secs: job.timeout_secs,
                shell: job.shell,
                condition: GraphCondition::of(job.condition.as_ref()),
                steps: job
                    .steps
                    .iter()
                    .map(|step| GraphStep {
                        id: &step.id,
                        command: &step.command,
                        shell: step.shell,
                        condition: GraphCondition::of(step.condition.as_ref()),
                    })
                    .collect(),
            })
            .collect(),
    };
    let bytes = serde_json::to_vec(&document).expect("graph document serializes");
    format!("{:x}", Sha256::digest(bytes))
}

/// Run-level fields shared by the history file and the `summary` event.
#[derive(Debug, Clone, Serialize)]
pub struct RunFields {
    pub run_id: String,
    pub started_at: String,
    pub outcome: &'static str,
    pub parallelism: usize,
    pub graph_sha256: String,
    pub max_in_flight: usize,
    pub host: Host,
}

/// The history file. Key order is field order; the allowlist below is the
/// whole contents, so command output and job outputs can never leak in.
#[derive(Serialize)]
struct Record<'a> {
    schema_version: u32,
    workflow: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<&'a str>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    requested_jobs: &'a [String],
    run_id: &'a str,
    started_at: &'a str,
    outcome: &'a str,
    duration_ms: u64,
    parallelism: usize,
    graph_sha256: &'a str,
    max_in_flight: usize,
    host: &'a Host,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<RecordSource<'a>>,
    results: Vec<Instance<'a>>,
}

/// The commit the run started from and whether tracked files were modified.
/// Branch names and remotes are never recorded.
#[derive(Serialize)]
struct RecordSource<'a> {
    commit: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty: Option<bool>,
}

impl<'a> RecordSource<'a> {
    /// `None` outside a git checkout or when git failed.
    fn of(source: Option<&'a Source>) -> Option<Self> {
        let source = source?;
        Some(Self {
            commit: source.commit.as_deref()?,
            dirty: source.dirty,
        })
    }
}

#[derive(Serialize)]
struct Instance<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition: Option<Partition>,
    status: Status,
    exit_code: Option<i32>,
    command: Option<String>,
    error: Option<String>,
    duration_ms: u128,
    /// `started`, `blocked_ms`, `queued_ms` and `commands`, serialized by the
    /// same struct as each `summary.results` entry.
    #[serde(flatten)]
    scheduling: Scheduling<'a>,
}

/// Cut to at most `MAX_TEXT_BYTES` on a character boundary.
fn truncate(text: &str) -> String {
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text[..end].to_owned()
}

/// Replace goodflows' own resolved paths in an error with placeholders,
/// wherever they begin a whole path in the string: the per-run log
/// directory, together with any path beneath it (so no log filename
/// survives), becomes `<log>`, and the workdir becomes `<workdir>` (a path
/// below it keeps its tail). The log directory goes first because it may sit
/// inside the workdir. Only whole paths match: `/w` never matches inside
/// `/w2/x` or `/x/w`.
fn redact(error: &str, workdir: &Path, log_dir: Option<&Path>) -> String {
    let mut text = error.to_owned();
    if let Some(dir) = log_dir {
        text = replace_path(&text, &dir.to_string_lossy(), "<log>", true);
    }
    replace_path(&text, &workdir.to_string_lossy(), "<workdir>", false)
}

/// Whether `c` can continue a path component, so a match ending before it is
/// only a prefix of a longer name.
fn name_char(c: char) -> bool {
    c.is_alphanumeric() || matches!(c, '_' | '-' | '.' | '~' | '+' | '@' | '%' | '=')
}

/// Whether `rest`, the text right after a match, continues the matched name.
/// A trailing `.` (sentence punctuation) does not.
fn continues_name(rest: &str) -> bool {
    let mut chars = rest.chars();
    match chars.next() {
        Some('.') => chars.next().is_some_and(name_char),
        Some(c) => name_char(c),
        None => false,
    }
}

/// Replace each whole-path occurrence of `path` with `placeholder`. With
/// `below`, a following `/component...` is swallowed too.
fn replace_path(text: &str, path: &str, placeholder: &str, below: bool) -> String {
    let path = path.trim_end_matches('/');
    if path.is_empty() {
        return text.to_owned();
    }
    let mut out = String::with_capacity(text.len());
    let mut copied = 0;
    for (start, _) in text.match_indices(path) {
        if start < copied {
            continue;
        }
        let end = start + path.len();
        let starts_path = text[..start]
            .chars()
            .next_back()
            .is_none_or(|c| c != '/' && !name_char(c));
        if !starts_path || continues_name(&text[end..]) {
            continue;
        }
        let mut stop = end;
        if below && text[end..].starts_with('/') {
            stop += text[end..]
                .find(|c: char| c != '/' && !name_char(c))
                .unwrap_or(text.len() - end);
            // Leave sentence punctuation after the path in place.
            while stop > end + 1 && text[..stop].ends_with('.') {
                stop -= 1;
            }
        }
        out.push_str(&text[copied..start]);
        out.push_str(placeholder);
        copied = stop;
    }
    out.push_str(&text[copied..]);
    out
}

/// Everything a history write needs beyond the run fields and results.
pub struct HistoryTarget<'a> {
    /// The resolved history directory.
    pub dir: &'a Path,
    /// The workflow name; legacy top-level jobs use `default`.
    pub workflow: &'a str,
    /// The selected `--env` profile, if any.
    pub environment: Option<&'a str>,
    /// `--jobs` as given; empty when every job was selected.
    pub requested_jobs: &'a [String],
    /// Source context captured before jobs ran.
    pub source: Option<&'a Source>,
    pub stem: &'a str,
    pub runs: u64,
    pub bytes: u64,
    pub workdir: &'a Path,
    pub log_dir: Option<&'a Path>,
}

/// A written history file, and the error of the prune that followed, if any.
/// A prune failure never hides the file it kept.
pub struct Written {
    pub path: PathBuf,
    pub pruned: Option<anyhow::Error>,
}

/// Write the history file and prune its workflow directory. `duration_ms` is
/// the time spent in `runner::execute`. Any error means nothing was written
/// and the caller must omit `history_file`.
pub fn write(
    target: &HistoryTarget,
    fields: &RunFields,
    duration_ms: u64,
    results: &[JobResult],
) -> Result<Written> {
    let record = Record {
        schema_version: 1,
        workflow: target.workflow,
        environment: target.environment,
        requested_jobs: target.requested_jobs,
        run_id: &fields.run_id,
        started_at: &fields.started_at,
        outcome: fields.outcome,
        duration_ms,
        parallelism: fields.parallelism,
        graph_sha256: &fields.graph_sha256,
        max_in_flight: fields.max_in_flight,
        host: &fields.host,
        source: RecordSource::of(target.source),
        results: results
            .iter()
            .map(|result| Instance {
                name: &result.name,
                partition: result.partition,
                status: result.status,
                exit_code: result.exit_code,
                command: result.command.as_deref().map(truncate),
                error: result
                    .error
                    .as_deref()
                    .map(|error| truncate(&redact(error, target.workdir, target.log_dir))),
                duration_ms: result.duration_ms,
                scheduling: result.scheduling(),
            })
            .collect(),
    };
    let mut bytes = serde_json::to_vec(&record)?;
    bytes.push(b'\n');
    crate::fsutil::dir_builder(true)
        .create(target.dir)
        .with_context(|| format!("history directory {}", target.dir.display()))?;
    // Every later step works relative to these descriptors, so swapping a
    // path component for a symlink mid-write cannot redirect it.
    let base = Dir::open(target.dir)
        .map_err(refuse_symlink)
        .with_context(|| format!("history directory {}", target.dir.display()))?;
    let component = crate::config::path_component(target.workflow);
    let path = target.dir.join(&component);
    let dir = base
        .create_dir(OsStr::new(&component))
        .map_err(refuse_symlink)
        .with_context(|| format!("history directory {}", path.display()))?;
    let name = format!("{}.json", target.stem);
    let path = path.join(&name);
    dir.write_new(OsStr::new(&name), &bytes)
        .with_context(|| format!("failed to write history file {}", path.display()))?;
    let pruned = crate::retention::prune_history(&dir, target.stem, target.runs, target.bytes)
        .with_context(|| {
            format!(
                "failed to prune history in {}",
                target.dir.join(&component).display()
            )
        })
        .err();
    Ok(Written { path, pruned })
}

/// Name the `ELOOP` from opening a symlink with `O_NOFOLLOW`.
fn refuse_symlink(error: std::io::Error) -> anyhow::Error {
    if error.raw_os_error() == Some(libc::ELOOP) {
        anyhow::anyhow!("refusing a symlink")
    } else {
        error.into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_cuts_on_a_character_boundary() {
        let text = format!("{}é", "a".repeat(1023));
        assert_eq!(truncate(&text), "a".repeat(1023));
        assert_eq!(truncate("short"), "short");
        assert_eq!(truncate(&"x".repeat(2000)).len(), 1024);
    }

    #[test]
    fn redact_replaces_log_dir_before_workdir() {
        let error = "failed to create log /w/logs/run/001-a.stdout.log: denied in /w/sub";
        assert_eq!(
            redact(error, Path::new("/w"), Some(Path::new("/w/logs/run"))),
            "failed to create log <log>: denied in <workdir>/sub"
        );
        assert_eq!(
            redact("exited with 1", Path::new("/w"), None),
            "exited with 1"
        );
    }

    #[test]
    fn redact_drops_every_log_filename() {
        let log = Some(Path::new("/w/logs/run"));
        let redact = |error: &str| redact(error, Path::new("/w"), log);
        assert_eq!(
            redact("open '/w/logs/run/002-b.p01.stderr.log' failed"),
            "open '<log>' failed"
        );
        assert_eq!(
            redact("log /w/logs/run/sub/001-a.stdout.log."),
            "log <log>."
        );
        assert_eq!(redact("in /w/logs/run"), "in <log>");
        assert_eq!(redact("in /w/logs/run/ now"), "in <log> now");
        assert_eq!(
            redact("a /w/logs/run/x.log and /w/logs/run/y.log"),
            "a <log> and <log>"
        );
    }

    #[test]
    fn redact_matches_whole_paths_only() {
        let workdir = Path::new("/home/u/proj");
        let redact = |error: &str| redact(error, workdir, None);
        // A sibling whose name extends the workdir is not the workdir.
        assert_eq!(redact("/home/u/proj2/x"), "/home/u/proj2/x");
        assert_eq!(redact("/home/u/proj.bak/x"), "/home/u/proj.bak/x");
        assert_eq!(redact("/home/u/proj-old"), "/home/u/proj-old");
        // Nor is the same suffix under another directory.
        assert_eq!(redact("/x/home/u/proj/y"), "/x/home/u/proj/y");
        // The workdir itself, followed by a separator or non-path text.
        assert_eq!(redact("/home/u/proj/x"), "<workdir>/x");
        assert_eq!(redact("/home/u/proj"), "<workdir>");
        assert_eq!(redact("in /home/u/proj."), "in <workdir>.");
        assert_eq!(redact("'/home/u/proj': denied"), "'<workdir>': denied");
        assert_eq!(redact("\"/home/u/proj\" x"), "\"<workdir>\" x");
        assert_eq!(
            redact("cd /home/u/proj (os error 2)"),
            "cd <workdir> (os error 2)"
        );
        // The log directory needs the same boundary.
        assert_eq!(
            super::redact(
                "/w/logs/run2/x",
                Path::new("/q"),
                Some(Path::new("/w/logs/run"))
            ),
            "/w/logs/run2/x"
        );
    }

    fn hash(source: &str) -> String {
        let mut config: crate::config::Config = toml::from_str(source).unwrap();
        config.validate().unwrap();
        graph_sha256(&config.jobs)
    }

    const GRAPH: &str = "[[jobs]]\nname = 'build'\ncommand = 'make'\nenv = { A = '1' }\n\
        [[jobs]]\nname = 'test'\ndepends_on = ['build']\npartitions = 2\n\
        [[jobs.steps]]\nid = 'run'\ncommand = 'make test'\ncondition = { shell = 'test -f x' }\n";

    #[test]
    fn graph_hash_is_stable_and_ignores_env_values() {
        let base = hash(GRAPH);
        assert_eq!(base.len(), 64);
        assert!(base
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()));
        assert_eq!(hash(GRAPH), base);
        assert_eq!(hash(&GRAPH.replace("A = '1'", "A = '2'")), base);
        assert_eq!(hash(&format!("[settings.env]\nB = 'x'\n{GRAPH}")), base);
        assert_ne!(hash(&GRAPH.replace("'make'", "'make all'")), base);
        assert_ne!(hash(&GRAPH.replace("test -f x", "test -f y")), base);
        assert_ne!(
            hash(&GRAPH.replace("partitions = 2", "partitions = 3")),
            base
        );
    }

    #[test]
    fn legacy_and_table_shell_conditions_hash_the_same() {
        let table =
            "[[jobs]]\nname = 'a'\ncommand = 'true'\ncondition = { shell = 'env.A == \"1\"' }\n";
        let legacy = "[[jobs]]\nname = 'a'\ncommand = 'true'\ncondition = 'env.A == \"1\"'\n";
        let expression = "[[jobs]]\nname = 'a'\ncommand = 'true'\ncondition = { expression = 'env.A == \"1\"' }\n";
        assert_eq!(hash(table), hash(legacy));
        assert_ne!(hash(table), hash(expression));
    }

    #[test]
    fn graph_document_has_the_canonical_shape() {
        let mut config: crate::config::Config = toml::from_str(GRAPH).unwrap();
        config.validate().unwrap();
        let document = GraphDocument {
            jobs: vec![GraphJob {
                name: "a",
                command: None,
                depends_on: &[],
                partitions: config.jobs[1].partitions,
                timeout_secs: None,
                shell: true,
                condition: None,
                steps: vec![GraphStep {
                    id: "s",
                    command: "c",
                    shell: None,
                    condition: Some(GraphCondition {
                        kind: "shell",
                        text: "t",
                    }),
                }],
            }],
        };
        assert_eq!(
            serde_json::to_string(&document).unwrap(),
            r#"{"jobs":[{"name":"a","command":null,"depends_on":[],"partitions":{"count":2,"strategy":"index"},"timeout_secs":null,"shell":true,"condition":null,"steps":[{"id":"s","command":"c","shell":null,"condition":{"kind":"shell","text":"t"}}]}]}"#
        );
    }

    #[test]
    fn graph_hash_covers_every_graph_field() {
        let base = hash(GRAPH);
        for (from, to) in [
            ("depends_on = ['build']", "depends_on = []"),
            ("partitions = 2\n", "partitions = 2\ntimeout_secs = 5\n"),
            ("command = 'make'\n", "command = 'make'\nshell = false\n"),
            ("id = 'run'", "id = 'go'"),
            (
                "command = 'make test'\n",
                "command = 'make test'\nshell = false\n",
            ),
        ] {
            let changed = GRAPH.replace(from, to);
            assert_ne!(changed, GRAPH, "{from}");
            assert_ne!(hash(&changed), base, "{from} -> {to}");
        }
    }

    #[test]
    fn graph_hash_is_pinned() {
        // The canonical document is a stable contract: a change to its shape
        // changes every hash and breaks comparisons with committed history.
        assert_eq!(
            hash(GRAPH),
            "485cdd8ba913129dab6e9123ddc330f4ca8c20a1836a7a7e167f9a75e81364ef"
        );
    }

    #[test]
    fn meminfo_needs_both_fields() {
        let text = "MemTotal:       16384 kB\nMemFree:  1 kB\nMemAvailable:    8192 kB\n";
        assert_eq!(parse_meminfo(text), Some((16384 * 1024, 8192 * 1024)));
        assert_eq!(
            parse_meminfo("MemTotal:       16384 kB\nMemFree:  1 kB\n"),
            None
        );
        assert_eq!(parse_meminfo("MemAvailable: 1 kB\n"), None);
        assert_eq!(parse_meminfo("MemTotal: lots\nMemAvailable: 1 kB\n"), None);
        assert_eq!(parse_meminfo(""), None);
    }

    #[test]
    fn host_reports_the_platform() {
        let host = Host::current();
        assert_eq!(host.os, std::env::consts::OS);
        assert_eq!(host.arch, std::env::consts::ARCH);
        assert!(host.logical_cpus >= 1);
        if cfg!(target_os = "linux") {
            let total = host.mem_total_bytes.unwrap();
            let available = host.mem_available_bytes.unwrap();
            assert!(total >= available && available > 0, "{host:?}");
        }
    }
}
