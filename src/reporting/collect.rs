//! Bounded, structural collection of `test-timings/1` JSONL reports.
//!
//! Only direct regular `*.jsonl` files in a job's own report directory are
//! read. Rejected files are never modified and are reported by structural code
//! only; record contents never appear in diagnostics. Every server vocabulary
//! enforced here comes from [`super::contract`], which collection applies in
//! full before a snapshot is frozen.

use super::contract::{self, *};
use super::Source;
use crate::config::Partition;
use serde::de::{DeserializeSeed, Deserializer, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde_json::{json, Map, Value};
use std::cell::Cell;
use std::collections::{BTreeMap, BinaryHeap, HashMap, HashSet};
use std::fmt;
use std::io::Read;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

pub use contract::{MAX_RECORDS, MAX_REQUEST_BYTES};

pub const MAX_REPORTS_PER_JOB: usize = 256;
/// Bounds the work spent on `*.jsonl` names that are not readable regular files.
pub const MAX_ENTRIES_PER_JOB: usize = 16 * MAX_REPORTS_PER_JOB;
/// Directory entries enumerated between cancellation checks.
const STOP_CHECK_INTERVAL: usize = 256;
pub const MAX_JOB_SOURCE_BYTES: u64 = 256 * 1024 * 1024;
/// Compact formatter output re-serializes to about its own size, so a source
/// file more than this allowance over the request limit cannot fit one request
/// and is rejected without being parsed.
pub const SOURCE_ALLOWANCE_BYTES: u64 = 1024 * 1024;
pub const MAX_SOURCE_BYTES_PER_REPORT: u64 = MAX_REQUEST_BYTES as u64 + SOURCE_ALLOWANCE_BYTES;
/// A record sits at level 4 of the body (body -> report -> records -> record),
/// so its own nesting, counting the record object as level 1, may be at most 13.
const MAX_RECORD_DEPTH: usize = MAX_BODY_DEPTH - 3;

/// A rejected duplicate whose already-frozen snapshot could not be removed.
/// The file is already counted as `duplicate_report_id`; this code only makes
/// the leftover snapshot visible.
pub const SNAPSHOT_DISCARD_FAILED: &str = "snapshot_discard_failed";

/// Rejection codes that annotate a file already counted under another code.
/// Every other code counts exactly one rejected file, so these are excluded
/// when rejections are totalled (see [`rejected_files`]).
pub const SECONDARY_REJECTIONS: &[&str] = &[SNAPSHOT_DISCARD_FAILED];

/// The number of files a collection rejected: one per file, never counting a
/// [`SECONDARY_REJECTIONS`] code.
pub fn rejected_files(rejected: &BTreeMap<String, usize>) -> usize {
    rejected
        .iter()
        .filter(|(code, _)| !SECONDARY_REJECTIONS.contains(&code.as_str()))
        .map(|(_, n)| n)
        .sum()
}

pub struct JobContext<'a> {
    pub workflow_run_id: &'a str,
    pub workflow: &'a str,
    pub environment: Option<&'a str>,
    pub job: &'a str,
    pub job_status: &'a str,
    pub source: &'a Source,
    /// The instance Goodflows scheduled, authoritative over producer values.
    pub partition: Option<Partition>,
}

/// What remains of one job's collection allowance. All instances of a
/// partitioned job draw from one budget, in index order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub reports: usize,
    pub bytes: u64,
    pub entries: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self {
            reports: MAX_REPORTS_PER_JOB,
            bytes: MAX_JOB_SOURCE_BYTES,
            entries: MAX_ENTRIES_PER_JOB,
        }
    }
}

