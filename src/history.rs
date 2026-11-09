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
use std::collections::HashMap;
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

/// The canonical selected-job document behind `graph_sha256`, which an upload
/// sends as `graph` after `truncated`.
#[derive(Clone, Serialize)]
pub struct GraphDocument<'a> {
    jobs: Vec<GraphJob<'a>>,
}

impl GraphDocument<'_> {
    /// The upload copy: every job command, step command and condition text
    /// cut to `MAX_TEXT_BYTES` on a character boundary. Hash before cutting.
    pub fn truncated(&self) -> Self {
        let mut document = self.clone();
        for job in &mut document.jobs {
            job.command = job.command.map(cut);
            GraphCondition::cut(&mut job.condition);
            for step in &mut job.steps {
                step.command = cut(step.command);
                GraphCondition::cut(&mut step.condition);
            }
        }
        document
    }
}

#[derive(Clone, Serialize)]
struct GraphJob<'a> {
    name: &'a str,
    command: Option<&'a str>,
    depends_on: &'a [String],
    partitions: Option<Partitions>,
    timeout_secs: Option<u64>,
    shell: bool,
    condition: Option<GraphCondition<'a>>,
    steps: Vec<GraphStep<'a>>,
    /// Workflow-call jobs only, after every existing field, so documents
    /// without calls keep their bytes and hashes.
    #[serde(skip_serializing_if = "Option::is_none")]
    call: Option<GraphCall<'a>>,
}

/// A call's target and the `graph_sha256` of that workflow's complete jobs
/// under the selected profile, so each workflow is hashed once however many
/// call paths reach it.
#[derive(Clone, Serialize)]
struct GraphCall<'a> {
    workflow: &'a str,
    graph_sha256: String,
}

#[derive(Clone, Serialize)]
struct GraphStep<'a> {
    id: &'a str,
    command: &'a str,
    shell: Option<bool>,
    condition: Option<GraphCondition<'a>>,
}

#[derive(Clone, Serialize)]
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

    fn cut(condition: &mut Option<Self>) {
        if let Some(condition) = condition {
            condition.text = cut(condition.text);
        }
    }
}

/// Lowercase hex SHA-256 of the canonical selected-job document, and that
/// document. Covers only the graph shape and commands, never env values,
/// paths or run identities. `targets` resolves a called workflow to its
/// complete jobs under the selected profile; it is never asked for a workflow
/// nothing calls.
pub fn graph_sha256<'a>(
    jobs: &'a [Job],
    targets: &dyn Fn(&str) -> &'a [Job],
) -> (String, GraphDocument<'a>) {
    document_sha256(jobs, targets, &mut HashMap::new())
}

/// `graph_sha256`, reusing the hashes of workflows already hashed.
fn document_sha256<'a>(
    jobs: &'a [Job],
    targets: &dyn Fn(&str) -> &'a [Job],
    hashed: &mut HashMap<&'a str, String>,
) -> (String, GraphDocument<'a>) {
    let document = GraphDocument {
        jobs: graph_jobs(jobs, targets, hashed),
    };
    let bytes = serde_json::to_vec(&document).expect("graph document serializes");
    (format!("{:x}", Sha256::digest(bytes)), document)
}

fn graph_jobs<'a>(
    jobs: &'a [Job],
    targets: &dyn Fn(&str) -> &'a [Job],
    hashed: &mut HashMap<&'a str, String>,
) -> Vec<GraphJob<'a>> {
    jobs.iter()
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
            // A call's condition never removes its target from the graph.
            call: job.workflow.as_deref().map(|workflow| {
                let graph_sha256 = match hashed.get(workflow) {
                    Some(hash) => hash.clone(),
                    None => {
                        let (hash, _) = document_sha256(targets(workflow), targets, hashed);
                        hashed.insert(workflow, hash.clone());
                        hash
                    }
                };
                GraphCall {
                    workflow,
                    graph_sha256,
                }
            }),
        })
        .collect()
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

/// The history file, and with `Upload` keys the uploaded run record. Key
/// order is field order; the allowlist below is the whole contents, so
/// command output and job outputs can never leak in.
#[derive(Serialize)]
pub struct Record<'a> {
    schema_version: u32,
    workflow: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    environment: Option<&'a str>,
    #[serde(skip_serializing_if = "<[_]>::is_empty")]
    requested_jobs: &'a [String],
    run_id: &'a str,
    /// A child invocation's parent run and the call job that started it.
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_run_id: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    caller_job: Option<&'a str>,
    started_at: &'a str,
    outcome: &'a str,
    duration_ms: u64,
    parallelism: usize,
    graph_sha256: &'a str,
    max_in_flight: usize,
    host: &'a Host,
    #[serde(skip_serializing_if = "Option::is_none")]
    source: Option<RecordSource<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ci: Option<&'a serde_json::Value>,
    results: Vec<Instance<'a>>,
    /// Last, because it is the largest key.
    #[serde(skip_serializing_if = "Option::is_none")]
    graph: Option<GraphDocument<'a>>,
}

