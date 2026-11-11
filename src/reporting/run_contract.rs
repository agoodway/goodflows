//! Local validation of an uploaded run record.
//!
//! The rules mirror the run-history contract in the goodflows_web repository
//! (its `workflow-run-storage` spec): the history document at
//! `schema_version` 1 with the workflow-call fields, plus the upload envelope
//! (`graph`, `ci`, `source.branch`). Validation runs on
//! the encoded `serde_json::Value`, so it checks exactly what is sent and
//! what a replay reads back.
//!
//! Only the first violation is reported, as a JSON path in the server's
//! `results[3].name` style and a code from a fixed list. An unknown key is
//! reported on its enclosing object, so no submitted key or value ever
//! reaches the error.

use super::contract::MAX_SAFE_INTEGER;
use reqwest::Url;
use serde_json::{Map, Value};
use std::collections::{HashMap, HashSet};

/// The largest encoded record the server accepts by default.
pub const MAX_BODY_BYTES: usize = 4_194_304;
/// Objects and arrays nest at most this many levels.
pub const MAX_DEPTH: usize = 8;
/// `requested_jobs`, `commands`, `graph.jobs`, `depends_on` and `steps`.
const MAX_ENTRIES: usize = 1024;
/// `command`, `error`, and graph command and condition text.
const MAX_TEXT_BYTES: usize = 1024;
const MAX_NAME_BYTES: usize = 64;
const MAX_BRANCH_BYTES: usize = 256;
pub const MAX_CI_RUN_ID_BYTES: usize = 256;
pub const MAX_URL_BYTES: usize = 2048;
const MAX_PARTITIONS: i64 = crate::config::MAX_PARTITIONS as i64;

/// The first rule a record breaks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// `body` for the whole document, otherwise for example `results[3].name`.
    pub path: String,
    pub code: &'static str,
}

/// Validate `record`, whose encoding is `encoded_len` bytes.
pub fn validate(record: &Value, encoded_len: usize) -> Result<(), Violation> {
    let body = Path::Body;
    if encoded_len > MAX_BODY_BYTES {
        return Err(body.fail("too_large"));
    }
    if depth(record) > MAX_DEPTH {
        return Err(body.fail("too_deep"));
    }
    let doc = object(record, &body, DOCUMENT)?;
    let at = |key| Path::Key(&body, key);
    if integer(&doc["schema_version"], &at("schema_version"))? != 1 {
        return Err(at("schema_version").fail("invalid_field"));
    }
    name(&doc["workflow"], &at("workflow"))?;
    if let Some(environment) = doc.get("environment") {
        name(environment, &at("environment"))?;
    }
    if let Some(requested) = doc.get("requested_jobs") {
        let path = at("requested_jobs");
        for (i, job) in list(requested, &path, 1, MAX_ENTRIES)?.iter().enumerate() {
            name(job, &Path::Index(&path, i))?;
        }
    }
    uuid(&doc["run_id"], &at("run_id"))?;
    if let Some(parent) = doc.get("parent_run_id") {
        uuid(parent, &at("parent_run_id"))?;
    }
    if let Some(caller) = doc.get("caller_job") {
        name(caller, &at("caller_job"))?;
    }
    started_at(&doc["started_at"], &at("started_at"))?;
    one_of(&doc["outcome"], &at("outcome"), OUTCOMES)?;
    at_least(&doc["duration_ms"], &at("duration_ms"), 0)?;
    at_least(&doc["parallelism"], &at("parallelism"), 1)?;
    hex(&doc["graph_sha256"], &at("graph_sha256"), &[64])?;
    at_least(&doc["max_in_flight"], &at("max_in_flight"), 0)?;
    host(&doc["host"], &at("host"))?;
    if let Some(source) = doc.get("source") {
        self::source(source, &at("source"))?;
    }
    if let Some(ci) = doc.get("ci") {
        self::ci(ci, &at("ci"))?;
    }
    let path = at("results");
    let results = list(&doc["results"], &path, 0, usize::MAX)?;
    for (i, result) in results.iter().enumerate() {
        self::result(result, &Path::Index(&path, i))?;
    }
    if let Some(graph) = doc.get("graph") {
        self::graph(graph, &at("graph"))?;
    }

    // Cross-field rules, once every field has its type.
    match (
        doc.contains_key("parent_run_id"),
        doc.contains_key("caller_job"),
    ) {
        (true, false) => return Err(at("parent_run_id").fail("unpaired_field")),
        (false, true) => return Err(at("caller_job").fail("unpaired_field")),
        _ => {}
    }
    for (i, result) in results.iter().enumerate() {
        let result = result.as_object().expect("checked above");
        if result.contains_key("child_run_id") && !result.contains_key("workflow") {
            let path = Path::Index(&path, i);
            return Err(Path::Key(&path, "child_run_id").fail("unpaired_field"));
        }
    }
    let status = |s: &str| results.iter().any(|r| r["status"] == s);
    let failed = status("failed") || status("timed_out");
    let consistent = match doc["outcome"].as_str() {
        Some("failed") => failed,
        Some("passed") => !failed && !status("cancelled"),
        // A signal can cancel a run after its last job finished.
        _ => !failed,
    };
    if !consistent {
        return Err(at("outcome").fail("inconsistent_outcome"));
    }
    if let Some(graph) = doc.get("graph") {
        graph_consistency(graph, results, &body)?;
    }
    Ok(())
}

/// The server's name rule: 1-64 UTF-8 bytes, no control characters, and not
/// only whitespace.
pub fn valid_name(name: &str) -> bool {
    valid_label(name, MAX_NAME_BYTES) && !name.trim().is_empty()
}

/// 1 to `max` UTF-8 bytes with no control characters.
pub fn valid_label(text: &str, max: usize) -> bool {
    !text.is_empty() && text.len() <= max && !text.chars().any(char::is_control)
}

const OUTCOMES: &[&str] = &["passed", "failed", "cancelled"];
const STATUSES: &[&str] = &["passed", "failed", "skipped", "timed_out", "cancelled"];

