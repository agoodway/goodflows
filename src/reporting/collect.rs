//! Bounded, structural collection of `exunit-timings` JSONL reports.
//!
//! Only direct regular `*.jsonl` files in a job's own report directory are
//! read. Rejected files are never modified and are reported by structural code
//! only; record contents never appear in diagnostics. Every server vocabulary
//! enforced here comes from [`super::contract`].

use super::contract::{self, *};
use super::Source;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Map, Value};
use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub use contract::{MAX_RECORDS, MAX_REQUEST_BYTES};

pub const MAX_REPORTS_PER_JOB: usize = 256;
/// Bounds the work spent on `*.jsonl` names that are not readable regular files.
pub const MAX_ENTRIES_PER_JOB: usize = 16 * MAX_REPORTS_PER_JOB;
pub const MAX_JOB_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
/// Compact formatter output re-serializes to about its own size, so a source
/// file more than this allowance over the request limit cannot fit one request
/// and is rejected without being parsed.
pub const SOURCE_ALLOWANCE_BYTES: u64 = 1024 * 1024;
pub const MAX_SOURCE_BYTES_PER_REPORT: u64 = MAX_REQUEST_BYTES as u64 + SOURCE_ALLOWANCE_BYTES;
/// A record sits at level 4 of the body (body -> report -> records -> record),
/// so its own nesting, counting the record object as level 1, may be at most 13.
const MAX_RECORD_DEPTH: usize = MAX_BODY_DEPTH - 3;

pub struct JobContext<'a> {
    pub workflow_run_id: &'a str,
    pub workflow: &'a str,
    pub environment: Option<&'a str>,
    pub job: &'a str,
    pub job_status: &'a str,
    pub source: &'a Source,
}

/// Receives each valid request body as soon as it is built, so at most one
/// body is held in memory during collection.
pub trait Sink {
    /// Persist a frozen request body, returning its snapshot index.
    fn freeze(&mut self, body: &[u8]) -> Result<u32, ()>;
    /// Remove a body frozen earlier in this collection (a duplicate identity).
    fn discard(&mut self, index: u32);
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frozen {
    pub report_id: String,
    pub index: u32,
}

pub struct Collection {
    pub outcome: &'static str,
    pub rejected: BTreeMap<String, usize>,
    pub reports: Vec<Frozen>,
}

impl Collection {
    pub fn not_executed() -> Self {
        Self::empty("not_executed")
    }

    pub fn empty(outcome: &'static str) -> Self {
        Self {
            outcome,
            rejected: BTreeMap::new(),
            reports: Vec::new(),
        }
    }

    pub fn reject(&mut self, code: &str) {
        self.reject_n(code, 1);
    }

    fn reject_n(&mut self, code: &str, n: usize) {
        if n > 0 {
            *self.rejected.entry(code.into()).or_default() += n;
        }
    }
}

/// Collect every valid report from one executed job's directory, freezing each
/// into `sink` as it is built. `stop` is checked before each file; once it
/// returns true the remaining files are left unread (`collection_interrupted`).
pub fn collect_dir(
    dir: &Path,
    ctx: &JobContext,
    sink: &mut dyn Sink,
    stop: &dyn Fn() -> bool,
) -> Collection {
    // The directory itself must be a real directory, never a symlink to elsewhere.
    match std::fs::symlink_metadata(dir) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            let mut collection = Collection::empty("missing_directory");
            collection.reject("not_directory");
            return collection;
        }
        Err(_) => return Collection::empty("missing_directory"),
    }
    let mut collection = Collection::empty("collected");
    let Ok(entries) = std::fs::read_dir(dir) else {
        collection.outcome = "missing_directory";
        collection.reject("unreadable_directory");
        return collection;
    };
    let mut names = Vec::new();
    for entry in entries.flatten() {
        let name = entry.file_name();
        match name.to_str() {
            Some(name) if name.ends_with(".jsonl") => names.push(name.to_owned()),
            Some(_) => {}
            None => {
                if name.as_encoded_bytes().ends_with(b".jsonl") {
                    collection.reject("invalid_file_name");
                }
            }
        }
    }
    names.sort();
    let mut used: u64 = 0;
    let mut read = 0;
    // Every parsed identity, frozen or not, so a duplicate is never chosen.
    let mut parsed: Vec<(String, Option<u32>)> = Vec::new();
    for (index, name) in names.iter().enumerate() {
        if stop() {
            collection.outcome = "interrupted";
            collection.reject_n("collection_interrupted", names.len() - index);
            break;
        }
        if index >= MAX_ENTRIES_PER_JOB {
            collection.reject("entry_limit");
            continue;
        }
        let path = dir.join(name);
        // Only regular files that were read count toward the report cap, so
        // symlinks and other junk cannot crowd out valid reports.
        if read >= MAX_REPORTS_PER_JOB {
            collection.reject(match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => "symlink",
                Ok(meta) if !meta.is_file() => "not_regular_file",
                Ok(_) => "report_limit",
                Err(_) => "unreadable_file",
            });
            continue;
        }
        let bytes = match read_regular(&path, MAX_JOB_SOURCE_BYTES - used) {
            Ok(bytes) => bytes,
            Err(code) => {
                collection.reject(code);
                continue;
            }
        };
        used += bytes.len() as u64;
        read += 1;
        let built = parse_report(&bytes).and_then(|report| {
            let body = envelope(ctx, &report)?;
            Ok((report.report_id, body))
        });
        drop(bytes);
        match built {
            Ok((report_id, body)) => {
                let frozen = sink.freeze(&body).ok();
                if frozen.is_none() {
                    collection.reject("snapshot_write_failed");
                }
                parsed.push((report_id, frozen));
            }
            Err(code) => collection.reject(code),
        }
    }
    // The server identity excludes suite_id, so a reused report_id cannot be disambiguated.
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for (report_id, _) in &parsed {
        *counts.entry(report_id.as_str()).or_default() += 1;
    }
    for (report_id, frozen) in &parsed {
        if counts[report_id.as_str()] > 1 {
            if let Some(index) = frozen {
                sink.discard(*index);
            }
            collection.reject("duplicate_report_id");
        } else if let Some(index) = frozen {
            collection.reports.push(Frozen {
                report_id: report_id.clone(),
                index: *index,
            });
        }
    }
    if collection.reports.is_empty() && collection.rejected.is_empty() {
        collection.outcome = "no_reports";
    }
    collection
}