/// Receives each valid request body as soon as it is built, so at most one
/// body is held in memory during collection.
pub trait Sink {
    /// Persist a frozen request body, returning its snapshot index.
    fn freeze(&mut self, body: &[u8]) -> Result<u32, ()>;
    /// Remove a body frozen earlier in this collection (a duplicate identity).
    /// An error means the snapshot may remain on disk.
    fn discard(&mut self, index: u32) -> Result<(), ()>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frozen {
    pub report_id: String,
    pub index: u32,
}

pub struct Collection {
    pub outcome: &'static str,
    pub rejected: BTreeMap<String, usize>,
    /// Frozen reports that survived the job-wide duplicate pass.
    pub reports: Vec<Frozen>,
    /// Every parsed identity (frozen or not), awaiting [`resolve_duplicates`].
    parsed: Vec<(String, Option<u32>)>,
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
            parsed: Vec::new(),
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

/// Collect every valid report from one executed job's (or instance's)
/// directory, freezing each into `sink` as it is built and drawing from the
/// job's remaining `budget`. `stop` is checked before each file; once it returns
/// true the remaining files are left unread (`collection_interrupted`).
/// Duplicate identities are resolved afterwards, across every directory of the
/// job, by [`resolve_duplicates`].
pub fn collect_dir(
    dir: &Path,
    ctx: &JobContext,
    sink: &mut dyn Sink,
    stop: &dyn Fn() -> bool,
    budget: &mut Budget,
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
    // Keep only the lowest remaining-allowance names while enumerating, so a
    // directory flooded with entries bounds memory and sorting work, and the
    // examined set does not depend on directory order.
    let mut kept: BinaryHeap<String> = BinaryHeap::new();
    for (seen, entry) in entries.flatten().enumerate() {
        if seen > 0 && seen % STOP_CHECK_INTERVAL == 0 && stop() {
            collection.outcome = "interrupted";
            collection.reject_n("collection_interrupted", kept.len());
            return collection;
        }
        let name = entry.file_name();
        match name.to_str() {
            Some(name) if name.ends_with(".jsonl") => {
                if kept.len() < budget.entries {
                    kept.push(name.to_owned());
                } else if kept.peek().is_some_and(|max| name < max.as_str()) {
                    kept.pop();
                    kept.push(name.to_owned());
                    collection.reject("entry_limit");
                } else {
                    collection.reject("entry_limit");
                }
            }
            Some(_) => {}
            None => {
                if name.as_encoded_bytes().ends_with(b".jsonl") {
                    collection.reject("invalid_file_name");
                }
            }
        }
    }
    let names = kept.into_sorted_vec();
    budget.entries -= names.len();
    for (index, name) in names.iter().enumerate() {
        if stop() {
            collection.outcome = "interrupted";
            collection.reject_n("collection_interrupted", names.len() - index);
            break;
        }
        let path = dir.join(name);
        // Only regular files that were read count toward the report cap, so
        // symlinks and other junk cannot crowd out valid reports.
        if budget.reports == 0 {
            collection.reject(match std::fs::symlink_metadata(&path) {
                Ok(meta) if meta.file_type().is_symlink() => "symlink",
                Ok(meta) if !meta.is_file() => "not_regular_file",
                Ok(_) => "report_limit",
                Err(_) => "unreadable_file",
            });
            continue;
        }
        let bytes = match read_regular(&path, budget.bytes) {
            Ok(bytes) => bytes,
            Err(code) => {
                collection.reject(code);
                continue;
            }
        };
        budget.bytes -= bytes.len() as u64;
        budget.reports -= 1;
        let built = parse_report(&bytes).and_then(|mut report| {
            assign_partition(ctx.partition, &mut report);
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
                collection.parsed.push((report_id, frozen));
            }
            Err(code) => collection.reject(code),
        }
    }
    collection
}

/// Resolve `report_id` uniqueness across every directory of one job. The
/// server identity is `(project, workflow_run_id, job, report_id)` and excludes
/// suite_id and partition, so a reused report_id cannot be disambiguated: every
/// copy is rejected and any already-frozen snapshot is discarded through its own
/// sink. A snapshot that cannot be removed is never listed or sent, and is
/// counted as [`SNAPSHOT_DISCARD_FAILED`] in addition to `duplicate_report_id`.
/// Each file counts under exactly one primary code: a duplicate that was never
/// frozen is already counted as `snapshot_write_failed`, so it still makes the
/// other copies duplicates but is not counted again. Surviving frozen reports
/// become the collection's `reports`.
pub fn resolve_duplicates<S: Sink>(instances: &mut [(Collection, S)]) {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for (collection, _) in instances.iter() {
        for (report_id, _) in &collection.parsed {
            *counts.entry(report_id.clone()).or_default() += 1;
        }
    }
    for (collection, sink) in instances.iter_mut() {
        for (report_id, frozen) in std::mem::take(&mut collection.parsed) {
            if counts[&report_id] > 1 {
                // Without a snapshot the file is already `snapshot_write_failed`.
                if let Some(index) = frozen {
                    if sink.discard(index).is_err() {
                        collection.reject(SNAPSHOT_DISCARD_FAILED);
                    }
                    collection.reject("duplicate_report_id");
                }
            } else if let Some(index) = frozen {
                collection.reports.push(Frozen { report_id, index });
            }
        }
        if collection.outcome == "collected"
            && collection.reports.is_empty()
            && collection.rejected.is_empty()
        {
            collection.outcome = "no_reports";
        }
    }
}

/// For an instance Goodflows scheduled, its identity replaces the producer's
/// partition fields in both the envelope and the initial `suite_started`
/// record (the server requires the two to agree). Producer values were already
/// grammar-checked by [`parse_report`]; no other record field changes.
fn assign_partition(partition: Option<Partition>, report: &mut Report) {
    let Some(partition) = partition else {
        return;
    };
    report.partition = Value::String(partition.index.to_string());
    report.partition_count = Value::from(partition.count);
    if let Some(Value::Object(first)) = report.records.first_mut() {
        first.insert("partition".into(), report.partition.clone());
        first.insert("partition_count".into(), report.partition_count.clone());
    }
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
/// Each record is fully validated (types, widths, identity, order, pairing)
/// as soon as its line is decoded, so memory spent on a bad report is bounded
/// by its valid prefix plus one line, and parsing stops at the first bad line
/// or as soon as the records alone could not fit one request.
pub fn parse_report(bytes: &[u8]) -> Result<Report, &'static str> {
    let text = std::str::from_utf8(bytes).map_err(|_| "invalid_utf8")?;
    let mut lines = text.split('\n').peekable();
    let mut records = Vec::new();
    let mut validator = Validator::default();
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
                validator.push(&records, &value)?;
                records.push(value);
            }
            Ok(_) => return Err("malformed_record"),
            // A rule violation can surface before the parser reaches the cut, so
            // a final fragment is recognized by its syntax alone.
            Err(_) if last && serde_json::from_str::<IgnoredAny>(line).is_err() => truncated = true,
            Err(code) => return Err(code),
        }
    }
    validator.finish(&records)?;
    let first = &records[0];
    Ok(Report {
        suite_id: first["suite_id"].as_str().unwrap_or_default().to_owned(),
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

    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Value, E> {
        // serde_json only uses i64 for negative integers.
        if v.unsigned_abs() > MAX_SAFE_INTEGER {
            return Err(self.fail("integer_out_of_range"));
        }
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
    pub partition: Option<String>,
    pub partition_count: Option<u64>,
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
    if !exact_keys(&body, ENVELOPE) {
        return Err(invalid);
    }
    if body["format"] != REPORT_FORMAT {
        return Err("unsupported_format");
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
    if !crate::fsutil::canonical_uuid(&workflow_run_id) {
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
    if report["suite_id"] != first["suite_id"]
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
        partition: report["partition"].as_str().map(str::to_owned),
        partition_count: report["partition_count"].as_u64(),
    })
}

fn contains_nul(value: &Value) -> bool {
    match value {
        Value::String(s) => s.contains('\0'),
        Value::Array(items) => items.iter().any(contains_nul),
        Value::Object(map) => map.iter().any(|(k, v)| k.contains('\0') || contains_nul(v)),
        _ => false,
    }
}

/// Full local validation against [`super::contract`] of a whole records
/// array; see [`Validator`].
fn validate_records(records: &[Value]) -> Result<(), &'static str> {
    let mut validator = Validator::default();
    for (index, record) in records.iter().enumerate() {
        validator.push(&records[..index], record)?;
    }
    validator.finish(records)
}

/// Started events of one kind: the open ones by `execution_id` (with the
/// index of their record, whose core values the finished event repeats) and
/// every id seen so far.
type Open = (HashMap<u64, usize>, HashSet<u64>);

/// Full local validation against [`super::contract`], one record at a time,
/// so a record is typed as soon as it is decoded and a report is rejected at
/// its first bad line rather than after the whole file is parsed: every
/// record is typed against its event's table (keys, required keys, widths,
/// `meta`), identities must agree, `suite_started` comes first and
/// `suite_finished` last, every finished event pairs with an open started
/// event of its kind with identical core values, and `suite_finished`'s
/// unfinished counts equal the started events left open. Memory beyond the
/// accepted records is two ids per started event, bounded by [`MAX_RECORDS`].
#[derive(Default)]
struct Validator {
    groups: Open,
    tests: Open,
    /// Set by `suite_finished`: any later record is out of order, otherwise
    /// this is the report's result (the unfinished-count check).
    finished: Option<Result<(), &'static str>>,
}

impl Validator {
    /// Check `record`, which follows `before`, the records already accepted.
    fn push(&mut self, before: &[Value], record: &Value) -> Result<(), &'static str> {
        let Some(first) = before.first() else {
            if record.get("event").and_then(Value::as_str) != Some("suite_started") {
                return Err("missing_suite_started");
            }
            // The format is checked first, so a retired producer gets a format
            // code rather than one for whichever of its keys the table no
            // longer knows.
            if record.get("format").and_then(Value::as_str) != Some(REPORT_FORMAT) {
                return Err("unsupported_format");
            }
            return self.check(before, record, None);
        };
        if self.finished.is_some() {
            return Err("invalid_order");
        }
        let ids = (first["suite_id"].as_str(), first["report_id"].as_str());
        self.check(before, record, Some(ids))
    }

    fn check(
        &mut self,
        before: &[Value],
        record: &Value,
        ids: Option<(Option<&str>, Option<&str>)>,
    ) -> Result<(), &'static str> {
        let Value::Object(map) = record else {
            return Err("malformed_record");
        };
        if contains_nul(record) {
            return Err("nul_character");
        }
        let shape = contract::check_record(map)?;
        if ids.is_some_and(|ids| ids != (map["suite_id"].as_str(), map["report_id"].as_str())) {
            return Err("identity_mismatch");
        }
        match map["event"].as_str() {
            Some("suite_started") if ids.is_some() => return Err("invalid_order"),
            Some("suite_finished") => {
                let unfinished = |key: &str| map[key].as_u64().unwrap_or_default() as usize;
                let matches = unfinished("unfinished_groups") == self.groups.0.len()
                    && unfinished("unfinished_tests") == self.tests.0.len();
                self.finished = Some(if matches {
                    Ok(())
                } else {
                    Err("unfinished_mismatch")
                });
            }
            _ => {}
        }
        let Some((kind, started)) = shape.pairing else {
            return Ok(());
        };
        let (open, seen) = match kind {
            Kind::Group => &mut self.groups,
            Kind::Test => &mut self.tests,
        };
        let id = map["execution_id"].as_u64().unwrap_or_default();
        if started {
            if !seen.insert(id) {
                return Err("unpaired_event");
            }
            open.insert(id, before.len());
            return Ok(());
        }
        let start = &before[open.remove(&id).ok_or("unpaired_event")?];
        if shape.base.iter().any(|(key, _)| start[*key] != map[*key]) {
            return Err("unpaired_event");
        }
        Ok(())
    }

    /// The result once every record has been pushed.
    fn finish(self, records: &[Value]) -> Result<(), &'static str> {
        if records.is_empty() {
            return Err("empty_report");
        }
        self.finished.unwrap_or(Ok(()))
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use serde_json::json;

    pub fn suite_started(report_id: &str, partition: Value) -> Value {
        json!({"event":"suite_started","suite_id":"suite-1","report_id":report_id,"received_offset_us":0,
            "format":"test-timings/1","started_at":"2026-11-01T00:00:00Z",
            "runner":{"name":"exunit","version":"1.18.0"},"partition":partition,"partition_count":3,"seed":1,
            "filters":{"include":[],"exclude":[{"tag":"chrome","value":true}]},
            "meta":{"elixir_version":"1.18.0","otp_release":"27","max_cases":8,"schedulers":8}})
    }

    fn group_started(report_id: &str) -> Value {
        json!({"event":"group_started","suite_id":"suite-1","report_id":report_id,"received_offset_us":2,
            "group":"MyTest","file":"test/my_test.exs","execution_id":1,"started_offset_us":2,
            "meta":{"async":true,"setup_all":false,"parameter_id":null}})
    }

    fn group_finished(report_id: &str) -> Value {
        json!({"event":"group_finished","suite_id":"suite-1","report_id":report_id,"received_offset_us":60,
            "group":"MyTest","file":"test/my_test.exs","execution_id":1,"started_offset_us":2,
            "duration_us":50,"outcome":"passed","counts":{"passed":1}})
    }

    fn test_started(report_id: &str) -> Value {
        json!({"event":"test_started","suite_id":"suite-1","report_id":report_id,"received_offset_us":5,
            "group":"MyTest","path":[],"name":"test works","file":"test/my_test.exs","line":3,
            "execution_id":1,"started_offset_us":5,"meta":{"async":true,"parameter_id":null}})
    }

    fn test_finished(report_id: &str, outcome: &str) -> Value {
        json!({"event":"test_finished","suite_id":"suite-1","report_id":report_id,"received_offset_us":10,
            "group":"MyTest","path":[],"name":"test works","file":"test/my_test.exs","line":3,
            "execution_id":1,"started_offset_us":5,"duration_us":42,"outcome":outcome})
    }

    fn suite_finished(report_id: &str) -> Value {
        json!({"event":"suite_finished","suite_id":"suite-1","report_id":report_id,"received_offset_us":99,
            "aborted":false,"unfinished_tests":0,"unfinished_groups":0,"counts":{"passed":1,"failed":0},
            "duration_us":100,"meta":{"run_us":100,"async_us":60,"sync_us":40,"load_us":null,"total_us":100}})
    }

    fn interrupted(report_id: &str) -> Value {
        json!({"event":"interrupted","suite_id":"suite-1","report_id":report_id,"received_offset_us":1})
    }

    pub fn jsonl(records: &[Value]) -> Vec<u8> {
        records
            .iter()
            .map(|r| format!("{r}\n"))
            .collect::<String>()
            .into_bytes()
    }

    /// A complete report: one group holding one passing test.
    fn complete_records(report_id: &str) -> Vec<Value> {
        vec![
            suite_started(report_id, json!("1")),
            group_started(report_id),
            test_started(report_id),
            test_finished(report_id, "passed"),
            group_finished(report_id),
            suite_finished(report_id),
        ]
    }

    pub fn complete(report_id: &str) -> Vec<u8> {
        jsonl(&complete_records(report_id))
    }

    /// Keeps frozen bodies in memory, as the spool writer keeps them on disk.
    #[derive(Default)]
    pub struct MemorySink {
        pub bodies: BTreeMap<u32, Vec<u8>>,
        next: u32,
        pub fail: bool,
        /// Discards fail and keep the body, like an unremovable snapshot.
        pub fail_discard: bool,
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

        fn discard(&mut self, index: u32) -> Result<(), ()> {
            if self.fail_discard {
                return Err(());
            }
            self.bodies.remove(&index);
            Ok(())
        }
    }

    /// Collect one directory as a whole job: its own budget and duplicate pass.
    fn collect_with(
        dir: &Path,
        ctx: &JobContext,
        sink: MemorySink,
        stop: &dyn Fn() -> bool,
    ) -> (Collection, MemorySink) {
        let mut sink = sink;
        let collection = collect_dir(dir, ctx, &mut sink, stop, &mut Budget::default());
        let mut instances = [(collection, sink)];
        resolve_duplicates(&mut instances);
        let [(collection, sink)] = instances;
        (collection, sink)
    }

    fn collect(dir: &Path, ctx: &JobContext) -> (Collection, MemorySink) {
        collect_with(dir, ctx, MemorySink::default(), &|| false)
    }

    fn ctx<'a>(source: &'a Source, status: &'a str) -> JobContext<'a> {
        JobContext {
            workflow_run_id: "67f7946d-136b-4608-ab01-e1f251a09754",
            workflow: "default",
            environment: None,
            job: "test",
            job_status: status,
            source,
            partition: None,
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
        assert_eq!(body["format"], "test-timings/1");
        assert_eq!(body["report"]["truncated"], false);
        assert_eq!(body["report"]["records"].as_array().unwrap().len(), 6);
        assert_eq!(body["report"]["records"][3]["duration_us"], 42);
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
            test_started("r"),
            test_finished("r", "failed"),
            suite_finished("r"),
        ]);
        assert_eq!(
            parse_report(&failed).unwrap().records[2]["outcome"],
            "failed"
        );
    }

    #[test]
    fn partial_reports_stay_partial() {
        let partial = jsonl(&[
            suite_started("r", json!("2")),
            test_started("r"),
            test_finished("r", "passed"),
        ]);
        let report = parse_report(&partial).unwrap();
        assert!(!report.truncated);
        assert_eq!(report.records.len(), 3);
        assert_ne!(report.records.last().unwrap()["event"], "suite_finished");
    }

    #[test]
    fn final_line_handling() {
        let mut unterminated = complete("r");
        unterminated.pop();
        let report = parse_report(&unterminated).unwrap();
        assert!(!report.truncated);
        assert_eq!(report.records.len(), 6);

        let mut fragment = jsonl(&[
            suite_started("r", Value::Null),
            test_started("r"),
            test_finished("r", "passed"),
        ]);
        fragment.extend_from_slice(br#"{"event":"test_fini"#);
        let report = parse_report(&fragment).unwrap();
        assert!(report.truncated);
        assert_eq!(report.records.len(), 3);

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
        other_suite["suite_id"] = json!("suite-2");
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
        // The retired ExUnit aggregates are unknown keys now.
        for (key, value) in [("files", json!([])), ("times_us", json!({}))] {
            let mut retired = suite_finished("r");
            retired[key] = value;
            assert_eq!(
                parse_report(&jsonl(&[suite_started("r", Value::Null), retired])).unwrap_err(),
                "unsupported_field"
            );
        }
        let mut excluded = suite_finished("r");
        excluded["counts"]["excluded"] = json!(1);
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), excluded])).unwrap_err(),
            "unsupported_field"
        );
        let log =
            json!({"event":"log","suite_id":"suite-1","report_id":"r","received_offset_us":1});
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
        long["group"] = json!("é".repeat(2049)); // 4098 bytes, 2049 chars
        assert_eq!(
            parse_report(&jsonl(&[suite_started("r", Value::Null), long])).unwrap_err(),
            "field_too_long"
        );
        let (mut start, mut fits) = (test_started("r"), test_finished("r", "passed"));
        start["group"] = json!("m".repeat(4096));
        fits["group"] = json!("m".repeat(4096));
        assert!(parse_report(&jsonl(&[suite_started("r", Value::Null), start, fits])).is_ok());
        let mut version = suite_started("r", Value::Null);
        version["runner"]["version"] = json!("1".repeat(129));
        assert_eq!(
            parse_report(&jsonl(&[version])).unwrap_err(),
            "field_too_long"
        );
        assert_eq!(parse_report(b"\xff\n").unwrap_err(), "invalid_utf8");
    }

    #[test]
    fn record_and_request_limits() {
        let started = suite_started("r", Value::Null);
        let tiny = interrupted("r");
        let mut records = vec![started.clone()];
        records.extend(std::iter::repeat_n(tiny.clone(), MAX_RECORDS - 1));
        assert!(parse_report(&jsonl(&records)).is_ok());
        records.push(tiny);
        assert_eq!(parse_report(&jsonl(&records)).unwrap_err(), "record_limit");

        let source = Source::default();
        // A valid filler record of about 4 KiB: flat meta at its string limit.
        let mut big = interrupted("r");
        big["meta"] = Value::Object(
            (0..15)
                .map(|i| (format!("pad{i}"), json!("d".repeat(256))))
                .collect(),
        );
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
        let tiny = interrupted("r");
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
        let mut bytes = jsonl(&[suite_started("r", Value::Null), test_started("r")]);
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
        // The record is level 1, so 12 nested arrays reach record depth 13:
        // that still decodes (and is then refused by the field's type), which
        // places a record's deepest level at body level 16.
        let nested = |levels: usize| format!("{}{}", "[".repeat(levels), "]".repeat(levels));
        let record = finished_with("file", &nested(12));
        assert!(decode(&record, MAX_RECORD_DEPTH).is_ok());
        assert_eq!(MAX_RECORD_DEPTH + 3, MAX_BODY_DEPTH);
        assert_eq!(parse_report(&line(&record)).unwrap_err(), "invalid_field");
        assert_eq!(
            parse_report(&line(&finished_with("file", &nested(13)))).unwrap_err(),
            "nested_too_deeply"
        );
        // Deeper than serde_json's own recursion limit still gets the structural code.
        assert_eq!(
            parse_report(&line(&finished_with("file", &nested(500)))).unwrap_err(),
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
        assert_eq!(max.records[2]["duration_us"], MAX_SAFE_INTEGER);
        let mut seed = suite_started("r", Value::Null);
        seed["seed"] = json!(-5);
        assert!(parse_report(&jsonl(&[seed])).is_ok());
        let seed = suite_started("r", Value::Null).to_string().replacen(
            r#""seed":1"#,
            r#""seed":-9007199254740992"#,
            1,
        );
        assert_eq!(
            parse_report(format!("{seed}\n").as_bytes()).unwrap_err(),
            "integer_out_of_range"
        );
        let seed = suite_started("r", Value::Null).to_string().replacen(
            r#""seed":1"#,
            r#""seed":-9007199254740991"#,
            1,
        );
        assert!(parse_report(format!("{seed}\n").as_bytes()).is_ok());
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
    fn filters_use_the_server_tag_grammar() {
        let with_filters = |list: &str, entries: Value| {
            let mut started = suite_started("r", Value::Null);
            started["filters"][list] = entries;
            jsonl(&[started])
        };
        for tag in [
            "chrome",
            "goodaudit_integration",
            "athena_preview",
            "chromic_teardown",
            "qpdf",
        ] {
            let raw = with_filters("include", json!([{"tag": tag, "value": false}]));
            assert!(parse_report(&raw).is_ok(), "{tag}");
        }
        // Any boolean identifier tag is accepted, in either list.
        let mut started = suite_started("r", Value::Null);
        started["filters"]["include"] = json!([{"tag": "slow", "value": true}]);
        started["filters"]["exclude"] = json!([{"tag": "chrome", "value": true}]);
        let report = parse_report(&jsonl(&[started])).unwrap();
        assert_eq!(
            report.records[0]["filters"]["include"],
            json!([{"tag": "slow", "value": true}])
        );
        assert_eq!(
            report.records[0]["filters"]["exclude"],
            json!([{"tag": "chrome", "value": true}])
        );
        // Tags are case-sensitive and frozen without normalization.
        let cased = json!([{"tag": "Chrome", "value": true}, {"tag": "chrome", "value": true}]);
        let report = parse_report(&with_filters("include", cased.clone())).unwrap();
        assert_eq!(report.records[0]["filters"]["include"], cased);
        // A repeated tag keeps its supplied order in the frozen envelope.
        let repeated = json!([{"tag": "slow", "value": true}, {"tag": "slow", "value": false}]);
        let report = parse_report(&with_filters("exclude", repeated.clone())).unwrap();
        let source = Source::default();
        let body: Value =
            serde_json::from_slice(&envelope(&ctx(&source, "passed"), &report).unwrap()).unwrap();
        assert_eq!(body["report"]["records"][0]["filters"]["exclude"], repeated);
        // Each list holds at most MAX_FILTER_ENTRIES entries.
        let entries = |n: usize| Value::Array(vec![json!({"tag": "slow", "value": true}); n]);
        for list in ["include", "exclude"] {
            assert!(
                parse_report(&with_filters(list, entries(256))).is_ok(),
                "{list}"
            );
            assert_eq!(
                parse_report(&with_filters(list, entries(257))).unwrap_err(),
                "unsupported_filter",
                "{list}"
            );
        }
        for entry in [
            json!({"tag":"slow test","value":true}),
            json!({"tag":"ok?","value":true}),
            json!({"tag":"slow\n","value":true}),
            json!({"tag":"","value":true}),
            json!({"tag":"a".repeat(129),"value":true}),
            json!({"tag":"chrome","value":"yes"}),
            json!({"tag":"chrome","value":null}),
            json!({"tag":1,"value":true}),
            json!({"value":true}),
            json!({"tag":"chrome"}),
        ] {
            for list in ["include", "exclude"] {
                assert_eq!(
                    parse_report(&with_filters(list, json!([entry]))).unwrap_err(),
                    "unsupported_filter",
                    "{entry}"
                );
            }
        }
        // Grammar boundaries on the filter path: 128 bytes, a leading digit, `.`,
        // `_` and `-` pass; a leading punctuation byte or any non-ASCII byte fails.
        for tag in ["a".repeat(128), "1x.a-b".into(), "a_b".into()] {
            let raw = with_filters("include", json!([{"tag": tag, "value": true}]));
            assert!(parse_report(&raw).is_ok(), "{tag}");
        }
        for tag in [".slow", "_slow", "-slow", "caf\u{e9}", "\u{ff53}low"] {
            for list in ["include", "exclude"] {
                assert_eq!(
                    parse_report(&with_filters(list, json!([{"tag": tag, "value": true}])))
                        .unwrap_err(),
                    "unsupported_filter",
                    "{tag:?} in {list}"
                );
            }
        }
        // A non-object entry has no tag to read.
        for entry in [json!("slow"), Value::Null, json!(["slow", true])] {
            for list in ["include", "exclude"] {
                assert_eq!(
                    parse_report(&with_filters(list, json!([entry]))).unwrap_err(),
                    "unsupported_filter",
                    "{entry} in {list}"
                );
            }
        }
        // Collection stops at the first failure and reports one code; the server
        // collects every error, so only accept/reject is shared. Pin the order:
        // an unknown key in an entry or in `filters` is `unsupported_field`.
        for list in ["include", "exclude"] {
            let raw = with_filters(list, json!([{"tag": "slow", "value": true, "note": "x"}]));
            assert_eq!(
                parse_report(&raw).unwrap_err(),
                "unsupported_field",
                "{list}"
            );
        }
        let mut started = suite_started("r", Value::Null);
        started["filters"] = json!({"include": [], "exclude": [], "x": 1});
        assert_eq!(
            parse_report(&jsonl(&[started])).unwrap_err(),
            "unsupported_field"
        );
        // `filters` must be an object whose `include` and `exclude` are both arrays.
        for list in ["include", "exclude"] {
            for value in [
                None,
                Some(Value::Null),
                Some(json!({})),
                Some(json!("slow")),
            ] {
                let mut started = suite_started("r", Value::Null);
                match value {
                    None => started["filters"].as_object_mut().unwrap().remove(list),
                    Some(value) => started["filters"]
                        .as_object_mut()
                        .unwrap()
                        .insert(list.into(), value),
                };
                assert_eq!(
                    parse_report(&jsonl(&[started])).unwrap_err(),
                    "unsupported_filter",
                    "{list}"
                );
            }
        }
        for filters in [json!([]), json!("x")] {
            let mut started = suite_started("r", Value::Null);
            started["filters"] = filters;
            assert_eq!(
                parse_report(&jsonl(&[started])).unwrap_err(),
                "unsupported_filter"
            );
        }
        // A runner without tags sends null; the key itself is a required core key.
        let mut started = suite_started("r", Value::Null);
        started["filters"] = Value::Null;
        assert!(parse_report(&jsonl(&[started])).is_ok());
        let mut started = suite_started("r", Value::Null);
        started.as_object_mut().unwrap().remove("filters");
        assert_eq!(
            parse_report(&jsonl(&[started])).unwrap_err(),
            "missing_field"
        );
    }

    fn rejects(records: &[Value]) -> &'static str {
        parse_report(&jsonl(records)).unwrap_err()
    }

    #[test]
    fn records_are_typed_against_the_contract() {
        let ss = || suite_started("r", Value::Null);
        // Every core key is required, even a nullable one.
        let mut missing = test_finished("r", "passed");
        missing.as_object_mut().unwrap().remove("duration_us");
        assert_eq!(
            rejects(&[ss(), test_started("r"), missing]),
            "missing_field"
        );
        let mut missing = ss();
        missing.as_object_mut().unwrap().remove("seed");
        assert_eq!(rejects(&[missing]), "missing_field");
        // Wrong-typed core values are caught before a snapshot is frozen.
        for (key, value) in [
            ("duration_us", json!("12")),
            ("duration_us", json!(-5)),
            ("outcome", json!("todo")),
            ("outcome", json!("excluded")),
            ("line", json!(0)),
            ("path", Value::Array(vec![json!("d"); 33])),
            ("execution_id", json!(0)),
        ] {
            let mut finished = test_finished("r", "passed");
            finished[key] = value;
            assert_eq!(
                rejects(&[ss(), test_started("r"), finished]),
                "invalid_field",
                "{key}"
            );
        }
        let mut long = group_started("r");
        long["group"] = json!("g".repeat(4097));
        assert_eq!(rejects(&[ss(), long]), "field_too_long");
        // The retired record keys are unknown keys.
        let mut retired = ss();
        let id = retired.as_object_mut().unwrap().remove("suite_id").unwrap();
        retired["run_id"] = id;
        assert_eq!(rejects(&[retired]), "unsupported_field");
        let mut reason = interrupted("r");
        reason["reason"] = json!("sigint");
        assert_eq!(rejects(&[ss(), reason]), "unsupported_field");
        // Runner identity and a negative seed.
        let mut jest = ss();
        jest["runner"] = json!({"name": "jest", "version": "30.0.0"});
        jest["seed"] = json!(-1843);
        jest["filters"] = Value::Null;
        assert!(parse_report(&jsonl(&[jest])).is_ok());
        let mut spaced = ss();
        spaced["runner"]["name"] = json!("jest runner");
        assert_eq!(rejects(&[spaced]), "invalid_field");
        let mut later = ss();
        later["format"] = json!("test-timings/2");
        assert_eq!(rejects(&[later]), "unsupported_format");
        // Nested describes, and a group that failed to load with empty counts.
        let (mut start, mut finish) = (test_started("r"), test_finished("r", "passed"));
        start["path"] = json!(["math", "sum"]);
        finish["path"] = json!(["math", "sum"]);
        let mut errored = group_finished("r");
        errored["outcome"] = json!("errored");
        errored["counts"] = json!({});
        let report =
            parse_report(&jsonl(&[ss(), group_started("r"), start, finish, errored])).unwrap();
        assert_eq!(report.records[3]["path"], json!(["math", "sum"]));
        // Millisecond producers send their values unchanged.
        let mut ms = test_finished("r", "passed");
        ms["duration_us"] = json!(12000);
        let report = parse_report(&jsonl(&[ss(), test_started("r"), ms])).unwrap();
        assert_eq!(report.records[2]["duration_us"], 12000);
        // A report that ends with an interruption is accepted (as incomplete).
        assert!(parse_report(&jsonl(&[ss(), test_started("r"), interrupted("r")])).is_ok());
        // A runner that stopped at its failure limit says so, then finishes.
        let mut stopped = interrupted("r");
        stopped["event"] = json!("max_failures_reached");
        stopped["meta"] = json!({"max_failures": 1});
        let mut finished = suite_finished("r");
        finished["aborted"] = json!(true);
        let report = parse_report(&jsonl(&[ss(), stopped, finished])).unwrap();
        assert_eq!(report.records[1]["event"], "max_failures_reached");
        let mut reason = interrupted("r");
        reason["event"] = json!("max_failures_reached");
        reason["failures"] = json!(1);
        assert_eq!(rejects(&[ss(), reason]), "unsupported_field");
        // A group whose tests were all skipped.
        let mut skipped = group_finished("r");
        skipped["outcome"] = json!("skipped");
        skipped["counts"] = json!({"skipped": 2});
        let report = parse_report(&jsonl(&[ss(), group_started("r"), skipped])).unwrap();
        assert_eq!(report.records[2]["counts"], json!({"skipped": 2}));
    }

    #[test]
    fn each_record_is_validated_as_its_line_is_decoded() {
        let ss = || suite_started("r", Value::Null);
        let mut long = test_started("r");
        long["name"] = json!("n".repeat(MAX_TEXT_BYTES + 1));
        // The over-wide record is rejected before the next line is decoded:
        // the later duplicate key (a decode error) is never reached.
        let mut bytes = jsonl(&[ss(), long]);
        bytes.extend_from_slice(b"{\"event\":\"interrupted\",\"event\":\"interrupted\"}\n");
        assert_eq!(parse_report(&bytes).unwrap_err(), "field_too_long");
        // Nor does a bad record wait for the request size limit: a later line
        // that would overflow it is never parsed.
        let mut bytes = jsonl(&[ss(), interrupted("other")]);
        let mut huge = interrupted("r");
        huge["meta"] = json!({"blob": "x".repeat(MAX_REQUEST_BYTES)});
        bytes.extend(jsonl(&[huge]));
        assert_eq!(parse_report(&bytes).unwrap_err(), "identity_mismatch");
        // A record after `suite_finished` is out of order even when the
        // unfinished counts are also wrong; a final one is checked for them.
        let mut finished = suite_finished("r");
        finished["unfinished_tests"] = json!(1);
        assert_eq!(
            rejects(&[ss(), finished.clone(), interrupted("r")]),
            "invalid_order"
        );
        assert_eq!(rejects(&[ss(), finished]), "unfinished_mismatch");
        assert_eq!(
            rejects(&[ss(), suite_finished("r"), suite_finished("r")]),
            "invalid_order"
        );
    }

    #[test]
    fn meta_is_flat_and_bounded() {
        let ss = || suite_started("r", Value::Null);
        for meta in [
            json!({"tags": ["a", "b"]}),
            json!({"x": {"x": 1}}),
            json!({"s": "s".repeat(257)}),
            json!({"bad key": 1}),
            json!("flat"),
            Value::Object((0..33).map(|i| (format!("k{i}"), json!(i))).collect()),
        ] {
            let mut started = test_started("r");
            started["meta"] = meta.clone();
            assert_eq!(rejects(&[ss(), started]), "invalid_meta", "{meta}");
        }
        let mut started = ss();
        started["meta"] = json!({"elixir_version": "1.20.0", "otp_release": "28", "max_cases": 16});
        assert!(parse_report(&jsonl(&[started])).is_ok());
        // `meta` is not compared during pairing.
        let mut finished = test_finished("r", "skipped");
        finished["meta"] = json!({"skip_reason": "excluded"});
        assert!(parse_report(&jsonl(&[ss(), test_started("r"), finished])).is_ok());
        let mut interrupted = interrupted("r");
        interrupted["meta"] = json!({"signal": "SIGINT"});
        assert!(parse_report(&jsonl(&[ss(), interrupted])).is_ok());
    }

    #[test]
    fn finished_events_pair_with_started_events() {
        let ss = || suite_started("r", Value::Null);
        // Finish without start, for tests and groups.
        assert_eq!(
            rejects(&[ss(), test_finished("r", "passed")]),
            "unpaired_event"
        );
        assert_eq!(rejects(&[ss(), group_finished("r")]), "unpaired_event");
        // A test's start does not pair a group's finish.
        assert_eq!(
            rejects(&[ss(), test_started("r"), group_finished("r")]),
            "unpaired_event"
        );
        // Finish changes identity.
        for (key, value) in [
            ("name", json!("other")),
            ("path", json!(["x"])),
            ("line", Value::Null),
            ("started_offset_us", json!(6)),
        ] {
            let mut finished = test_finished("r", "passed");
            finished[key] = value;
            assert_eq!(
                rejects(&[ss(), test_started("r"), finished]),
                "unpaired_event",
                "{key}"
            );
        }
        // Finished twice, started twice (even after finishing).
        let pair = [test_started("r"), test_finished("r", "passed")];
        assert_eq!(
            rejects(&[ss(), pair[0].clone(), pair[1].clone(), pair[1].clone()]),
            "unpaired_event"
        );
        assert_eq!(
            rejects(&[ss(), test_started("r"), test_started("r")]),
            "unpaired_event"
        );
        assert_eq!(
            rejects(&[
                ss(),
                pair[0].clone(),
                pair[1].clone(),
                pair[0].clone(),
                pair[1].clone()
            ]),
            "unpaired_event"
        );
        assert_eq!(
            rejects(&[ss(), group_started("r"), group_started("r")]),
            "unpaired_event"
        );
        // Group and test execution ids are separate namespaces.
        assert!(parse_report(&complete("r")).is_ok());
        // Started but never finished: accepted, and counted as unfinished.
        assert!(parse_report(&jsonl(&[ss(), group_started("r"), test_started("r")])).is_ok());
        let mut finished = suite_finished("r");
        finished["unfinished_tests"] = json!(1);
        finished["unfinished_groups"] = json!(1);
        finished["aborted"] = json!(true);
        assert!(parse_report(&jsonl(&[
            ss(),
            group_started("r"),
            test_started("r"),
            finished
        ]))
        .is_ok());
    }

    #[test]
    fn unfinished_counts_must_match_the_open_events() {
        let ss = || suite_started("r", Value::Null);
        assert_eq!(
            rejects(&[ss(), test_started("r"), suite_finished("r")]),
            "unfinished_mismatch"
        );
        assert_eq!(
            rejects(&[ss(), group_started("r"), suite_finished("r")]),
            "unfinished_mismatch"
        );
        let mut over = suite_finished("r");
        over["unfinished_tests"] = json!(1);
        assert_eq!(rejects(&[ss(), over]), "unfinished_mismatch");
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
    fn entry_limit_keeps_the_lowest_names_regardless_of_directory_order() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::default();
        for i in 0..MAX_ENTRIES_PER_JOB {
            std::os::unix::fs::symlink("/nonexistent", dir.path().join(format!("b{i:05}.jsonl")))
                .unwrap();
        }
        // Created last, so it is not among the first entries enumerated.
        std::fs::write(dir.path().join("a.jsonl"), complete("a")).unwrap();
        let (collection, _) = collect(dir.path(), &ctx(&source, "passed"));
        assert_eq!(collection.reports.len(), 1);
        assert_eq!(collection.rejected["symlink"], MAX_ENTRIES_PER_JOB - 1);
        assert_eq!(collection.rejected["entry_limit"], 1);
    }

    #[test]
    fn enumeration_stops_when_asked() {
        let dir = tempfile::tempdir().unwrap();
        let source = Source::default();
        for i in 0..STOP_CHECK_INTERVAL + 1 {
            std::fs::write(dir.path().join(format!("{i:05}.jsonl")), complete("x")).unwrap();
        }
        let (collection, sink) = collect_with(
            dir.path(),
            &ctx(&source, "passed"),
            MemorySink::default(),
            &|| true,
        );
        assert_eq!(collection.outcome, "interrupted");
        assert!(collection.reports.is_empty());
        assert_eq!(
            collection.rejected["collection_interrupted"],
            STOP_CHECK_INTERVAL
        );
        assert!(sink.bodies.is_empty());
    }

    #[test]
    fn truncated_reports_say_so_in_the_request_body() {
        let source = Source::default();
        let mut fragment = jsonl(&[
            suite_started("r", Value::Null),
            test_started("r"),
            test_finished("r", "passed"),
        ]);
        fragment.extend_from_slice(br#"{"event":"test_fin"#);
        let report = parse_report(&fragment).unwrap();
        let body = envelope(&ctx(&source, "failed"), &report).unwrap();
        let value: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(value["report"]["truncated"], true);
        assert_eq!(value["report"]["records"].as_array().unwrap().len(), 3);
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
                partition: Some("1".into()),
                partition_count: Some(3),
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
                &|v: &mut Value| v["report"]["partition_count"] = json!(4),
                "identity_mismatch",
            ),
            (
                &|v: &mut Value| v["report"]["partition_count"] = Value::Null,
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
                // A snapshot frozen before both filter lists were required is
                // refused on replay (`invalid_snapshot`), never sent.
                &|v: &mut Value| {
                    v["report"]["records"][0]["filters"]
                        .as_object_mut()
                        .unwrap()
                        .remove("exclude");
                },
                "unsupported_filter",
            ),
            (
                &|v: &mut Value| v["workflow"] = json!("a\u{0}b"),
                "nul_character",
            ),
            (
                &|v: &mut Value| v["report"]["records"][0]["format"] = json!("test-timings/2"),
                "unsupported_format",
            ),
            (
                &|v: &mut Value| v["report"]["suite_id"] = json!("suite-2"),
                "identity_mismatch",
            ),
            (
                &|v: &mut Value| v["report"]["records"][3]["duration_us"] = json!("12"),
                "invalid_field",
            ),
        ] {
            assert_eq!(tampered(edit).unwrap_err(), code);
        }
        // Snapshots are re-validated against the current grammar, so a tag the
        // old allowlist refused now replays (and an older CLI would refuse it).
        assert!(tampered(&|v: &mut Value| {
            v["report"]["records"][0]["filters"]["include"] =
                json!([{"tag": "slow", "value": true}])
        })
        .is_ok());
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
        let (collection, _) = collect_with(
            dir.path(),
            &ctx(&source, "passed"),
            MemorySink::default(),
            &stop,
        );
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
        let sink = MemorySink {
            fail: true,
            ..Default::default()
        };
        let (collection, _) = collect_with(dir.path(), &ctx(&source, "passed"), sink, &|| false);
        assert!(collection.reports.is_empty());
        // One code per file: the unfrozen duplicates are not counted twice.
        assert_eq!(
            collection.rejected,
            BTreeMap::from([("snapshot_write_failed".into(), 3)])
        );
        assert_eq!(rejected_files(&collection.rejected), 3);
    }

    #[test]
    fn a_duplicate_that_failed_to_freeze_still_rejects_its_frozen_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let source = Source::default();
        let mut instances = Vec::new();
        let mut budget = Budget::default();
        for (index, fail) in [(1, true), (2, false)] {
            let dir = tmp.path().join(format!("p{index}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("dup.jsonl"), complete("dup")).unwrap();
            let mut sink = MemorySink {
                fail,
                ..Default::default()
            };
            let ctx = instance_ctx(&source, index, 2);
            let collection = collect_dir(&dir, &ctx, &mut sink, &|| false, &mut budget);
            instances.push((collection, sink));
        }
        resolve_duplicates(&mut instances);
        assert_eq!(
            instances[0].0.rejected,
            BTreeMap::from([("snapshot_write_failed".into(), 1)])
        );
        assert_eq!(
            instances[1].0.rejected,
            BTreeMap::from([("duplicate_report_id".into(), 1)])
        );
        assert!(instances
            .iter()
            .all(|(c, s)| c.reports.is_empty() && s.bodies.is_empty()));
    }

    #[test]
    fn rejected_files_skip_secondary_codes() {
        let rejected = BTreeMap::from([
            ("duplicate_report_id".into(), 2),
            (SNAPSHOT_DISCARD_FAILED.into(), 1),
            ("snapshot_write_failed".into(), 1),
        ]);
        assert_eq!(rejected_files(&rejected), 3);
        assert_eq!(rejected_files(&BTreeMap::new()), 0);
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

    fn instance_ctx<'a>(source: &'a Source, index: u32, count: u32) -> JobContext<'a> {
        JobContext {
            partition: Some(Partition { index, count }),
            ..ctx(source, "passed")
        }
    }

    /// Collect one report file as instance `index` of `count` and return its frozen body.
    fn collect_instance(report: &[u8], index: u32, count: u32) -> (Collection, MemorySink) {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("r.jsonl"), report).unwrap();
        let source = Source::default();
        collect(dir.path(), &instance_ctx(&source, index, count))
    }

    fn frozen_partition(sink: &MemorySink) -> (Value, Value, Value, Value) {
        let body: Value = serde_json::from_slice(sink.bodies.values().next().unwrap()).unwrap();
        let report = &body["report"];
        (
            report["partition"].clone(),
            report["partition_count"].clone(),
            report["records"][0]["partition"].clone(),
            report["records"][0]["partition_count"].clone(),
        )
    }

    #[test]
    fn instance_identity_replaces_null_producer_partition() {
        let mut records = complete_records("r");
        records[0]["partition"] = Value::Null;
        records[0]["partition_count"] = Value::Null;
        let report = jsonl(&records);
        let (collection, sink) = collect_instance(&report, 2, 3);
        assert_eq!(collection.reports.len(), 1);
        assert_eq!(
            frozen_partition(&sink),
            (json!("2"), json!(3), json!("2"), json!(3))
        );
        let head = validate_envelope(sink.bodies.values().next().unwrap()).unwrap();
        assert_eq!(head.partition.as_deref(), Some("2"));
        assert_eq!(head.partition_count, Some(3));
    }

    #[test]
    fn instance_identity_overrides_a_disagreeing_producer() {
        let mut records = complete_records("r");
        records[0]["partition_count"] = json!(4);
        let report = jsonl(&records);
        let (collection, sink) = collect_instance(&report, 2, 3);
        assert_eq!(collection.reports.len(), 1);
        assert_eq!(
            frozen_partition(&sink),
            (json!("2"), json!(3), json!("2"), json!(3))
        );
        // The frozen body agrees with itself, as the server requires.
        let body = sink.bodies.values().next().unwrap();
        assert!(validate_envelope(body).is_ok());
        // No other record field changes.
        let value: Value = serde_json::from_slice(body).unwrap();
        let mut first = value["report"]["records"][0].clone();
        let mut original = suite_started("r", json!("1"));
        for record in [&mut first, &mut original] {
            record.as_object_mut().unwrap().remove("partition");
            record.as_object_mut().unwrap().remove("partition_count");
        }
        assert_eq!(first, original);
        assert_eq!(value["report"]["records"][3], test_finished("r", "passed"));
    }

    #[test]
    fn malformed_producer_partition_still_rejects_an_instance_report() {
        let report = jsonl(&[suite_started("r", json!("a b")), suite_finished("r")]);
        let (collection, sink) = collect_instance(&report, 1, 2);
        assert!(collection.reports.is_empty());
        assert_eq!(collection.rejected["invalid_partition"], 1);
        assert!(sink.bodies.is_empty());
    }

    #[test]
    fn unpartitioned_jobs_keep_producer_partition_values() {
        let dir = tempfile::tempdir().unwrap();
        let mut started = suite_started("r", json!("7"));
        started["partition_count"] = json!(9);
        std::fs::write(
            dir.path().join("r.jsonl"),
            jsonl(&[started, suite_finished("r")]),
        )
        .unwrap();
        let source = Source::default();
        let (_, sink) = collect(dir.path(), &ctx(&source, "passed"));
        assert_eq!(
            frozen_partition(&sink),
            (json!("7"), json!(9), json!("7"), json!(9))
        );
    }

    #[test]
    fn duplicates_across_instances_reject_every_copy() {
        let tmp = tempfile::tempdir().unwrap();
        let source = Source::default();
        let mut instances = Vec::new();
        let mut budget = Budget::default();
        for (index, ids) in [(1, ["dup", "a"]), (2, ["b", "c"]), (3, ["dup", "d"])] {
            let dir = tmp.path().join(format!("p{index}"));
            std::fs::create_dir(&dir).unwrap();
            for id in ids {
                std::fs::write(dir.join(format!("{id}.jsonl")), complete(id)).unwrap();
            }
            let mut sink = MemorySink::default();
            let ctx = instance_ctx(&source, index, 3);
            let collection = collect_dir(&dir, &ctx, &mut sink, &|| false, &mut budget);
            instances.push((collection, sink));
        }
        resolve_duplicates(&mut instances);
        let ids = |i: usize| -> Vec<&str> {
            instances[i]
                .0
                .reports
                .iter()
                .map(|r| r.report_id.as_str())
                .collect()
        };
        assert_eq!(ids(0), ["a"]);
        assert_eq!(ids(1), ["b", "c"]);
        assert_eq!(ids(2), ["d"]);
        for i in [0, 2] {
            assert_eq!(instances[i].0.rejected["duplicate_report_id"], 1);
            // The frozen duplicate was discarded through its own instance's sink.
            assert_eq!(instances[i].1.bodies.len(), 1);
        }
        assert!(instances[1].0.rejected.is_empty());
        assert_eq!(
            std::fs::read(tmp.path().join("p1/dup.jsonl")).unwrap(),
            complete("dup")
        );
    }

    #[test]
    fn a_failed_duplicate_discard_is_counted_and_never_listed() {
        let tmp = tempfile::tempdir().unwrap();
        let source = Source::default();
        let mut instances = Vec::new();
        let mut budget = Budget::default();
        for (index, fail_discard) in [(1, true), (2, false)] {
            let dir = tmp.path().join(format!("p{index}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("dup.jsonl"), complete("dup")).unwrap();
            let mut sink = MemorySink {
                fail_discard,
                ..Default::default()
            };
            let ctx = instance_ctx(&source, index, 2);
            let collection = collect_dir(&dir, &ctx, &mut sink, &|| false, &mut budget);
            instances.push((collection, sink));
        }
        resolve_duplicates(&mut instances);
        let (failed, kept) = (&instances[0], &instances[1]);
        assert!(failed.0.reports.is_empty());
        assert_eq!(
            failed.0.rejected,
            BTreeMap::from([
                ("duplicate_report_id".into(), 1),
                (SNAPSHOT_DISCARD_FAILED.into(), 1)
            ])
        );
        assert_eq!(rejected_files(&failed.0.rejected), 1);
        // The snapshot stays behind, but it is not a collected report.
        assert_eq!(failed.1.bodies.len(), 1);
        assert!(kept.0.reports.is_empty());
        assert_eq!(
            kept.0.rejected,
            BTreeMap::from([("duplicate_report_id".into(), 1)])
        );
        assert!(kept.1.bodies.is_empty());
    }

    #[test]
    fn instances_of_one_job_share_the_report_and_entry_caps() {
        let tmp = tempfile::tempdir().unwrap();
        let source = Source::default();
        let mut budget = Budget::default();
        let mut instances = Vec::new();
        for index in 1..=2u32 {
            let dir = tmp.path().join(format!("p{index}"));
            std::fs::create_dir(&dir).unwrap();
            for i in 0..200 {
                std::fs::write(
                    dir.join(format!("{i:04}.jsonl")),
                    complete(&format!("p{index}-r{i}")),
                )
                .unwrap();
            }
            let mut sink = MemorySink::default();
            let ctx = instance_ctx(&source, index, 2);
            let collection = collect_dir(&dir, &ctx, &mut sink, &|| false, &mut budget);
            instances.push((collection, sink));
        }
        resolve_duplicates(&mut instances);
        assert_eq!(instances[0].0.reports.len(), 200);
        assert_eq!(instances[0].1.bodies.len(), 200);
        assert_eq!(instances[1].1.bodies.len(), MAX_REPORTS_PER_JOB - 200);
        assert!(instances[0].0.rejected.is_empty());
        assert_eq!(instances[1].0.reports.len(), MAX_REPORTS_PER_JOB - 200);
        assert_eq!(
            instances[1].0.rejected["report_limit"],
            400 - MAX_REPORTS_PER_JOB
        );
        assert_eq!(budget.reports, 0);
        assert_eq!(budget.entries, MAX_ENTRIES_PER_JOB - 400);
        // A later instance with no allowance left examines nothing.
        let dir = tmp.path().join("p3");
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("x.jsonl"), complete("x")).unwrap();
        let mut empty = Budget {
            entries: 0,
            ..budget
        };
        let mut sink = MemorySink::default();
        let collection = collect_dir(
            &dir,
            &instance_ctx(&source, 3, 3),
            &mut sink,
            &|| false,
            &mut empty,
        );
        assert_eq!(collection.rejected["entry_limit"], 1);
        assert!(sink.bodies.is_empty());
    }

    #[test]
    fn instances_of_one_job_share_the_source_byte_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let source = Source::default();
        let report = complete("r1");
        let mut budget = Budget {
            bytes: report.len() as u64,
            ..Budget::default()
        };
        let mut rejected = Vec::new();
        for index in 1..=2u32 {
            let dir = tmp.path().join(format!("p{index}"));
            std::fs::create_dir(&dir).unwrap();
            std::fs::write(dir.join("r.jsonl"), complete(&format!("r{index}"))).unwrap();
            let mut sink = MemorySink::default();
            let ctx = instance_ctx(&source, index, 2);
            rejected.push(collect_dir(&dir, &ctx, &mut sink, &|| false, &mut budget).rejected);
        }
        assert!(rejected[0].is_empty());
        assert_eq!(rejected[1]["job_byte_limit"], 1);
    }
}