/// Required keys must be present. Optional keys may be absent but never
/// null. Nullable keys must be present and may be null.
#[derive(Clone, Copy, PartialEq)]
enum Presence {
    Required,
    Optional,
    Nullable,
}
use Presence::{Nullable, Optional, Required};

type Fields = &'static [(&'static str, Presence)];

const DOCUMENT: Fields = &[
    ("schema_version", Required),
    ("workflow", Required),
    ("environment", Optional),
    ("requested_jobs", Optional),
    ("run_id", Required),
    ("parent_run_id", Optional),
    ("caller_job", Optional),
    ("started_at", Required),
    ("outcome", Required),
    ("duration_ms", Required),
    ("parallelism", Required),
    ("graph_sha256", Required),
    ("max_in_flight", Required),
    ("host", Required),
    ("source", Optional),
    ("ci", Optional),
    ("results", Required),
    ("graph", Optional),
];
const HOST: Fields = &[
    ("os", Required),
    ("arch", Required),
    ("logical_cpus", Required),
    ("mem_total_bytes", Optional),
    ("mem_available_bytes", Optional),
];
const SOURCE: Fields = &[
    ("commit", Required),
    ("dirty", Optional),
    ("branch", Optional),
];
const CI: Fields = &[
    ("provider", Required),
    ("event", Optional),
    ("run_id", Optional),
    ("attempt", Optional),
    ("pull_request", Optional),
    ("runner", Optional),
    ("url", Optional),
];
const RESULT: Fields = &[
    ("name", Required),
    ("partition", Optional),
    ("workflow", Optional),
    ("child_run_id", Optional),
    ("status", Required),
    ("exit_code", Nullable),
    ("command", Nullable),
    ("error", Nullable),
    ("duration_ms", Required),
    ("started", Required),
    ("blocked_ms", Required),
    ("queued_ms", Required),
    ("commands", Optional),
];
const PARTITION: Fields = &[("index", Required), ("count", Required)];
const COMMAND: Fields = &[
    ("kind", Required),
    ("step", Optional),
    ("wall_ms", Required),
    ("cpu_user_ms", Optional),
    ("cpu_system_ms", Optional),
    ("peak_rss_bytes", Optional),
];
const GRAPH: Fields = &[("jobs", Required)];
const GRAPH_JOB: Fields = &[
    ("name", Required),
    ("command", Nullable),
    ("depends_on", Required),
    ("partitions", Nullable),
    ("timeout_secs", Nullable),
    ("shell", Required),
    ("condition", Nullable),
    ("steps", Required),
    ("call", Optional),
];
const GRAPH_PARTITIONS: Fields = &[("count", Required), ("strategy", Required)];
const GRAPH_CONDITION: Fields = &[("kind", Required), ("text", Required)];
const GRAPH_STEP: Fields = &[
    ("id", Required),
    ("command", Required),
    ("shell", Nullable),
    ("condition", Nullable),
];
const GRAPH_CALL: Fields = &[("workflow", Required), ("graph_sha256", Required)];

/// A JSON path, kept on the stack and rendered only for an error.
#[derive(Clone, Copy)]
enum Path<'a> {
    Body,
    Key(&'a Path<'a>, &'static str),
    Index(&'a Path<'a>, usize),
}

impl Path<'_> {
    fn render(&self, out: &mut String) {
        match self {
            Path::Body => {}
            Path::Key(parent, key) => {
                parent.render(out);
                if !out.is_empty() {
                    out.push('.');
                }
                out.push_str(key);
            }
            Path::Index(parent, index) => {
                parent.render(out);
                out.push_str(&format!("[{index}]"));
            }
        }
    }

    fn fail(&self, code: &'static str) -> Violation {
        let mut path = String::new();
        self.render(&mut path);
        if path.is_empty() {
            path.push_str("body");
        }
        Violation { path, code }
    }
}

type Check<T = ()> = Result<T, Violation>;

/// Objects and arrays nest this many levels; a scalar is 0.
fn depth(value: &Value) -> usize {
    match value {
        Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
        Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// An object with exactly the allowed keys, required ones present and
/// optional ones not null.
fn object<'v>(value: &'v Value, path: &Path, fields: Fields) -> Check<&'v Map<String, Value>> {
    let Value::Object(map) = value else {
        return Err(path.fail("invalid_field"));
    };
    if map
        .keys()
        .any(|key| !fields.iter().any(|(name, _)| name == key))
    {
        return Err(path.fail("unsupported_field"));
    }
    for &(key, presence) in fields {
        match (map.get(key), presence) {
            (None, Required | Nullable) => return Err(Path::Key(path, key).fail("missing_field")),
            (Some(Value::Null), Required | Optional) => {
                return Err(Path::Key(path, key).fail("null_field"))
            }
            _ => {}
        }
    }
    Ok(map)
}

/// A present, non-null value of a nullable key.
fn present(value: &Value) -> Option<&Value> {
    (!value.is_null()).then_some(value)
}

fn list<'v>(value: &'v Value, path: &Path, min: usize, max: usize) -> Check<&'v Vec<Value>> {
    let Value::Array(items) = value else {
        return Err(path.fail("invalid_field"));
    };
    if items.len() > max {
        return Err(path.fail("too_many_entries"));
    }
    if items.len() < min {
        return Err(path.fail("empty_list"));
    }
    Ok(items)
}

fn string<'v>(value: &'v Value, path: &Path) -> Check<&'v str> {
    match value {
        Value::String(text) if text.contains('\0') => Err(path.fail("nul_character")),
        Value::String(text) => Ok(text),
        _ => Err(path.fail("invalid_field")),
    }
}

fn name(value: &Value, path: &Path) -> Check {
    if valid_name(string(value, path)?) {
        Ok(())
    } else {
        Err(path.fail("invalid_name"))
    }
}

fn text(value: &Value, path: &Path) -> Check {
    if string(value, path)?.len() > MAX_TEXT_BYTES {
        Err(path.fail("field_too_long"))
    } else {
        Ok(())
    }
}

fn label(value: &Value, path: &Path, max: usize) -> Check {
    let text = string(value, path)?;
    if text.len() > max {
        Err(path.fail("field_too_long"))
    } else if !valid_label(text, max) {
        Err(path.fail("invalid_field"))
    } else {
        Ok(())
    }
}