/// Open a path without following a final symlink.
pub fn open_nofollow(path: &Path) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    // O_NOFOLLOW closes the race between a symlink check and opening the file;
    // O_NONBLOCK keeps a FIFO from blocking the open.
    #[cfg(unix)]
    options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    options.open(path)
}

/// Read a regular file (never a symlink) of at most `limit` bytes.
pub fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, &'static str> {
    let meta = std::fs::symlink_metadata(path).map_err(|_| "unreadable_file")?;
    if meta.file_type().is_symlink() {
        return Err("symlink");
    }
    if !meta.is_file() {
        return Err("not_regular_file");
    }
    let file = open_nofollow(path).map_err(|_| "unreadable_file")?;
    let meta = file.metadata().map_err(|_| "unreadable_file")?;
    if !meta.is_file() {
        return Err("not_regular_file");
    }
    if meta.len() > limit {
        return Err("too_large");
    }
    let mut bytes = Vec::with_capacity(meta.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "unreadable_file")?;
    if bytes.len() as u64 > limit {
        return Err("too_large");
    }
    Ok(bytes)
}

fn read_regular(path: &Path, remaining: u64) -> Result<Vec<u8>, &'static str> {
    // Size checks happen on the opened file before any content is read.
    let limit = remaining.min(MAX_SOURCE_BYTES_PER_REPORT);
    match read_bounded(path, limit) {
        Err("too_large") => {
            let len = std::fs::symlink_metadata(path)
                .map(|m| m.len())
                .unwrap_or(u64::MAX);
            Err(if len > remaining {
                "job_byte_limit"
            } else {
                "request_too_large"
            })
        }
        other => other,
    }
}

#[derive(Debug)]
pub struct Report {
    pub suite_id: String,
    pub report_id: String,
    pub partition: Value,
    pub partition_count: Value,
    pub truncated: bool,
    pub records: Vec<Value>,
}

/// Counts serialized bytes without storing them.
struct Count(usize);

impl std::io::Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// Parse ordered JSONL. A final unterminated invalid fragment is dropped and
/// marks the report truncated; any other invalid line rejects the report.
/// Parsing stops as soon as the records alone could not fit one request.
pub fn parse_report(bytes: &[u8]) -> Result<Report, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "invalid_utf8")?;
    let mut lines = text.split('\n').peekable();
    let mut records = Vec::new();
    let mut truncated = false;
    // Exact compact size of the records array contents built so far.
    let mut size = 0usize;
    while let Some(line) = lines.next() {
        // Only the segment after the last newline can be an unterminated line.
        let last = lines.peek().is_none();
        if line.trim().is_empty() {
            continue;
        }
        match decode(line, MAX_RECORD_DEPTH) {
            Ok(value @ Value::Object(_)) => {
                if records.len() == MAX_RECORDS {
                    return Err("record_limit");
                }
                let mut count = Count(0);
                serde_json::to_writer(&mut count, &value).map_err(|_| "serialization_failed")?;
                size += count.0 + usize::from(!records.is_empty());
                if size > MAX_REQUEST_BYTES {
                    return Err("request_too_large");
                }
                records.push(value);
            }
            Ok(_) => return Err("malformed_record"),
            // A rule violation can surface before the parser reaches the cut, so
            // a final fragment is recognized by its syntax alone.
            Err(_) if last && serde_json::from_str::<IgnoredAny>(line).is_err() => truncated = true,
            Err(code) => return Err(code),
        }
    }
    validate_records(&records)?;
    let first = &records[0];
    Ok(Report {
        suite_id: first["run_id"].as_str().unwrap_or_default().to_owned(),
        report_id: first["report_id"].as_str().unwrap_or_default().to_owned(),
        partition: first.get("partition").cloned().unwrap_or(Value::Null),
        partition_count: first.get("partition_count").cloned().unwrap_or(Value::Null),
        truncated,
        records,
    })
}

/// Decode one JSON document, enforcing the D1 rules that decoding into a
/// `serde_json::Value` would otherwise lose: duplicate keys (which would be
/// silently collapsed), nesting depth, and integer-only numbers.
fn decode(text: &str, max_depth: usize) -> Result<Value, &'static str> {
    let violation = Cell::new(None);
    let mut deserializer = serde_json::Deserializer::from_str(text);
    // Strict stops at MAX_BODY_DEPTH (16), far below serde_json's own
    // recursion limit, so the structural code always wins.
    let value = Strict {
        depth: 0,
        max_depth,
        violation: &violation,
    }
    .deserialize(&mut deserializer)
    .and_then(|value| deserializer.end().map(|()| value));
    value.map_err(|_| violation.get().unwrap_or("malformed_record"))
}

#[derive(Clone, Copy)]
struct Strict<'a> {
    /// Nesting level of the enclosing container (0 at the top of a document).
    depth: usize,
    max_depth: usize,
    violation: &'a Cell<Option<&'static str>>,
}

impl Strict<'_> {
    fn fail<E: serde::de::Error>(&self, code: &'static str) -> E {
        if self.violation.get().is_none() {
            self.violation.set(Some(code));
        }
        E::custom(code)
    }

    fn nested<E: serde::de::Error>(&self) -> Result<Self, E> {
        if self.depth + 1 > self.max_depth {
            return Err(self.fail("nested_too_deeply"));
        }
        Ok(Strict {
            depth: self.depth + 1,
            ..*self
        })
    }
}

impl<'de> DeserializeSeed<'de> for Strict<'_> {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for Strict<'_> {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a JSON value")
    }

    fn visit_bool<E>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }

    fn visit_i64<E>(self, v: i64) -> Result<Value, E> {
        // serde_json only uses i64 for negative integers; the server is the
        // authority on their range.
        Ok(Value::from(v))
    }

    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Value, E> {
        if v > MAX_SAFE_INTEGER {
            return Err(self.fail("integer_out_of_range"));
        }
        Ok(Value::from(v))
    }

    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Value, E> {
        // Any fraction or exponent (and integers beyond 64 bits, also far over
        // 2^53-1) decodes as f64; the contract allows integers only.
        Err(self.fail("non_integer_number"))
    }

    fn visit_str<E>(self, v: &str) -> Result<Value, E> {
        Ok(Value::String(v.to_owned()))
    }

    fn visit_string<E>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }

    fn visit_unit<E>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let inner = self.nested()?;
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(inner)? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let inner = self.nested()?;
        let mut object = Map::new();
        while let Some(key) = map.next_key::<String>()? {
            if object.contains_key(&key) {
                return Err(self.fail("duplicate_key"));
            }
            let value = map.next_value_seed(inner)?;
            object.insert(key, value);
        }
        Ok(Value::Object(object))
    }
}