/// The upload-only record keys. Each is omitted when absent, so the local
/// file, built with `Upload::default()`, never carries them.
#[derive(Default)]
pub struct Upload<'a> {
    /// Add the captured source's branch, if any, after `dirty`.
    pub branch: bool,
    pub ci: Option<&'a serde_json::Value>,
    /// The hashed graph document; the record carries its truncated copy.
    pub graph: Option<&'a GraphDocument<'a>>,
}

/// The commit the run started from and whether tracked files were modified.
/// Remotes are never recorded, and the branch only in an upload.
#[derive(Serialize)]
struct RecordSource<'a> {
    commit: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    branch: Option<&'a str>,
}

impl<'a> RecordSource<'a> {
    /// `None` outside a git checkout or when git failed.
    fn of(source: Option<&'a Source>, branch: bool) -> Option<Self> {
        let source = source?;
        Some(Self {
            commit: source.commit.as_deref()?,
            dirty: source.dirty,
            branch: source.branch.as_deref().filter(|_| branch),
        })
    }
}

#[derive(Serialize)]
struct Instance<'a> {
    name: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    partition: Option<Partition>,
    /// A workflow-call job's target and, once activated, its child run.
    #[serde(skip_serializing_if = "Option::is_none")]
    workflow: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    child_run_id: Option<&'a str>,
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
    cut(text).to_owned()
}