fn one_of<'v>(value: &'v Value, path: &Path, allowed: &[&str]) -> Check<&'v str> {
    let text = string(value, path)?;
    if allowed.contains(&text) {
        Ok(text)
    } else {
        Err(path.fail("invalid_field"))
    }
}

fn boolean(value: &Value, path: &Path) -> Check {
    if value.is_boolean() {
        Ok(())
    } else {
        Err(path.fail("invalid_field"))
    }
}

/// An integer within ±(2^53-1). A fraction, including `1.0`, is not one.
fn integer(value: &Value, path: &Path) -> Check<i64> {
    let Value::Number(number) = value else {
        return Err(path.fail("invalid_field"));
    };
    let safe = MAX_SAFE_INTEGER as i64;
    if let Some(n) = number.as_i64() {
        return if n.unsigned_abs() <= MAX_SAFE_INTEGER {
            Ok(n)
        } else {
            Err(path.fail("unsafe_integer"))
        };
    }
    if number.as_u64().is_some() {
        return Err(path.fail("unsafe_integer"));
    }
    // A float: an integer too large for 64 bits, or a fraction.
    match number.as_f64() {
        Some(f) if f.fract() == 0.0 && f.abs() > safe as f64 => Err(path.fail("unsafe_integer")),
        _ => Err(path.fail("invalid_field")),
    }
}

fn at_least(value: &Value, path: &Path, min: i64) -> Check<i64> {
    let n = integer(value, path)?;
    if n < min {
        Err(path.fail("invalid_field"))
    } else {
        Ok(n)
    }
}