/// Build the request body with exactly the server contract's top-level keys.
pub fn envelope(ctx: &JobContext, report: &Report) -> Result<Vec<u8>, &'static str> {
    let body = json!({
        "format": REPORT_FORMAT,
        "workflow_run_id": ctx.workflow_run_id,
        "workflow": ctx.workflow,
        "environment": ctx.environment,
        "job": ctx.job,
        "job_status": ctx.job_status,
        "source": {
            "commit": ctx.source.commit,
            "branch": ctx.source.branch,
            "dirty": ctx.source.dirty,
        },
        "report": {
            "suite_id": report.suite_id,
            "report_id": report.report_id,
            "partition": report.partition,
            "partition_count": report.partition_count,
            "truncated": report.truncated,
            "records": report.records,
        },
    });
    let bytes = serde_json::to_vec(&body).map_err(|_| "serialization_failed")?;
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("request_too_large");
    }
    Ok(bytes)
}

/// The identifying metadata of a validated request snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Head {
    pub workflow_run_id: String,
    pub workflow: String,
    pub environment: Option<String>,
    pub job: String,
    pub job_status: String,
    pub source: Source,
    pub report_id: String,
}

fn exact_keys(map: &Map<String, Value>, keys: &[&str]) -> bool {
    map.len() == keys.len() && keys.iter().all(|k| map.contains_key(*k))
}

/// Re-validate a frozen request snapshot against the full local contract
/// before it is sent, so a modified or foreign file is never uploaded.
pub fn validate_envelope(bytes: &[u8]) -> Result<Head, &'static str> {
    let invalid = "invalid_envelope";
    if bytes.len() > MAX_REQUEST_BYTES {
        return Err("request_too_large");
    }
    let text = std::str::from_utf8(bytes).map_err(|_| "invalid_utf8")?;
    let body = decode(text, MAX_BODY_DEPTH)?;
    if contains_nul(&body) {
        return Err("nul_character");
    }
    let Value::Object(body) = body else {
        return Err(invalid);
    };
    if !exact_keys(&body, ENVELOPE) || body["format"] != REPORT_FORMAT {
        return Err(invalid);
    }
    let string = |value: &Value| value.as_str().map(str::to_owned).ok_or(invalid);
    let name = |value: &Value| {
        string(value).and_then(|s| {
            crate::config::valid_report_name(&s)
                .then_some(s)
                .ok_or(invalid)
        })
    };
    let workflow_run_id = string(&body["workflow_run_id"])?;
    if !super::canonical_uuid(&workflow_run_id) {
        return Err(invalid);
    }
    let environment = match &body["environment"] {
        Value::Null => None,
        value => Some(name(value)?),
    };
    let job_status = string(&body["job_status"])?;
    if !JOB_STATUSES.contains(&job_status.as_str()) {
        return Err(invalid);
    }
    let Value::Object(source) = &body["source"] else {
        return Err(invalid);
    };
    if !exact_keys(source, SOURCE) {
        return Err(invalid);
    }
    let optional = |value: &Value, valid: fn(&str) -> Option<String>| match value {
        Value::Null => Ok(None),
        Value::String(s) => valid(s).map(Some).ok_or(invalid),
        _ => Err(invalid),
    };
    let source = Source {
        commit: optional(&source["commit"], super::source::valid_commit)?,
        branch: optional(&source["branch"], super::source::valid_branch)?,
        dirty: match &source["dirty"] {
            Value::Null => None,
            Value::Bool(dirty) => Some(*dirty),
            _ => return Err(invalid),
        },
    };
    let Value::Object(report) = &body["report"] else {
        return Err(invalid);
    };
    if !exact_keys(report, REPORT) || !report["truncated"].is_boolean() {
        return Err(invalid);
    }
    let records = report["records"].as_array().ok_or(invalid)?;
    if records.len() > MAX_RECORDS {
        return Err("record_limit");
    }
    validate_records(records)?;
    let first = &records[0];
    let initial = |key: &str| first.get(key).unwrap_or(&Value::Null);
    if report["suite_id"] != first["run_id"]
        || report["report_id"] != first["report_id"]
        || report["partition"] != *initial("partition")
        || report["partition_count"] != *initial("partition_count")
    {
        return Err("identity_mismatch");
    }
    Ok(Head {
        workflow_run_id,
        workflow: name(&body["workflow"])?,
        environment,
        job: name(&body["job"])?,
        job_status,
        source,
        report_id: string(&report["report_id"])?,
    })
}

pub fn valid_identifier(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 128
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn valid_partition(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::String(p) => {
            !p.is_empty()
                && p.len() <= 64
                && p.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        }
        _ => false,
    }
}

fn valid_partition_count(value: &Value) -> bool {
    match value {
        Value::Null => true,
        Value::Number(n) => n
            .as_u64()
            .is_some_and(|n| (1..=MAX_SAFE_INTEGER).contains(&n)),
        _ => false,
    }
}

fn contains_nul(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains('\0'),
        Value::Array(items) => items.iter().any(contains_nul),
        Value::Object(map) => map.iter().any(|(k, v)| k.contains('\0') || contains_nul(v)),
        _ => false,
    }
}

fn keys_within(value: &Value, allowed: &[&str]) -> bool {
    match value {
        Value::Object(map) => map.keys().all(|k| allowed.contains(&k.as_str())),
        _ => true,
    }
}

fn too_long(map: &Map<String, Value>) -> bool {
    let over = |fields: &[&str], limit: usize| {
        fields.iter().any(|f| {
            map.get(*f)
                .and_then(Value::as_str)
                .is_some_and(|s| s.len() > limit)
        })
    };
    over(LONG_FIELDS, 4096) || over(VERSION_FIELDS, 128)
}