/// `truncate` without the copy.
fn cut(text: &str) -> &str {
    let mut end = text.len().min(MAX_TEXT_BYTES);
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
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
    /// `--jobs` as given; empty when every job was selected and for a child
    /// invocation, which always runs its complete workflow.
    pub requested_jobs: &'a [String],
    /// A child invocation's parent run id and caller job.
    pub parent: Option<(&'a str, &'a str)>,
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

/// The record for a run: the history file's contents with `Upload::default()`,
/// or the uploaded document with the upload keys. `duration_ms` is the time
/// spent in `runner::execute`.
pub fn record<'a>(
    target: &HistoryTarget<'a>,
    fields: &'a RunFields,
    duration_ms: u64,
    results: &'a [JobResult],
    upload: Upload<'a>,
) -> Record<'a> {
    Record {
        schema_version: 1,
        workflow: target.workflow,
        environment: target.environment,
        requested_jobs: target.requested_jobs,
        run_id: &fields.run_id,
        parent_run_id: target.parent.map(|(run_id, _)| run_id),
        caller_job: target.parent.map(|(_, job)| job),
        started_at: &fields.started_at,
        outcome: fields.outcome,
        duration_ms,
        parallelism: fields.parallelism,
        graph_sha256: &fields.graph_sha256,
        max_in_flight: fields.max_in_flight,
        host: &fields.host,
        source: RecordSource::of(target.source, upload.branch),
        ci: upload.ci,
        results: results
            .iter()
            .map(|result| Instance {
                name: &result.name,
                partition: result.partition,
                workflow: result.workflow.as_deref(),
                child_run_id: result.child_run_id.as_deref(),
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
        graph: upload.graph.map(GraphDocument::truncated),
    }
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
    let record = record(target, fields, duration_ms, results, Upload::default());
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
        graph_sha256(&config.jobs, &|_| unreachable!("no workflow calls")).0
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
                call: None,
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
    fn a_call_appends_its_target_hash_after_the_existing_fields() {
        let mut config: crate::config::Config = toml::from_str(
            "[[workflows.ci.jobs]]\nname = 'call'\nworkflow = 'suite'\ndepends_on = ['prep']\n\
             condition = { expression = 'true' }\n\
             [[workflows.ci.jobs]]\nname = 'prep'\ncommand = 'make'\n\
             [[workflows.suite.jobs]]\nname = 't'\ncommand = 'true'\n",
        )
        .unwrap();
        config.validate().unwrap();
        let (_, ci) = config.workflow(Some("ci"), None).unwrap();
        let (_, suite) = config.workflow(Some("suite"), None).unwrap();
        // The target's hash is its own graph hash, which is what a child
        // invocation of it reports.
        let suite_hash = graph_sha256(&suite.jobs, &|_| unreachable!("no workflow calls")).0;
        assert_eq!(
            suite_hash,
            format!(
                "{:x}",
                Sha256::digest(
                    br#"{"jobs":[{"name":"t","command":"true","depends_on":[],"partitions":null,"timeout_secs":null,"shell":true,"condition":null,"steps":[]}]}"#
                )
            )
        );
        let targets = |_: &str| suite.jobs.as_slice();
        let document = GraphDocument {
            jobs: graph_jobs(&ci.jobs, &targets, &mut HashMap::new()),
        };
        assert_eq!(
            serde_json::to_string(&document).unwrap(),
            format!(
                r#"{{"jobs":[{{"name":"call","command":null,"depends_on":["prep"],"partitions":null,"timeout_secs":null,"shell":true,"condition":{{"kind":"expression","text":"true"}},"steps":[],"call":{{"workflow":"suite","graph_sha256":"{suite_hash}"}}}},{{"name":"prep","command":"make","depends_on":[],"partitions":null,"timeout_secs":null,"shell":true,"condition":null,"steps":[]}}]}}"#
            )
        );
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

    fn job_result(name: &str, status: Status) -> JobResult {
        JobResult {
            name: name.into(),
            partition: None,
            command: None,
            workflow: None,
            child_run_id: None,
            status,
            duration_ms: 0,
            exit_code: None,
            error: None,
            outputs: Default::default(),
            steps: Vec::new(),
            stdout_tail: None,
            stderr_tail: None,
            stdout_truncated: None,
            stderr_truncated: None,
            executed: true,
            started: true,
            blocked_ms: 0,
            queued_ms: 0,
            commands: Vec::new(),
            fail_fast_skip: false,
        }
    }

    /// A finished child run touching every optional record field: a
    /// partition, an over-long command, a redacted error, step costs, a
    /// workflow call and an unstarted job.
    fn fixed_results() -> Vec<JobResult> {
        vec![
            JobResult {
                partition: Some(Partition { index: 2, count: 3 }),
                command: Some(format!("make {}é", "x".repeat(1020))),
                exit_code: Some(1),
                error: Some("failed in /w/sub; log /w/logs/run/001-test.stdout.log".into()),
                duration_ms: 1200,
                blocked_ms: 5,
                queued_ms: 7,
                commands: vec![crate::runner::CommandCost {
                    kind: crate::runner::CommandKind::Command,
                    step: Some("s".into()),
                    wall_ms: 1100,
                    cpu_user_ms: Some(900),
                    cpu_system_ms: Some(80),
                    peak_rss_bytes: Some(4096),
                }],
                ..job_result("test", Status::Failed)
            },
            JobResult {
                workflow: Some("suite".into()),
                child_run_id: Some("0e54fb55-a115-4ac7-80fe-8fde26e76afb".into()),
                duration_ms: 30,
                ..job_result("call", Status::Passed)
            },
            JobResult {
                command: Some("true".into()),
                executed: false,
                started: false,
                ..job_result("later", Status::Cancelled)
            },
        ]
    }

    fn fixed_fields() -> RunFields {
        RunFields {
            run_id: "fb7d2711-6b67-4f5a-9104-492032a9e1e5".into(),
            started_at: "2026-11-09T12:00:00Z".into(),
            outcome: "failed",
            parallelism: 4,
            graph_sha256: "ab".repeat(32),
            max_in_flight: 2,
            host: Host {
                os: "linux",
                arch: "x86_64",
                logical_cpus: 8,
                mem_total_bytes: Some(16 << 30),
                mem_available_bytes: Some(8 << 30),
            },
        }
    }

    fn fixed_source() -> Source {
        Source {
            commit: Some("c".repeat(40)),
            branch: Some("main".into()),
            dirty: Some(true),
        }
    }

    fn fixed_target<'a>(
        dir: &'a Path,
        source: &'a Source,
        requested: &'a [String],
    ) -> HistoryTarget<'a> {
        HistoryTarget {
            dir,
            workflow: "ci",
            environment: Some("local"),
            requested_jobs: requested,
            parent: Some(("0f0f0f0f-0000-4000-8000-000000000000", "call-ci")),
            source: Some(source),
            stem: "stem",
            runs: 10,
            bytes: 1 << 20,
            workdir: Path::new("/w"),
            log_dir: Some(Path::new("/w/logs/run")),
        }
    }

    #[test]
    fn local_history_bytes_are_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let (results, source) = (fixed_results(), fixed_source());
        let requested = ["test".to_owned()];
        let target = fixed_target(dir.path(), &source, &requested);
        let written = write(&target, &fixed_fields(), 1500, &results).unwrap();
        // Pinned from `write` before the record builder was extracted. The
        // source's branch never reaches a local file.
        let command = format!("make {}", "x".repeat(1019));
        assert_eq!(
            std::fs::read_to_string(written.path).unwrap(),
            format!(
                r#"{{"schema_version":1,"workflow":"ci","environment":"local","requested_jobs":["test"],"run_id":"fb7d2711-6b67-4f5a-9104-492032a9e1e5","parent_run_id":"0f0f0f0f-0000-4000-8000-000000000000","caller_job":"call-ci","started_at":"2026-11-09T12:00:00Z","outcome":"failed","duration_ms":1500,"parallelism":4,"graph_sha256":"abababababababababababababababababababababababababababababababab","max_in_flight":2,"host":{{"os":"linux","arch":"x86_64","logical_cpus":8,"mem_total_bytes":17179869184,"mem_available_bytes":8589934592}},"source":{{"commit":"cccccccccccccccccccccccccccccccccccccccc","dirty":true}},"results":[{{"name":"test","partition":{{"index":2,"count":3}},"status":"failed","exit_code":1,"command":"{command}","error":"failed in <workdir>/sub; log <log>","duration_ms":1200,"started":true,"blocked_ms":5,"queued_ms":7,"commands":[{{"kind":"command","step":"s","wall_ms":1100,"cpu_user_ms":900,"cpu_system_ms":80,"peak_rss_bytes":4096}}]}},{{"name":"call","workflow":"suite","child_run_id":"0e54fb55-a115-4ac7-80fe-8fde26e76afb","status":"passed","exit_code":null,"command":null,"error":null,"duration_ms":30,"started":true,"blocked_ms":0,"queued_ms":0}},{{"name":"later","status":"cancelled","exit_code":null,"command":"true","error":null,"duration_ms":0,"started":false,"blocked_ms":0,"queued_ms":0}}]}}
"#
            )
        );
    }

    #[test]
    fn an_upload_record_is_the_local_record_plus_the_upload_keys() {
        let (results, source, fields) = (fixed_results(), fixed_source(), fixed_fields());
        let requested = ["test".to_owned()];
        let target = fixed_target(Path::new("/unused"), &source, &requested);
        let mut config: crate::config::Config = toml::from_str(&format!(
            "[[jobs]]\nname = 'test'\ncommand = '{}'\n",
            "x".repeat(2000)
        ))
        .unwrap();
        config.validate().unwrap();
        let (_, graph) = graph_sha256(&config.jobs, &|_| unreachable!("no workflow calls"));
        let ci = serde_json::json!({"provider": "github_actions", "event": "push"});
        let upload = record(
            &target,
            &fields,
            1500,
            &results,
            Upload {
                branch: true,
                ci: Some(&ci),
                graph: Some(&graph),
            },
        );
        let text = serde_json::to_string(&upload).unwrap();
        // Key order: branch after dirty, ci after source, graph last.
        let at = |key: &str| text.find(&format!("\"{key}\":")).unwrap();
        assert!(
            at("dirty") < at("branch") && at("branch") < at("ci"),
            "{text}"
        );
        assert!(
            at("source") < at("ci") && at("ci") < at("results"),
            "{text}"
        );
        assert!(at("results") < at("graph"), "{text}");
        assert!(text.ends_with("]}}"), "graph is not last: {text}");

        let mut upload = serde_json::to_value(&upload).unwrap();
        assert_eq!(upload["source"]["branch"], "main");
        assert_eq!(upload["ci"], ci);
        // The record carries the truncated copy of the hashed graph.
        assert_eq!(
            upload["graph"],
            serde_json::to_value(graph.truncated()).unwrap()
        );
        assert_eq!(
            upload["graph"]["jobs"][0]["command"]
                .as_str()
                .unwrap()
                .len(),
            1024
        );
        let object = upload.as_object_mut().unwrap();
        object.remove("graph").unwrap();
        object.remove("ci").unwrap();
        object["source"]
            .as_object_mut()
            .unwrap()
            .remove("branch")
            .unwrap();
        let local = record(&target, &fields, 1500, &results, Upload::default());
        assert_eq!(upload, serde_json::to_value(&local).unwrap());

        // Without a captured branch there is no `source.branch` to add.
        let detached = Source {
            branch: None,
            ..fixed_source()
        };
        let target = fixed_target(Path::new("/unused"), &detached, &requested);
        let upload = record(
            &target,
            &fields,
            1500,
            &results,
            Upload {
                branch: true,
                ..Upload::default()
            },
        );
        let upload = serde_json::to_value(&upload).unwrap();
        assert_eq!(
            upload["source"],
            serde_json::json!({"commit": "c".repeat(40), "dirty": true})
        );
        assert!(upload.get("ci").is_none() && upload.get("graph").is_none());
    }

    #[test]
    fn the_upload_graph_is_cut_after_hashing_the_full_text() {
        let long = "x".repeat(2000);
        // 5 + 2 * 600 bytes: the 1,024-byte limit falls inside an `é`.
        let multibyte = format!("test {}", "é".repeat(600));
        let mut config: crate::config::Config = toml::from_str(&format!(
            "[[jobs]]\nname = 'a'\ncommand = '{long}'\ncondition = {{ shell = '{multibyte}' }}\n\
             [[jobs]]\nname = 'b'\n\
             [[jobs.steps]]\nid = 's'\ncommand = '{multibyte}'\ncondition = {{ shell = '{long}' }}\n"
        ))
        .unwrap();
        config.validate().unwrap();
        let (hash, document) = graph_sha256(&config.jobs, &|_| unreachable!("no workflow calls"));
        let digest = |document: &GraphDocument| {
            format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(document).unwrap())
            )
        };
        // The hash is over the uncut document that is returned.
        assert_eq!(hash, digest(&document));
        let full = serde_json::to_value(&document).unwrap();
        assert_eq!(full["jobs"][0]["command"], long.as_str());
        assert_eq!(full["jobs"][1]["steps"][0]["command"], multibyte.as_str());

        let truncated = document.truncated();
        assert_ne!(digest(&truncated), hash);
        let cut = serde_json::to_value(&truncated).unwrap();
        let expect_cut = |value: &serde_json::Value, from: &str, len: usize| {
            let text = value.as_str().unwrap();
            assert_eq!(text.len(), len);
            assert!(from.starts_with(text));
        };
        let job = &cut["jobs"][0];
        expect_cut(&job["command"], &long, 1024);
        expect_cut(&job["condition"]["text"], &multibyte, 1023);
        let step = &cut["jobs"][1]["steps"][0];
        expect_cut(&step["command"], &multibyte, 1023);
        expect_cut(&step["condition"]["text"], &long, 1024);
        // Only text is cut: the shape and every other value are the same.
        let mut restored = cut.clone();
        restored["jobs"][0]["command"] = full["jobs"][0]["command"].clone();
        restored["jobs"][0]["condition"]["text"] = full["jobs"][0]["condition"]["text"].clone();
        restored["jobs"][1]["steps"][0] = full["jobs"][1]["steps"][0].clone();
        assert_eq!(restored, full);
        // Text within the limit is kept whole.
        let short = "[[jobs]]\nname = 'a'\ncommand = 'true'\n";
        let mut config: crate::config::Config = toml::from_str(short).unwrap();
        config.validate().unwrap();
        let (_, document) = graph_sha256(&config.jobs, &|_| unreachable!("no workflow calls"));
        assert_eq!(
            serde_json::to_value(document.truncated()).unwrap(),
            serde_json::to_value(&document).unwrap()
        );
    }

    #[test]
    fn an_upload_graph_references_a_called_workflow_by_hash() {
        let mut config: crate::config::Config = toml::from_str(&format!(
            "[[workflows.ci.jobs]]\nname = 'call'\nworkflow = 'suite'\n\
             [[workflows.suite.jobs]]\nname = 't'\ncommand = '{}'\n",
            "y".repeat(2000)
        ))
        .unwrap();
        config.validate().unwrap();
        let (_, ci) = config.workflow(Some("ci"), None).unwrap();
        let (_, suite) = config.workflow(Some("suite"), None).unwrap();
        let suite_hash = graph_sha256(&suite.jobs, &|_| unreachable!("no workflow calls")).0;
        let targets = |_: &str| suite.jobs.as_slice();
        let (_, document) = graph_sha256(&ci.jobs, &targets);
        let graph = serde_json::to_value(document.truncated()).unwrap();
        // One job: the call, with its target's full-text hash and no inlined jobs.
        assert_eq!(
            graph,
            serde_json::json!({"jobs": [{
                "name": "call", "command": null, "depends_on": [], "partitions": null,
                "timeout_secs": null, "shell": true, "condition": null, "steps": [],
                "call": {"workflow": "suite", "graph_sha256": suite_hash},
            }]})
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