fn is_hex(text: &str) -> bool {
    text.bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// Lowercase hex of one of `lengths`.
fn hex(value: &Value, path: &Path, lengths: &[usize]) -> Check {
    let text = string(value, path)?;
    if lengths.contains(&text.len()) && is_hex(text) {
        Ok(())
    } else {
        Err(path.fail("invalid_field"))
    }
}

/// A lowercase canonical UUID.
fn uuid(value: &Value, path: &Path) -> Check {
    let text = string(value, path)?;
    let groups: Vec<_> = text.split('-').map(str::len).collect();
    if groups == [8, 4, 4, 4, 12] && is_hex(&text.replace('-', "")) {
        Ok(())
    } else {
        Err(path.fail("invalid_field"))
    }
}

/// Exactly `YYYY-MM-DDTHH:MM:SSZ`, naming a real UTC second.
fn started_at(value: &Value, path: &Path) -> Check {
    let text = string(value, path)?;
    let b = text.as_bytes();
    let digits = |range: std::ops::Range<usize>| {
        b[range.clone()].iter().all(u8::is_ascii_digit).then(|| {
            b[range]
                .iter()
                .fold(0u32, |n, d| n * 10 + u32::from(d - b'0'))
        })
    };
    let shape = b.len() == 20
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && b[19] == b'Z';
    let valid = shape
        && match (
            digits(0..4),
            digits(5..7),
            digits(8..10),
            digits(11..13),
            digits(14..16),
            digits(17..19),
        ) {
            (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) => {
                let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
                let days = match month {
                    1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
                    4 | 6 | 9 | 11 => 30,
                    2 if leap => 29,
                    2 => 28,
                    _ => 0,
                };
                (1..=days).contains(&day) && hour < 24 && minute < 60 && second < 60
            }
            _ => false,
        };
    if valid {
        Ok(())
    } else {
        Err(path.fail("invalid_field"))
    }
}

fn host(value: &Value, path: &Path) -> Check {
    let host = object(value, path, HOST)?;
    let at = |key| Path::Key(path, key);
    name(&host["os"], &at("os"))?;
    name(&host["arch"], &at("arch"))?;
    at_least(&host["logical_cpus"], &at("logical_cpus"), 1)?;
    for key in ["mem_total_bytes", "mem_available_bytes"] {
        if let Some(bytes) = host.get(key) {
            at_least(bytes, &at(key), 0)?;
        }
    }
    Ok(())
}

fn source(value: &Value, path: &Path) -> Check {
    let source = object(value, path, SOURCE)?;
    let at = |key| Path::Key(path, key);
    hex(&source["commit"], &at("commit"), &[40, 64])?;
    if let Some(dirty) = source.get("dirty") {
        boolean(dirty, &at("dirty"))?;
    }
    if let Some(branch) = source.get("branch") {
        label(branch, &at("branch"), MAX_BRANCH_BYTES)?;
    }
    Ok(())
}

fn ci(value: &Value, path: &Path) -> Check {
    let ci = object(value, path, CI)?;
    let at = |key| Path::Key(path, key);
    one_of(&ci["provider"], &at("provider"), &["github_actions"])?;
    for key in ["event", "runner"] {
        if let Some(value) = ci.get(key) {
            name(value, &at(key))?;
        }
    }
    if let Some(run_id) = ci.get("run_id") {
        label(run_id, &at("run_id"), MAX_CI_RUN_ID_BYTES)?;
    }
    for key in ["attempt", "pull_request"] {
        if let Some(value) = ci.get(key) {
            at_least(value, &at(key), 1)?;
        }
    }
    if let Some(url) = ci.get("url") {
        let path = at("url");
        let text = string(url, &path)?;
        if text.len() > MAX_URL_BYTES {
            return Err(path.fail("field_too_long"));
        }
        let https =
            Url::parse(text).is_ok_and(|url| url.scheme() == "https" && url.host_str().is_some());
        if !https {
            return Err(path.fail("invalid_field"));
        }
    }
    Ok(())
}

fn result(value: &Value, path: &Path) -> Check {
    let result = object(value, path, RESULT)?;
    let at = |key| Path::Key(path, key);
    name(&result["name"], &at("name"))?;
    if let Some(partition) = result.get("partition") {
        let path = at("partition");
        let fields = object(partition, &path, PARTITION)?;
        let index = at_least(&fields["index"], &Path::Key(&path, "index"), 1)?;
        let count = at_least(&fields["count"], &Path::Key(&path, "count"), 1)?;
        if index > count || count > MAX_PARTITIONS {
            return Err(path.fail("invalid_field"));
        }
    }
    if let Some(workflow) = result.get("workflow") {
        name(workflow, &at("workflow"))?;
    }
    if let Some(child) = result.get("child_run_id") {
        uuid(child, &at("child_run_id"))?;
    }
    one_of(&result["status"], &at("status"), STATUSES)?;
    if let Some(code) = present(&result["exit_code"]) {
        let path = at("exit_code");
        if i32::try_from(integer(code, &path)?).is_err() {
            return Err(path.fail("invalid_field"));
        }
    }
    for key in ["command", "error"] {
        if let Some(value) = present(&result[key]) {
            text(value, &at(key))?;
        }
    }
    for key in ["duration_ms", "blocked_ms", "queued_ms"] {
        at_least(&result[key], &at(key), 0)?;
    }
    boolean(&result["started"], &at("started"))?;
    if let Some(commands) = result.get("commands") {
        let path = at("commands");
        for (i, command) in list(commands, &path, 1, MAX_ENTRIES)?.iter().enumerate() {
            let path = Path::Index(&path, i);
            let fields = object(command, &path, COMMAND)?;
            let at = |key| Path::Key(&path, key);
            one_of(&fields["kind"], &at("kind"), &["condition", "command"])?;
            if let Some(step) = fields.get("step") {
                name(step, &at("step"))?;
            }
            at_least(&fields["wall_ms"], &at("wall_ms"), 0)?;
            for key in ["cpu_user_ms", "cpu_system_ms"] {
                if let Some(ms) = fields.get(key) {
                    at_least(ms, &at(key), 0)?;
                }
            }
            if let Some(rss) = fields.get("peak_rss_bytes") {
                at_least(rss, &at("peak_rss_bytes"), 1)?;
            }
        }
    }
    Ok(())
}

fn condition(value: &Value, path: &Path) -> Check {
    let Some(value) = present(value) else {
        return Ok(());
    };
    let condition = object(value, path, GRAPH_CONDITION)?;
    one_of(
        &condition["kind"],
        &Path::Key(path, "kind"),
        &["expression", "shell"],
    )?;
    text(&condition["text"], &Path::Key(path, "text"))
}

fn graph(value: &Value, path: &Path) -> Check {
    let graph = object(value, path, GRAPH)?;
    let path = Path::Key(path, "jobs");
    for (i, job) in list(&graph["jobs"], &path, 1, MAX_ENTRIES)?
        .iter()
        .enumerate()
    {
        let path = Path::Index(&path, i);
        let job = object(job, &path, GRAPH_JOB)?;
        let at = |key| Path::Key(&path, key);
        name(&job["name"], &at("name"))?;
        if let Some(command) = present(&job["command"]) {
            text(command, &at("command"))?;
        }
        let deps = at("depends_on");
        for (j, dep) in list(&job["depends_on"], &deps, 0, MAX_ENTRIES)?
            .iter()
            .enumerate()
        {
            name(dep, &Path::Index(&deps, j))?;
        }
        if let Some(partitions) = present(&job["partitions"]) {
            let path = at("partitions");
            let fields = object(partitions, &path, GRAPH_PARTITIONS)?;
            let count = Path::Key(&path, "count");
            if at_least(&fields["count"], &count, 1)? > MAX_PARTITIONS {
                return Err(count.fail("invalid_field"));
            }
            one_of(
                &fields["strategy"],
                &Path::Key(&path, "strategy"),
                &["index"],
            )?;
        }
        if let Some(timeout) = present(&job["timeout_secs"]) {
            at_least(timeout, &at("timeout_secs"), 1)?;
        }
        boolean(&job["shell"], &at("shell"))?;
        condition(&job["condition"], &at("condition"))?;
        let steps = at("steps");
        for (j, step) in list(&job["steps"], &steps, 0, MAX_ENTRIES)?
            .iter()
            .enumerate()
        {
            let path = Path::Index(&steps, j);
            let step = object(step, &path, GRAPH_STEP)?;
            let at = |key| Path::Key(&path, key);
            name(&step["id"], &at("id"))?;
            text(&step["command"], &at("command"))?;
            if let Some(shell) = present(&step["shell"]) {
                boolean(shell, &at("shell"))?;
            }
            condition(&step["condition"], &at("condition"))?;
        }
        if let Some(call) = job.get("call") {
            let path = at("call");
            let call = object(call, &path, GRAPH_CALL)?;
            name(&call["workflow"], &Path::Key(&path, "workflow"))?;
            hex(
                &call["graph_sha256"],
                &Path::Key(&path, "graph_sha256"),
                &[64],
            )?;
        }
    }
    Ok(())
}

/// Unique job names, known dependencies, no cycle, and every result naming
/// a graph job. Runs on a graph that passed `graph`.
fn graph_consistency(graph: &Value, results: &[Value], body: &Path) -> Check {
    let graph_path = Path::Key(body, "graph");
    let jobs_path = Path::Key(&graph_path, "jobs");
    let jobs = graph["jobs"].as_array().expect("checked by graph");
    let mut index = HashMap::new();
    for (i, job) in jobs.iter().enumerate() {
        if index.insert(job["name"].as_str(), i).is_some() {
            let path = Path::Index(&jobs_path, i);
            return Err(Path::Key(&path, "name").fail("duplicate_name"));
        }
    }
    let mut deps = Vec::with_capacity(jobs.len());
    for (i, job) in jobs.iter().enumerate() {
        let path = Path::Index(&jobs_path, i);
        let path = Path::Key(&path, "depends_on");
        let mut edges = Vec::new();
        for (j, dep) in job["depends_on"]
            .as_array()
            .expect("checked")
            .iter()
            .enumerate()
        {
            match index.get(&dep.as_str()) {
                Some(&target) => edges.push(target),
                None => return Err(Path::Index(&path, j).fail("unknown_dependency")),
            }
        }
        deps.push(edges);
    }
    // Depth-first search with colors: 0 unseen, 1 on the stack, 2 done.
    let mut color = vec![0u8; jobs.len()];
    for start in 0..jobs.len() {
        if color[start] != 0 {
            continue;
        }
        let mut stack = vec![(start, 0)];
        color[start] = 1;
        while let Some(top) = stack.last_mut() {
            let node = top.0;
            if let Some(&dep) = deps[node].get(top.1) {
                top.1 += 1;
                match color[dep] {
                    0 => {
                        color[dep] = 1;
                        stack.push((dep, 0));
                    }
                    1 => {
                        let path = Path::Index(&jobs_path, node);
                        return Err(Path::Key(&path, "depends_on").fail("dependency_cycle"));
                    }
                    _ => {}
                }
            } else {
                color[node] = 2;
                stack.pop();
            }
        }
    }
    let names: HashSet<_> = index.keys().collect();
    let results_path = Path::Key(body, "results");
    for (i, result) in results.iter().enumerate() {
        if !names.contains(&result["name"].as_str()) {
            let path = Path::Index(&results_path, i);
            return Err(Path::Key(&path, "name").fail("unknown_job"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    const MINIMAL: &str = include_str!("../../tests/fixtures/workflow-runs/minimal.json");
    const FULL: &str = include_str!("../../tests/fixtures/workflow-runs/full.json");
    const CHILD: &str = include_str!("../../tests/fixtures/workflow-runs/child.json");
    /// History files captured when run-history upload was designed, byte for byte.
    const HISTORY_PARENT: &str =
        include_str!("../../tests/fixtures/workflow-runs/history-parent-default.json");
    const HISTORY_CHILD: &str =
        include_str!("../../tests/fixtures/workflow-runs/history-child-suite.json");

    fn parse(text: &str) -> Value {
        serde_json::from_str(text).unwrap()
    }

    fn check(record: &Value) -> Result<(), Violation> {
        validate(record, serde_json::to_vec(record).unwrap().len())
    }

    #[track_caller]
    fn fails(record: &Value, path: &str, code: &'static str) {
        let expected = Violation {
            path: path.into(),
            code,
        };
        assert_eq!(check(record), Err(expected));
    }

    /// `text` with `change` applied.
    fn with(text: &str, change: impl FnOnce(&mut Value)) -> Value {
        let mut record = parse(text);
        change(&mut record);
        record
    }

    /// `text` with the value at JSON `pointer` replaced.
    fn set(text: &str, pointer: &str, value: Value) -> Value {
        with(text, |r| *r.pointer_mut(pointer).unwrap() = value)
    }

    fn remove(value: &mut Value, key: &str) {
        value.as_object_mut().unwrap().remove(key).unwrap();
    }

    #[test]
    fn the_fixtures_and_the_captured_history_files_pass() {
        for (name, text) in [
            ("minimal", MINIMAL),
            ("full", FULL),
            ("child", CHILD),
            ("history-parent-default", HISTORY_PARENT),
            ("history-child-suite", HISTORY_CHILD),
        ] {
            assert_eq!(check(&parse(text)), Ok(()), "{name}");
        }
        // The copies are those captured bytes.
        let digest = |text: &str| format!("{:x}", Sha256::digest(text));
        assert_eq!(
            digest(HISTORY_PARENT),
            "885e038c0f9b987034b408e021199f81da87b707b7f19d97781cc9a1ff79b6dc"
        );
        assert_eq!(
            digest(HISTORY_CHILD),
            "72e944d953fea14064d52c743e85c247f1bbdcaacbf5a054def8053bab0020b1"
        );
        // Every result in the captures has a null `error`, and the call
        // result a null `command` and `exit_code`.
        let parent = parse(HISTORY_PARENT);
        let call = &parent["results"][1];
        assert!(call["command"].is_null() && call["exit_code"].is_null());
        assert!(parent["results"]
            .as_array()
            .unwrap()
            .iter()
            .all(|r| r["error"].is_null()));
        // `full.json` covers every optional part of the upload.
        let full = parse(FULL);
        for key in ["environment", "requested_jobs", "ci", "graph"] {
            assert!(full.get(key).is_some(), "{key}");
        }
        assert!(full["source"]["branch"].is_string());
        let jobs = &full["graph"]["jobs"];
        assert!(jobs[1]["partitions"].is_object() && jobs[1]["steps"][0]["condition"].is_object());
        assert!(jobs[0]["condition"].is_object() && jobs[2]["call"].is_object());
    }

    #[test]
    fn keys_are_allowlisted_and_optional_keys_are_never_null() {
        fails(&with(FULL, |r| remove(r, "host")), "host", "missing_field");
        fails(
            &with(FULL, |r| r["project"] = json!("goodcases")),
            "body",
            "unsupported_field",
        );
        fails(
            &with(FULL, |r| remove(&mut r["results"][0], "command")),
            "results[0].command",
            "missing_field",
        );
        for (path, pointer) in [
            ("environment", "/environment"),
            ("source", "/source"),
            ("ci", "/ci"),
            ("graph", "/graph"),
            ("source.branch", "/source/branch"),
            ("host.mem_total_bytes", "/host/mem_total_bytes"),
            ("results[1].partition", "/results/1/partition"),
            (
                "results[0].commands[0].peak_rss_bytes",
                "/results/0/commands/0/peak_rss_bytes",
            ),
        ] {
            fails(&set(FULL, pointer, Value::Null), path, "null_field");
        }
        // The three nullable result keys may be null.
        let nulls = with(FULL, |r| {
            for key in ["command", "exit_code", "error"] {
                r["results"][0][key] = Value::Null;
            }
        });
        assert_eq!(check(&nulls), Ok(()));
    }

    #[test]
    fn errors_name_the_enclosing_object_and_never_a_submitted_value() {
        let record = with(FULL, |r| {
            r["results"][0]["secret_token_xyz"] = json!("secret-value");
        });
        fails(&record, "results[0]", "unsupported_field");
        let record = with(FULL, |r| {
            r["results"][0]["command"] = json!(format!("sentinel-{}", "x".repeat(1100)));
        });
        let error = check(&record).unwrap_err();
        assert_eq!(error.path, "results[0].command");
        assert!(!format!("{error:?}").contains("sentinel"));
    }

    #[test]
    fn run_level_fields_follow_the_server_rules() {
        let field = |key: &str, value: Value| with(FULL, |r| r[key] = value);
        fails(
            &field("schema_version", json!(2)),
            "schema_version",
            "invalid_field",
        );
        fails(
            &field("schema_version", json!("1")),
            "schema_version",
            "invalid_field",
        );
        for run_id in [
            "FB7D2711-6B67-4F5A-9104-492032A9E1E5",
            "fb7d27116b674f5a9104492032a9e1e5",
            "fb7d2711-6b67-4f5a-9104-492032a9e1e",
            "gb7d2711-6b67-4f5a-9104-492032a9e1e5",
        ] {
            fails(&field("run_id", json!(run_id)), "run_id", "invalid_field");
        }
        for started_at in [
            "2026-11-05T12:00:00.123Z",
            "2026-11-05T12:00:00+00:00",
            "2026-11-05 12:00:00Z",
            "2026-02-29T12:00:00Z",
            "2026-11-05T24:00:00Z",
            "2026-13-05T12:00:00Z",
        ] {
            fails(
                &field("started_at", json!(started_at)),
                "started_at",
                "invalid_field",
            );
        }
        assert_eq!(
            check(&field("started_at", json!("2028-02-29T23:59:59Z"))),
            Ok(())
        );
        fails(
            &field("outcome", json!("green")),
            "outcome",
            "invalid_field",
        );
        fails(
            &field("parallelism", json!(0)),
            "parallelism",
            "invalid_field",
        );
        fails(
            &field("duration_ms", json!(-1)),
            "duration_ms",
            "invalid_field",
        );
        fails(
            &field("duration_ms", json!(1.0)),
            "duration_ms",
            "invalid_field",
        );
        fails(
            &field("graph_sha256", json!("AB".repeat(32))),
            "graph_sha256",
            "invalid_field",
        );
        fails(
            &field("graph_sha256", json!("ab".repeat(31))),
            "graph_sha256",
            "invalid_field",
        );
        assert_eq!(
            check(&with(FULL, |r| remove(&mut r["host"], "mem_total_bytes"))),
            Ok(()),
            "memory fields are optional"
        );
        fails(
            &with(FULL, |r| r["host"]["logical_cpus"] = json!(0)),
            "host.logical_cpus",
            "invalid_field",
        );
    }

    #[test]
    fn names_are_1_to_64_bytes_without_control_characters_or_only_whitespace() {
        fails(
            &with(FULL, |r| r["workflow"] = json!("w".repeat(65))),
            "workflow",
            "invalid_name",
        );
        assert_eq!(
            check(&with(FULL, |r| r["workflow"] = json!("é".repeat(32)))),
            Ok(())
        );
        fails(
            &with(FULL, |r| r["workflow"] = json!("   ")),
            "workflow",
            "invalid_name",
        );
        fails(
            &with(FULL, |r| r["environment"] = json!("")),
            "environment",
            "invalid_name",
        );
        fails(
            &with(FULL, |r| r["requested_jobs"][0] = json!("a\tb")),
            "requested_jobs[0]",
            "invalid_name",
        );
        fails(
            &with(CHILD, |r| r["caller_job"] = json!("call\u{1b}[31m")),
            "caller_job",
            "invalid_name",
        );
        // A result name over 64 bytes, as before `validate-job-names`.
        fails(
            &with(FULL, |r| r["results"][3]["name"] = json!("n".repeat(65))),
            "results[3].name",
            "invalid_name",
        );
        fails(
            &with(FULL, |r| {
                r["results"][1]["commands"][0]["step"] = json!(" ")
            }),
            "results[1].commands[0].step",
            "invalid_name",
        );
        fails(
            &with(FULL, |r| {
                r["graph"]["jobs"][1]["steps"][0]["id"] = json!("s".repeat(65))
            }),
            "graph.jobs[1].steps[0].id",
            "invalid_name",
        );
    }

    #[test]
    fn lists_have_1_to_1024_entries() {
        let names = |n: usize| json!((0..n).map(|i| format!("j{i}")).collect::<Vec<_>>());
        fails(
            &with(FULL, |r| r["requested_jobs"] = json!([])),
            "requested_jobs",
            "empty_list",
        );
        fails(
            &with(FULL, |r| r["requested_jobs"] = names(1025)),
            "requested_jobs",
            "too_many_entries",
        );
        assert_eq!(
            check(&with(FULL, |r| r["requested_jobs"] = names(1024))),
            Ok(())
        );
        let commands = |n: usize| {
            with(FULL, |r| {
                let entry = r["results"][0]["commands"][0].clone();
                r["results"][0]["commands"] = json!(vec![entry; n]);
            })
        };
        fails(&commands(0), "results[0].commands", "empty_list");
        fails(&commands(1025), "results[0].commands", "too_many_entries");
        assert_eq!(check(&commands(1024)), Ok(()));
        fails(
            &with(FULL, |r| {
                let job = r["graph"]["jobs"][0].clone();
                r["graph"]["jobs"] = json!(vec![job; 1025]);
            }),
            "graph.jobs",
            "too_many_entries",
        );
        fails(
            &with(FULL, |r| r["graph"]["jobs"] = json!([])),
            "graph.jobs",
            "empty_list",
        );
        fails(
            &with(FULL, |r| {
                r["graph"]["jobs"][1]["depends_on"] = json!(vec!["build"; 1025])
            }),
            "graph.jobs[1].depends_on",
            "too_many_entries",
        );
        fails(
            &with(FULL, |r| {
                let step = r["graph"]["jobs"][1]["steps"][1].clone();
                r["graph"]["jobs"][1]["steps"] = json!(vec![step; 1025]);
            }),
            "graph.jobs[1].steps",
            "too_many_entries",
        );
    }

    #[test]
    fn text_integer_and_partition_bounds() {
        for (path, pointer) in [
            ("results[0].command", "/results/0/command"),
            ("results[0].error", "/results/0/error"),
            ("graph.jobs[0].command", "/graph/jobs/0/command"),
            (
                "graph.jobs[0].condition.text",
                "/graph/jobs/0/condition/text",
            ),
            (
                "graph.jobs[1].steps[0].command",
                "/graph/jobs/1/steps/0/command",
            ),
            (
                "graph.jobs[1].steps[0].condition.text",
                "/graph/jobs/1/steps/0/condition/text",
            ),
        ] {
            fails(
                &set(FULL, pointer, json!("x".repeat(1025))),
                path,
                "field_too_long",
            );
            let at_limit = set(FULL, pointer, json!("é".repeat(512)));
            assert_eq!(check(&at_limit), Ok(()), "{path}");
        }
        let exit_code = |code: Value| with(FULL, |r| r["results"][0]["exit_code"] = code);
        fails(
            &exit_code(json!(2_147_483_648i64)),
            "results[0].exit_code",
            "invalid_field",
        );
        fails(
            &exit_code(json!(-2_147_483_649i64)),
            "results[0].exit_code",
            "invalid_field",
        );
        assert_eq!(check(&exit_code(json!(i32::MIN))), Ok(()));
        assert_eq!(check(&exit_code(json!(i32::MAX))), Ok(()));
        fails(
            &with(FULL, |r| {
                r["results"][0]["commands"][0]["peak_rss_bytes"] = json!(0)
            }),
            "results[0].commands[0].peak_rss_bytes",
            "invalid_field",
        );
        let partition = |index: i64, count: i64| {
            with(FULL, |r| {
                r["results"][1]["partition"] = json!({"index": index, "count": count})
            })
        };
        fails(&partition(1, 65), "results[1].partition", "invalid_field");
        fails(
            &partition(0, 2),
            "results[1].partition.index",
            "invalid_field",
        );
        fails(&partition(3, 2), "results[1].partition", "invalid_field");
        assert_eq!(check(&partition(64, 64)), Ok(()));
        fails(
            &with(FULL, |r| {
                r["graph"]["jobs"][1]["partitions"]["count"] = json!(65)
            }),
            "graph.jobs[1].partitions.count",
            "invalid_field",
        );
    }

    #[test]
    fn the_outcome_agrees_with_the_result_statuses() {
        let run = |outcome: &str, status: &str| {
            with(FULL, |r| {
                r["outcome"] = json!(outcome);
                r["results"][0]["status"] = json!(status);
            })
        };
        // Passed with a failure, failed without one, passed with a
        // cancellation: rejected.
        fails(&run("passed", "failed"), "outcome", "inconsistent_outcome");
        fails(
            &run("passed", "timed_out"),
            "outcome",
            "inconsistent_outcome",
        );
        fails(&run("failed", "passed"), "outcome", "inconsistent_outcome");
        fails(
            &run("passed", "cancelled"),
            "outcome",
            "inconsistent_outcome",
        );
        fails(
            &run("cancelled", "failed"),
            "outcome",
            "inconsistent_outcome",
        );
        // A signal after the last job: cancelled with every result passed.
        assert_eq!(check(&run("cancelled", "passed")), Ok(()));
        assert_eq!(check(&run("failed", "timed_out")), Ok(()));
        assert_eq!(check(&run("cancelled", "cancelled")), Ok(()));
        assert_eq!(check(&run("passed", "skipped")), Ok(()));
    }

    #[test]
    fn source_and_ci_follow_their_rules() {
        let branch = |b: Value| with(FULL, |r| r["source"]["branch"] = b);
        fails(
            &branch(json!("b".repeat(257))),
            "source.branch",
            "field_too_long",
        );
        assert_eq!(check(&branch(json!("b".repeat(256)))), Ok(()));
        fails(&branch(json!("a\nb")), "source.branch", "invalid_field");
        fails(&branch(json!("")), "source.branch", "invalid_field");
        fails(
            &with(FULL, |r| r["source"]["commit"] = json!("a".repeat(41))),
            "source.commit",
            "invalid_field",
        );
        assert_eq!(
            check(&with(FULL, |r| r["source"]["commit"] = json!("a".repeat(64)))),
            Ok(())
        );
        fails(
            &with(FULL, |r| remove(&mut r["ci"], "provider")),
            "ci.provider",
            "missing_field",
        );
        fails(
            &with(FULL, |r| r["ci"]["provider"] = json!("gitlab")),
            "ci.provider",
            "invalid_field",
        );
        fails(
            &with(FULL, |r| r["ci"]["attempt"] = json!(0)),
            "ci.attempt",
            "invalid_field",
        );
        fails(
            &with(FULL, |r| r["ci"]["pull_request"] = json!("42")),
            "ci.pull_request",
            "invalid_field",
        );
        fails(
            &with(FULL, |r| {
                r["ci"]["url"] = json!("http://ghe.local/a/b/actions/runs/1")
            }),
            "ci.url",
            "invalid_field",
        );
        fails(
            &with(FULL, |r| {
                r["ci"]["url"] = json!(format!("https://github.com/{}", "a".repeat(2030)))
            }),
            "ci.url",
            "field_too_long",
        );
        fails(
            &with(FULL, |r| r["ci"]["token"] = json!("x")),
            "ci",
            "unsupported_field",
        );
        fails(
            &with(FULL, |r| r["ci"]["run_id"] = json!("1".repeat(257))),
            "ci.run_id",
            "field_too_long",
        );
        assert_eq!(
            check(&with(FULL, |r| r["ci"] = json!({"provider": "github_actions"}))),
            Ok(())
        );
    }

    #[test]
    fn the_graph_is_well_formed_and_covers_the_results() {
        fails(
            &with(FULL, |r| r["graph"]["jobs"][0]["env"] = json!({})),
            "graph.jobs[0]",
            "unsupported_field",
        );
        fails(
            &with(FULL, |r| {
                r["graph"]["jobs"][1]["partitions"]["strategy"] = json!("hash")
            }),
            "graph.jobs[1].partitions.strategy",
            "invalid_field",
        );
        fails(
            &with(FULL, |r| remove(&mut r["graph"]["jobs"][0], "steps")),
            "graph.jobs[0].steps",
            "missing_field",
        );
        fails(
            &with(FULL, |r| r["graph"]["jobs"][2]["name"] = json!("build")),
            "graph.jobs[2].name",
            "duplicate_name",
        );
        fails(
            &with(FULL, |r| {
                r["graph"]["jobs"][1]["depends_on"] = json!(["deploy"])
            }),
            "graph.jobs[1].depends_on[0]",
            "unknown_dependency",
        );
        // A two-job cycle: build -> test -> build.
        fails(
            &with(FULL, |r| {
                r["graph"]["jobs"][0]["depends_on"] = json!(["test"])
            }),
            "graph.jobs[1].depends_on",
            "dependency_cycle",
        );
        fails(
            &with(FULL, |r| {
                r["graph"]["jobs"][0]["depends_on"] = json!(["build"])
            }),
            "graph.jobs[0].depends_on",
            "dependency_cycle",
        );
        fails(
            &with(FULL, |r| r["results"][0]["name"] = json!("deploy")),
            "results[0].name",
            "unknown_job",
        );
        // A graph is optional, and without one results are not cross-checked.
        assert_eq!(
            check(&with(FULL, |r| {
                remove(r, "graph");
                r["results"][0]["name"] = json!("deploy");
            })),
            Ok(())
        );
        let call = |change: &dyn Fn(&mut Value)| {
            with(FULL, |r| change(&mut r["graph"]["jobs"][2]["call"]))
        };
        fails(
            &call(&|c| c["graph_sha256"] = json!("AB".repeat(32))),
            "graph.jobs[2].call.graph_sha256",
            "invalid_field",
        );
        fails(
            &call(&|c| c["graph_sha256"] = json!("ab")),
            "graph.jobs[2].call.graph_sha256",
            "invalid_field",
        );
        fails(
            &call(&|c| remove(c, "workflow")),
            "graph.jobs[2].call.workflow",
            "missing_field",
        );
        fails(
            &call(&|c| c["jobs"] = json!([])),
            "graph.jobs[2].call",
            "unsupported_field",
        );
        fails(
            &call(&|c| *c = Value::Null),
            "graph.jobs[2].call",
            "null_field",
        );
    }

    #[test]
    fn workflow_call_fields_come_in_pairs() {
        fails(
            &with(CHILD, |r| remove(r, "caller_job")),
            "parent_run_id",
            "unpaired_field",
        );
        fails(
            &with(CHILD, |r| remove(r, "parent_run_id")),
            "caller_job",
            "unpaired_field",
        );
        fails(
            &with(CHILD, |r| r["parent_run_id"] = Value::Null),
            "parent_run_id",
            "null_field",
        );
        fails(
            &with(CHILD, |r| r["caller_job"] = Value::Null),
            "caller_job",
            "null_field",
        );
        fails(
            &with(CHILD, |r| {
                r["parent_run_id"] = json!("FB7D2711-6B67-4F5A-9104-492032A9E1E5")
            }),
            "parent_run_id",
            "invalid_field",
        );
        fails(
            &with(FULL, |r| remove(&mut r["results"][3], "workflow")),
            "results[3].child_run_id",
            "unpaired_field",
        );
        fails(
            &with(FULL, |r| r["results"][3]["child_run_id"] = Value::Null),
            "results[3].child_run_id",
            "null_field",
        );
        fails(
            &with(FULL, |r| r["results"][3]["workflow"] = Value::Null),
            "results[3].workflow",
            "null_field",
        );
        // A call that never activated a child has `workflow` only.
        assert_eq!(
            check(&with(FULL, |r| remove(
                &mut r["results"][3],
                "child_run_id"
            ))),
            Ok(())
        );
    }

    /// `MINIMAL` padded with passed results to encode to exactly `len` bytes.
    fn sized(len: usize) -> Value {
        let result = |error: usize| json!({"name":"a","status":"passed","exit_code":null,"command":null,"error":"x".repeat(error),"duration_ms":0,"started":true,"blocked_ms":0,"queued_ms":0});
        let mut record = parse(MINIMAL);
        let empty = serde_json::to_vec(&record).unwrap().len();
        let entry = serde_json::to_vec(&result(MAX_TEXT_BYTES)).unwrap().len() + 1;
        // Full-size results joined by commas, then shorten errors from the end.
        let count = (len - empty + 1).div_ceil(entry);
        let mut errors = vec![MAX_TEXT_BYTES; count];
        let mut excess = empty + count * entry - 1 - len;
        for error in errors.iter_mut().rev() {
            let cut = excess.min(*error);
            *error -= cut;
            excess -= cut;
        }
        record["results"] = errors.into_iter().map(result).collect();
        assert_eq!(serde_json::to_vec(&record).unwrap().len(), len);
        record
    }

    #[test]
    fn document_wide_limits() {
        fails(&sized(MAX_BODY_BYTES + 1), "body", "too_large");
        assert_eq!(check(&sized(MAX_BODY_BYTES)), Ok(()));
        fails(
            &with(FULL, |r| r["workflow"] = json!("c\u{0}i")),
            "workflow",
            "nul_character",
        );
        fails(
            &with(FULL, |r| r["results"][0]["error"] = json!("oops\u{0}")),
            "results[0].error",
            "nul_character",
        );
        // The body is level 1, so eight nested arrays make depth 9.
        let nested = |levels: usize| (0..levels).fold(json!(1), |inner, _| json!([inner]));
        fails(&with(FULL, |r| r["deep"] = nested(8)), "body", "too_deep");
        fails(
            &with(FULL, |r| r["deep"] = nested(7)),
            "body",
            "unsupported_field",
        );
        let duration = |value: Value| with(FULL, |r| r["duration_ms"] = value);
        assert_eq!(check(&duration(json!(MAX_SAFE_INTEGER))), Ok(()));
        for value in [json!(MAX_SAFE_INTEGER + 1), json!(u64::MAX), parse("1e20")] {
            fails(&duration(value), "duration_ms", "unsafe_integer");
        }
        fails(
            &with(FULL, |r| {
                r["results"][0]["exit_code"] = json!(-(1i64 << 53))
            }),
            "results[0].exit_code",
            "unsafe_integer",
        );
    }
}