fn validate_nested(map: &Map<String, Value>) -> Result<(), &'static str> {
    let unsupported = Err("unsupported_field");
    if map.get("counts").is_some_and(|c| !keys_within(c, OUTCOMES)) {
        return unsupported;
    }
    if map.get("times_us").is_some_and(|t| !keys_within(t, TIMES)) {
        return unsupported;
    }
    if let Some(filters) = map.get("filters") {
        if !keys_within(filters, &["include", "exclude"]) {
            return unsupported;
        }
        for list in ["include", "exclude"] {
            for entry in filters[list].as_array().into_iter().flatten() {
                if !keys_within(entry, &["tag", "value"]) {
                    return unsupported;
                }
                // Only allowlisted boolean tags; filters never carry arbitrary tags.
                let tag = entry.get("tag").and_then(Value::as_str);
                if !tag.is_some_and(|tag| FILTER_TAGS.contains(&tag))
                    || !entry.get("value").is_some_and(Value::is_boolean)
                {
                    return Err("unsupported_filter");
                }
            }
        }
    }
    for file in map
        .get("files")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let file_keys: Vec<&str> = BUCKET.iter().chain(FILE_EXTRA).copied().collect();
        if !keys_within(file, &file_keys) {
            return unsupported;
        }
        if let Value::Object(file) = file {
            if file
                .get("file")
                .and_then(Value::as_str)
                .is_some_and(|f| f.len() > 4096)
            {
                return Err("field_too_long");
            }
            for bucket in ["async", "sync", "unknown"] {
                if let Some(bucket) = file.get(bucket) {
                    if !keys_within(bucket, BUCKET) {
                        return unsupported;
                    }
                    if bucket
                        .get("counts")
                        .is_some_and(|c| !keys_within(c, OUTCOMES))
                    {
                        return unsupported;
                    }
                }
            }
            if file
                .get("counts")
                .is_some_and(|c| !keys_within(c, OUTCOMES))
            {
                return unsupported;
            }
        }
    }
    Ok(())
}

/// Structural checks shared with the server contract. The server remains the
/// authority on full type validation; these checks stop locally knowable
/// rejections before a request is ever frozen.
fn validate_records(records: &[Value]) -> Result<(), &'static str> {
    let Some(first) = records.first() else {
        return Err("empty_report");
    };
    if first["event"] != "suite_started" {
        return Err("missing_suite_started");
    }
    if first["format"] != REPORT_FORMAT {
        return Err("unsupported_format");
    }
    let suite_id = first["run_id"].as_str().ok_or("invalid_identifier")?;
    let report_id = first["report_id"].as_str().ok_or("invalid_identifier")?;
    if !valid_identifier(suite_id) || !valid_identifier(report_id) {
        return Err("invalid_identifier");
    }
    if !valid_partition(first.get("partition").unwrap_or(&Value::Null)) {
        return Err("invalid_partition");
    }
    if !valid_partition_count(first.get("partition_count").unwrap_or(&Value::Null)) {
        return Err("invalid_partition_count");
    }
    let last = records.len() - 1;
    for (index, record) in records.iter().enumerate() {
        let Value::Object(map) = record else {
            return Err("malformed_record");
        };
        if contains_nul(record) {
            return Err("nul_character");
        }
        let event = map
            .get("event")
            .and_then(Value::as_str)
            .ok_or("missing_field")?;
        for key in map.keys() {
            match contract::allowed(event, key) {
                None => return Err("unsupported_event"),
                Some(false) => return Err("unsupported_field"),
                Some(true) => {}
            }
        }
        if map.get("run_id").and_then(Value::as_str) != Some(suite_id)
            || map.get("report_id").and_then(Value::as_str) != Some(report_id)
        {
            return Err("identity_mismatch");
        }
        match map.get("received_offset_us").and_then(Value::as_u64) {
            Some(offset) if offset <= MAX_SAFE_INTEGER => {}
            _ => return Err("invalid_field"),
        }
        if event == "suite_started" && index != 0 {
            return Err("invalid_order");
        }
        if event == "suite_finished" && index != last {
            return Err("invalid_order");
        }
        if too_long(map) {
            return Err("field_too_long");
        }
        validate_nested(map)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    pub fn suite_started(report_id: &str, partition: Value) -> Value {
        json!({"event":"suite_started","run_id":"suite-1","report_id":report_id,"received_offset_us":0,
            "format":"exunit-timings","started_at":"2026-11-01T00:00:00Z","partition":partition,
            "partition_count":3,"seed":1,"max_cases":8,"schedulers":8,"elixir_version":"1.18.0",
            "otp_release":"27","filters":{"include":[],"exclude":[{"tag":"chrome","value":true}]}})
    }

    fn test_finished(report_id: &str, outcome: &str) -> Value {
        json!({"event":"test_finished","run_id":"suite-1","report_id":report_id,"received_offset_us":10,
            "module":"MyTest","name":"test works","description":null,"file":"test/my_test.exs","line":3,
            "describe":null,"async":true,"parameter_id":null,"execution_id":1,"started_offset_us":5,
            "duration_us":42,"outcome":outcome})
    }

    fn suite_finished(report_id: &str) -> Value {
        let bucket = json!({"test_count":1,"module_count":1,"counts":{"passed":1},"test_duration_us":42,"observed_module_duration_us":50});
        json!({"event":"suite_finished","run_id":"suite-1","report_id":report_id,"received_offset_us":99,
            "aborted":false,"unfinished_tests":0,"unfinished_modules":0,"counts":{"passed":1,"failed":0},
            "files":[{"file":"test/my_test.exs","test_count":1,"module_count":1,"counts":{"passed":1},
                "test_duration_us":42,"observed_module_duration_us":50,"async":bucket,"sync":bucket,"unknown":bucket}],
            "times_us":{"run":100,"async":60,"sync":40,"load":null,"total":100}})
    }

    pub fn jsonl(records: &[Value]) -> Vec<u8> {
        records
            .iter()
            .map(|r| format!("{r}\n"))
            .collect::<String>()
            .into_bytes()
    }

    pub fn complete(report_id: &str) -> Vec<u8> {
        jsonl(&[
            suite_started(report_id, json!("1")),
            test_finished(report_id, "passed"),
            suite_finished(report_id),
        ])
    }

    /// Keeps frozen bodies in memory, as the spool writer keeps them on disk.
    #[derive(Default)]
    pub struct MemorySink {
        pub bodies: BTreeMap<u32, Vec<u8>>,
        next: u32,
        pub fail: bool,
    }

    impl Sink for MemorySink {
        fn freeze(&mut self, body: &[u8]) -> Result<u32, ()> {
            if self.fail {
                return Err(());
            }
            let index = self.next;
            self.next += 1;
            self.bodies.insert(index, body.to_vec());
            Ok(index)
        }

        fn discard(&mut self, index: u32) {
            self.bodies.remove(&index);
        }
    }

    fn collect(dir: &Path, ctx: &JobContext) -> (Collection, MemorySink) {
        let mut sink = MemorySink::default();
        let collection = collect_dir(dir, ctx, &mut sink, &|| false);
        (collection, sink)
    }

    fn ctx<'a>(source: &'a Source, status: &'a str) -> JobContext<'a> {
        JobContext {
            workflow_run_id: "67f7946d-136b-4608-ab01-e1f251a09754",
            workflow: "default",
            environment: None,
            job: "test",
            job_status: status,
            source,
        }
    }

    #[test]
    fn complete_report_produces_full_contract_envelope() {
        let source = Source::default();
        let report = parse_report(&complete("report-1")).unwrap();
        assert!(!report.truncated);
        let body: Value =
            serde_json::from_slice(&envelope(&ctx(&source, "passed"), &report).unwrap()).unwrap();
        let keys: Vec<_> = body
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        let mut expected = vec![
            "format",
            "workflow_run_id",
            "workflow",
            "environment",
            "job",
            "job_status",
            "source",
            "report",
        ];
        expected.sort();
        assert_eq!(keys, expected);
        assert!(body.get("project").is_none() && body.get("account_id").is_none());
        assert_eq!(
            body["workflow_run_id"],
            "67f7946d-136b-4608-ab01-e1f251a09754"
        );
        assert_eq!(
            body["source"],
            json!({"commit":null,"branch":null,"dirty":null})
        );
        assert_eq!(body["report"]["suite_id"], "suite-1");
        assert_eq!(body["report"]["report_id"], "report-1");
        assert_eq!(body["report"]["partition"], "1");
        assert_eq!(body["report"]["partition_count"], 3);
        assert_eq!(body["report"]["truncated"], false);
        assert_eq!(body["report"]["records"].as_array().unwrap().len(), 3);
        assert_eq!(body["report"]["records"][1]["duration_us"], 42);
    }

    #[test]
    fn failed_and_unknown_job_statuses_are_carried() {
        let source = Source::default();
        let report = parse_report(&complete("r")).unwrap();
        for status in ["failed", "unknown"] {
            let body: Value =
                serde_json::from_slice(&envelope(&ctx(&source, status), &report).unwrap()).unwrap();
            assert_eq!(body["job_status"], status);
        }
        let failed = jsonl(&[
            suite_started("r", Value::Null),
            test_finished("r", "failed"),
            suite_finished("r"),
        ]);
        assert_eq!(
            parse_report(&failed).unwrap().records[1]["outcome"],
            "failed"
        );
    }

    #[test]
    fn partial_reports_stay_partial() {
        let partial = jsonl(&[suite_started("r", json!("2")), test_finished("r", "passed")]);
        let report = parse_report(&partial).unwrap();
        assert!(!report.truncated);
        assert_eq!(report.records.len(), 2);
        assert_ne!(report.records.last().unwrap()["event"], "suite_finished");
    }

    #[test]
    fn final_line_handling() {
        let mut unterminated = complete("r");
        unterminated.pop();
        let report = parse_report(&unterminated).unwrap();
        assert!(!report.truncated);
        assert_eq!(report.records.len(), 3);

        let mut fragment = jsonl(&[
            suite_started("r", Value::Null),
            test_finished("r", "passed"),
        ]);
        fragment.extend_from_slice(br#"{"event":"test_fini"#);
        let report = parse_report(&fragment).unwrap();
        assert!(report.truncated);
        assert_eq!(report.records.len(), 2);

        let mut earlier = jsonl(&[suite_started("r", Value::Null)]);
        earlier.extend_from_slice(b"{not json}\n");
        earlier.extend(jsonl(&[test_finished("r", "passed")]));
        assert_eq!(parse_report(&earlier).unwrap_err(), "malformed_record");

        let mut terminated_bad = jsonl(&[suite_started("r", Value::Null)]);
        terminated_bad.extend_from_slice(b"{bad\n");
        assert_eq!(
            parse_report(&terminated_bad).unwrap_err(),
            "malformed_record"
        );
    }

    #[test]
    fn identity_partition_and_field_rules() {
        let mut other = test_finished("r", "passed");
        other["report_id"] = json!("other");
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), other])).unwrap_err(),
            "identity_mismatch"
        );
        let mut other_suite = test_finished("r", "passed");
        other_suite["run_id"] = json!("suite-2");
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), other_suite])).unwrap_err(),
            "identity_mismatch"
        );

        for partition in [json!(""), json!("a b"), json!(1), json!("x".repeat(65))] {
            assert_eq!(
                parse_report(&jsonl(&[suite_started("r", partition)])).unwrap_err(),
                "invalid_partition"
            );
        }
        assert!(parse_report(&jsonl(&[suite_started("r", json!("p.1_a-B"))])).is_ok());
        let mut count = suite_started("r", Value::Null);
        count["partition_count"] = json!(0);
        assert_eq!(
            parse_report(&jsonl(&[count])).unwrap_err(),
            "invalid_partition_count"
        );

        for id in ["", "-r", "r r", &"r".repeat(129)] {
            assert_eq!(
                parse_report(&jsonl(&[suite_started(id, Value::Null)])).unwrap_err(),
                "invalid_identifier"
            );
        }
        assert!(parse_report(&jsonl(&[suite_started(&"r".repeat(128), Value::Null)])).is_ok());

        let mut extra = test_finished("r", "passed");
        extra["stacktrace"] = json!("boom");
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), extra])).unwrap_err(),
            "unsupported_field"
        );
        let mut nested = suite_finished("r");
        nested["times_us"]["wall"] = json!(1);
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), nested])).unwrap_err(),
            "unsupported_field"
        );
        let mut bucket = suite_finished("r");
        bucket["files"][0]["async"]["file"] = json!("x");
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), bucket])).unwrap_err(),
            "unsupported_field"
        );
        let log = json!({"event":"log","run_id":"suite-1","report_id":"r","received_offset_us":1});
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), log])).unwrap_err(),
            "unsupported_event"
        );

        let mut format = suite_started("r", Value::Null);
        format["format"] = json!("junit");
        assert_eq!(
            parse_report(&jsonl(&[format])).unwrap_err(),
            "unsupported_format"
        );
        assert_eq!(
            parse_report(&jsonl(&[test_finished("r", "passed")])).unwrap_err(),
            "missing_suite_started"
        );
        assert_eq!(
            parse_report(&jsonl(&[
                suite_started("r", Value::Null),
                suite_finished("r"),
                test_finished("r", "passed")
            ]))
            .unwrap_err(),
            "invalid_order"
        );
    }

    #[test]
    fn nul_and_byte_length_limits() {
        let mut nul = test_finished("r", "passed");
        nul["name"] = json!("a\u{0}b");
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), nul])).unwrap_err(),
            "nul_character"
        );
        let mut long = test_finished("r", "passed");
        long["module"] = json!("é".repeat(2049)); // 4098 bytes, 2049 chars
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), long])).unwrap_err(),
            "field_too_long"
        );
        let mut fits = test_finished("r", "passed");
        fits["module"] = json!("m".repeat(4096));
        assert!(parse_report(&jsonl(&[suite_started("r", Value::Null), fits])).is_ok());
        let mut version = suite_started("r", Value::Null);
        version["otp_release"] = json!("1".repeat(129));
        assert_eq!(
            parse_report(&jsonl(&[version])).unwrap_err(),
            "field_too_long"
        );
        assert_eq!(parse_report(b"\xff\n").unwrap_err(), "invalid_utf8");
    }

    #[test]
    fn record_and_request_limits() {
        let started = suite_started("r", Value::Null);
        let tiny = json!({"event":"interrupted","run_id":"suite-1","report_id":"r","received_offset_us":1});
        let mut records = vec![started.clone()];
        records.extend(std::iter::repeat_n(tiny.clone(), MAX_RECORDS - 1));
        assert!(parse_report(&jsonl(&records)).is_ok());
        records.push(tiny);
        assert_eq!(parse_report(&jsonl(&records)).unwrap_err(), "record_limit");

        let source = Source::default();
        let mut big = test_finished("r", "passed");
        big["description"] = json!("d".repeat(4000));
        let mut records = vec![started];
        records.extend(std::iter::repeat_n(big, MAX_REQUEST_BYTES / 4000 + 1));
        // Parsing stops as soon as the records alone exceed one request.
        assert_eq!(
            parse_report(&jsonl(&records)).unwrap_err(),
            "request_too_large"
        );
        // Records that fit can still be pushed over the limit by the envelope.
        let size = |r: &Value| r.to_string().len() + 1;
        let mut fits = vec![records[0].clone()];
        let mut used = size(&records[0]);
        while used + size(&records[1]) < MAX_REQUEST_BYTES - 150 {
            used += size(&records[1]);
            fits.push(records[1].clone());
        }
        let tiny = json!({"event":"interrupted","run_id":"suite-1","report_id":"r","received_offset_us":1});
        while used + size(&tiny) < MAX_REQUEST_BYTES - 50 {
            used += size(&tiny);
            fits.push(tiny.clone());
        }
        let report = parse_report(&jsonl(&fits)).unwrap();
        assert_eq!(
            envelope(&ctx(&source, "passed"), &report).unwrap_err(),
            "request_too_large"
        );
    }

    #[test]
    fn directory_collection_rules() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::default();
        let ctx = ctx(&source, "passed");
        let reports = dir.path().join("reports");
        assert_eq!(collect(&reports, &ctx).0.outcome, "missing_directory");
        std::fs::create_dir(&reports).unwrap();
        assert_eq!(collect(&reports, &ctx).0.outcome, "no_reports");

        std::fs::write(reports.join("a.jsonl"), complete("a")).unwrap();
        std::fs::write(reports.join("ignored.txt"), complete("x")).unwrap();
        std::fs::create_dir(reports.join("nested")).unwrap();
        std::fs::write(reports.join("nested/n.jsonl"), complete("n")).unwrap();
        let outside = dir.path().join("outside.jsonl");
        std::fs::write(&outside, complete("outside")).unwrap();
        std::os::unix::fs::symlink(&outside, reports.join("link.jsonl")).unwrap();
        std::fs::write(reports.join("dup1.jsonl"), complete("dup")).unwrap();
        std::fs::write(reports.join("dup2.jsonl"), complete("dup")).unwrap();
        let before = std::fs::read(reports.join("dup1.jsonl")).unwrap();
        let (collection, sink) = collect(&reports, &ctx);
        // Duplicates frozen before detection are discarded again.
        assert_eq!(sink.bodies.len(), 1);
        let ids: Vec<_> = collection
            .reports
            .iter()
            .map(|r| r.report_id.as_str())
            .collect();
        assert_eq!(ids, ["a"]);
        assert_eq!(collection.rejected["symlink"], 1);
        assert_eq!(collection.rejected["duplicate_report_id"], 2);
        assert_eq!(std::fs::read(reports.join("dup1.jsonl")).unwrap(), before);

        let linked = dir.path().join("linked");
        std::os::unix::fs::symlink(&reports, &linked).unwrap();
        let (collection, _) = collect(&linked, &ctx);
        assert_eq!(collection.outcome, "missing_directory");
        assert!(collection.reports.is_empty());
    }

    #[test]
    fn report_count_limit() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::default();
        for i in 0..MAX_REPORTS_PER_JOB + 2 {
            std::fs::write(
                dir.path().join(format!("{i:04}.jsonl")),
                complete(&format!("r{i}")),
            )
            .unwrap();
        }
        let (collection, _) = collect(dir.path(), &ctx(&source, "passed"));
        assert_eq!(collection.reports.len(), MAX_REPORTS_PER_JOB);
        assert_eq!(collection.rejected["report_limit"], 2);
    }

    fn line(record: &str) -> Vec<u8> {
        let mut bytes = jsonl(&[suite_started("r", Value::Null)]);
        bytes.extend_from_slice(record.as_bytes());
        bytes.push(b'\n');
        bytes
    }

    fn finished_with(field: &str, raw: &str) -> String {
        let record = test_finished("r", "passed").to_string();
        let marker = format!("\"{field}\":");
        let start = record.find(&marker).unwrap() + marker.len();
        let end = start + record[start..].find([',', '}']).unwrap();
        format!("{}{raw}{}", &record[..start], &record[end..])
    }

    fn depth(value: &Value) -> usize {
        match value {
            Value::Array(items) => 1 + items.iter().map(depth).max().unwrap_or(0),
            Value::Object(map) => 1 + map.values().map(depth).max().unwrap_or(0),
            _ => 0,
        }
    }

    #[test]
    fn duplicate_keys_are_rejected_not_collapsed() {
        let record = test_finished("r", "passed").to_string();
        let top = record.replacen("{", r#"{"outcome":"failed","#, 1);
        assert_eq!(parse_report(&line(&top)).unwrap_err(), "duplicate_key");
        let mut finished = suite_finished("r").to_string();
        finished = finished.replacen(r#""counts":{"#, r#""counts":{"passed":2,"#, 1);
        assert_eq!(parse_report(&line(&finished)).unwrap_err(), "duplicate_key");
        let mut started = suite_started("r", Value::Null).to_string();
        started = started.replacen("{", r#"{"seed":2,"#, 1);
        assert_eq!(
            parse_report(format!("{started}\n").as_bytes()).unwrap_err(),
            "duplicate_key"
        );
    }

    #[test]
    fn nesting_is_bounded_by_the_server_body_depth() {
        // The record is level 1, so 12 nested arrays reach record depth 13.
        let nested = |levels: usize| format!("{}{}", "[".repeat(levels), "]".repeat(levels));
        let fits = parse_report(&line(&finished_with("description", &nested(12)))).unwrap();
        let source = Source::default();
        let body: Value =
            serde_json::from_slice(&envelope(&ctx(&source, "passed"), &fits).unwrap()).unwrap();
        assert_eq!(depth(&body), MAX_BODY_DEPTH);
        assert_eq!(
            parse_report(&line(&finished_with("description", &nested(13)))).unwrap_err(),
            "nested_too_deeply"
        );
        // Deeper than serde_json's own recursion limit still gets the structural code.
        assert_eq!(
            parse_report(&line(&finished_with("description", &nested(500)))).unwrap_err(),
            "nested_too_deeply"
        );
        // The real contract's deepest structure stays well inside the limit.
        assert!(parse_report(&complete("r")).is_ok());
    }

    #[test]
    fn numbers_must_be_safe_integers() {
        for raw in [
            "42.0",
            "4.2",
            "4e1",
            "4E+1",
            "1e-3",
            "-1.5",
            "100000000000000000000",
        ] {
            assert_eq!(
                parse_report(&line(&finished_with("duration_us", raw))).unwrap_err(),
                "non_integer_number",
                "{raw}"
            );
        }
        assert_eq!(
            parse_report(&line(&finished_with("duration_us", "9007199254740992"))).unwrap_err(),
            "integer_out_of_range"
        );
        let max = parse_report(&line(&finished_with("duration_us", "9007199254740991"))).unwrap();
        assert_eq!(max.records[1]["duration_us"], MAX_SAFE_INTEGER);
        let mut seed = suite_started("r", Value::Null);
        seed["seed"] = json!(-5);
        assert!(parse_report(&jsonl(&[seed])).is_ok());
    }

    #[test]
    fn rule_violations_before_a_final_cut_still_truncate() {
        let mut fragment = jsonl(&[suite_started("r", Value::Null)]);
        fragment.extend_from_slice(br#"{"event":"test_finished","duration_us":1.5,"na"#);
        let report = parse_report(&fragment).unwrap();
        assert!(report.truncated);
        assert_eq!(report.records.len(), 1);
        // A complete final line that breaks a rule is a violation, not a fragment.
        let mut complete_bad = line(&finished_with("duration_us", "1.5"));
        complete_bad.pop();
        assert_eq!(
            parse_report(&complete_bad).unwrap_err(),
            "non_integer_number"
        );
    }

    #[test]
    fn filters_use_the_server_tag_allowlist() {
        for tag in FILTER_TAGS {
            let mut started = suite_started("r", Value::Null);
            started["filters"]["include"] = json!([{"tag": tag, "value": false}]);
            assert!(parse_report(&jsonl(&[started])).is_ok(), "{tag}");
        }
        for entry in [
            json!({"tag":"slow","value":true}),
            json!({"tag":"chrome","value":"yes"}),
            json!({"tag":"chrome","value":null}),
            json!({"tag":1,"value":true}),
            json!({"value":true}),
            json!({"tag":"chrome"}),
        ] {
            for list in ["include", "exclude"] {
                let mut started = suite_started("r", Value::Null);
                started["filters"][list] = json!([entry]);
                assert_eq!(
                    parse_report(&jsonl(&[started])).unwrap_err(),
                    "unsupported_filter",
                    "{entry}"
                );
            }
        }
    }

    #[test]
    fn junk_entries_do_not_count_toward_the_report_cap() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::default();
        let target = dir.path().join("target.jsonl");
        std::fs::write(&target, complete("t")).unwrap();
        let reports = dir.path().join("reports");
        std::fs::create_dir(&reports).unwrap();
        // Junk sorts first so it would have consumed the cap before valid reports.
        for i in 0..MAX_REPORTS_PER_JOB {
            let link = reports.join(format!("a-link-{i:04}.jsonl"));
            std::os::unix::fs::symlink(&target, link).unwrap();
        }
        std::fs::create_dir(reports.join("a-dir.jsonl")).unwrap();
        for i in 0..MAX_REPORTS_PER_JOB + 1 {
            std::fs::write(
                reports.join(format!("r-{i:04}.jsonl")),
                complete(&format!("r{i}")),
            )
            .unwrap();
        }
        let (collection, _) = collect(&reports, &ctx(&source, "passed"));
        assert_eq!(collection.reports.len(), MAX_REPORTS_PER_JOB);
        assert_eq!(collection.rejected["symlink"], MAX_REPORTS_PER_JOB);
        assert_eq!(collection.rejected["not_regular_file"], 1);
        assert_eq!(collection.rejected["report_limit"], 1);
    }

    #[test]
    fn total_entries_examined_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::default();
        for i in 0..MAX_ENTRIES_PER_JOB + 3 {
            std::os::unix::fs::symlink("/nonexistent", dir.path().join(format!("{i:05}.jsonl")))
                .unwrap();
        }
        let (collection, _) = collect(dir.path(), &ctx(&source, "passed"));
        assert!(collection.reports.is_empty());
        assert_eq!(collection.rejected["symlink"], MAX_ENTRIES_PER_JOB);
        assert_eq!(collection.rejected["entry_limit"], 3);
    }

    #[test]
    fn truncated_reports_say_so_in_the_request_body() {
        let source = Source::default();
        let mut fragment = jsonl(&[
            suite_started("r", Value::Null),
            test_finished("r", "passed"),
        ]);
        fragment.extend_from_slice(br#"{"event":"test_fin"#);
        let report = parse_report(&fragment).unwrap();
        let body = envelope(&ctx(&source, "failed"), &report).unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["report"]["truncated"], true);
        assert_eq!(value["report"]["records"].as_array().unwrap().len(), 2);
        assert!(validate_envelope(&body).is_ok());
    }

    #[test]
    fn frozen_envelopes_are_revalidated() {
        let source = Source {
            commit: Some("a".repeat(40)),
            branch: Some("main".into()),
            dirty: Some(false),
        };
        let body = envelope(
            &ctx(&source, "passed"),
            &parse_report(&complete("r")).unwrap(),
        )
        .unwrap();
        let head = validate_envelope(&body).unwrap();
        assert_eq!(
            head,
            Head {
                workflow_run_id: "67f7946d-136b-4608-ab01-e1f251a09754".into(),
                workflow: "default".into(),
                environment: None,
                job: "test".into(),
                job_status: "passed".into(),
                source: source.clone(),
                report_id: "r".into(),
            }
        );
        let value: Value = serde_json::from_slice(&body).unwrap();
        let tampered = |edit: &dyn Fn(&mut Value)| {
            let mut value = value.clone();
            edit(&mut value);
            validate_envelope(&serde_json::to_vec(&value).unwrap())
        };
        for (edit, code) in [
            (
                &(|v: &mut Value| v["project"] = json!("goodcases")) as &dyn Fn(&mut Value),
                "invalid_envelope",
            ),
            (
                &|v: &mut Value| {
                    v["workflow_run_id"] = json!("67F7946D-136B-4608-AB01-E1F251A09754")
                },
                "invalid_envelope",
            ),
            (
                &|v: &mut Value| v["job"] = json!("j".repeat(65)),
                "invalid_envelope",
            ),
            (
                &|v: &mut Value| v["job_status"] = json!("green"),
                "invalid_envelope",
            ),
            (
                &|v: &mut Value| v["environment"] = json!(""),
                "invalid_envelope",
            ),
            (
                &|v: &mut Value| v["source"]["remote"] = json!("git@x"),
                "invalid_envelope",
            ),
            (
                &|v: &mut Value| v["source"]["branch"] = json!(""),
                "invalid_envelope",
            ),
            (
                &|v: &mut Value| v["report"]["truncated"] = json!("no"),
                "invalid_envelope",
            ),
            (
                &|v: &mut Value| v["report"]["partition"] = json!("9"),
                "identity_mismatch",
            ),
            (
                &|v: &mut Value| v["report"]["report_id"] = json!("other"),
                "identity_mismatch",
            ),
            (
                &|v: &mut Value| v["report"]["records"][1]["stacktrace"] = json!("boom"),
                "unsupported_field",
            ),
            (
                &|v: &mut Value| v["report"]["records"] = json!([]),
                "empty_report",
            ),
            (
                &|v: &mut Value| v["workflow"] = json!("a\u{0}b"),
                "nul_character",
            ),
        ] {
            assert_eq!(tampered(edit).unwrap_err(), code);
        }
        let duplicate = String::from_utf8(body.clone())
            .unwrap()
            .replacen("{", r#"{"job":"x","#, 1);
        assert_eq!(
            validate_envelope(duplicate.as_bytes()).unwrap_err(),
            "duplicate_key"
        );
        assert_eq!(validate_envelope(b"[]").unwrap_err(), "invalid_envelope");
        assert_eq!(validate_envelope(b"\xff").unwrap_err(), "invalid_utf8");
        assert_eq!(
            validate_envelope(&vec![b' '; MAX_REQUEST_BYTES + 1]).unwrap_err(),
            "request_too_large"
        );
        let deep = format!("{}{}", "[".repeat(17), "]".repeat(17));
        assert_eq!(
            validate_envelope(deep.as_bytes()).unwrap_err(),
            "nested_too_deeply"
        );
    }

    #[test]
    fn collection_stops_between_reports_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::default();
        for id in ["a", "b", "c"] {
            std::fs::write(dir.path().join(format!("{id}.jsonl")), complete(id)).unwrap();
        }
        let calls = Cell::new(0);
        let stop = || {
            calls.set(calls.get() + 1);
            calls.get() > 1
        };
        let mut sink = MemorySink::default();
        let collection = collect_dir(dir.path(), &ctx(&source, "passed"), &mut sink, &stop);
        assert_eq!(collection.outcome, "interrupted");
        assert_eq!(collection.reports.len(), 1);
        assert_eq!(collection.rejected["collection_interrupted"], 2);
    }

    #[test]
    fn snapshot_write_failures_are_counted_and_duplicates_still_found() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::default();
        std::fs::write(dir.path().join("a.jsonl"), complete("a")).unwrap();
        std::fs::write(dir.path().join("b.jsonl"), complete("a")).unwrap();
        std::fs::write(dir.path().join("c.jsonl"), complete("c")).unwrap();
        let mut sink = MemorySink {
            fail: true,
            ..Default::default()
        };
        let collection = collect_dir(dir.path(), &ctx(&source, "passed"), &mut sink, &|| false);
        assert!(collection.reports.is_empty());
        assert_eq!(collection.rejected["snapshot_write_failed"], 3);
        assert_eq!(collection.rejected["duplicate_report_id"], 2);
    }

    #[test]
    fn oversized_sources_are_rejected_unparsed_and_kept() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::default();
        let path = dir.path().join("big.jsonl");
        std::fs::write(&path, complete("r")).unwrap();
        // Sparse: a file just over the request limit plus allowance is never read.
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_SOURCE_BYTES_PER_REPORT + 1).unwrap();
        let (collection, sink) = collect(dir.path(), &ctx(&source, "passed"));
        assert_eq!(collection.rejected["request_too_large"], 1);
        assert!(sink.bodies.is_empty());
        assert_eq!(
            std::fs::metadata(&path).unwrap().len(),
            MAX_SOURCE_BYTES_PER_REPORT + 1
        );
        assert_eq!(MAX_SOURCE_BYTES_PER_REPORT, 33 * 1024 * 1024);
    }

    #[test]
    fn job_byte_limit_is_checked_before_reading() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("big.jsonl");
        std::fs::write(&path, complete("r")).unwrap();
        assert_eq!(read_regular(&path, 10).unwrap_err(), "job_byte_limit");
        let file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        file.set_len(MAX_SOURCE_BYTES_PER_REPORT + 1).unwrap();
        assert_eq!(
            read_regular(&path, MAX_JOB_SOURCE_BYTES).unwrap_err(),
            "request_too_large"
        );
    }
}
