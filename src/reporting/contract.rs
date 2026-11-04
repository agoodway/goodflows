//! The authoritative definition of the `test-timings/1` wire contract.
//!
//! goodflows owns this contract: the GoodFlows server mirrors this module and
//! the `test-timing-contract` spec, not the other way round. Every vocabulary,
//! field type, limit and identifier grammar (suite, report, runner and
//! filter-tag ids) lives here, tied to one contract version, and collection
//! types every record against it before a request snapshot is frozen. A
//! frozen snapshot is therefore one a conforming server accepts.
//!
//! The per-event tables pair each core key with a [`FieldType`]. The same
//! tables drive three checks: every core key must be present (nullable ones
//! as `null`), keys outside the common keys, the event's core keys and
//! [`META`] are rejected, and each value is typed. When the contract changes
//! incompatibly, add a new `test-timings/<n>` format, bump
//! [`CONTRACT_VERSION`], and release the CLI before producers emit it.

use serde_json::{Map, Value};

/// The id of the OpenSpec change that defines the contract these tables
/// implement (`openspec/changes/generalize-test-timings`, shared with the
/// GoodFlows server, which pins its mirror of this module to the same change).
///
/// It is not a wire format: it is only sent in the publisher's User-Agent
/// (`goodflows/<version> test-reports/<CONTRACT_VERSION>`) so server logs can
/// tell which contract revision a stale publisher was built against.
/// [`REPORT_FORMAT`] is what the server validates and what a reporter emits.
/// Several revisions of one format may name different changes, so the two
/// change independently: rename this when an archived change gets its dated
/// id or a later change revises the tables; bump [`REPORT_FORMAT`] only for an
/// incompatible wire change.
pub const CONTRACT_VERSION: &str = "generalize-test-timings";

/// The wire format in the envelope and in every `suite_started` record: the
/// [`crate::config::REPORT_FAMILY`] a job requests plus its major version.
pub const REPORT_FORMAT: &str = "test-timings/1";

/// Maximum request body (exceeding it is a 413 on the server).
pub const MAX_REQUEST_BYTES: usize = 32 * 1024 * 1024;
/// Maximum `report.records` length.
pub const MAX_RECORDS: usize = 100_000;
/// Integers are JSON numbers without fraction or exponent and at most 2^53-1.
pub const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;
/// Objects and arrays nest at most this many levels in the request body.
pub const MAX_BODY_DEPTH: usize = 16;
/// Workflow, job and environment names: at most this many UTF-8 bytes.
pub const MAX_NAME_BYTES: usize = 64;
/// Text fields (`group`, `name`, `file`, `path` entries): 1 to this many bytes.
pub const MAX_TEXT_BYTES: usize = 4096;
/// `started_at`: at most this many bytes.
pub const MAX_TIMESTAMP_BYTES: usize = 64;
/// `runner.version`: at most this many bytes.
pub const MAX_RUNNER_VERSION_BYTES: usize = 128;
/// `path`: at most this many entries.
pub const MAX_PATH_ENTRIES: usize = 32;
/// `partition`: 1 to this many bytes.
pub const MAX_PARTITION_BYTES: usize = 64;
/// Maximum entries in each of `filters.include` and `filters.exclude`.
pub const MAX_FILTER_ENTRIES: usize = 256;
/// Suite, report, runner, `meta` key and filter-tag identifiers: at most this many bytes.
pub const MAX_IDENTIFIER_BYTES: usize = 128;
/// `meta`: at most this many keys.
pub const MAX_META_KEYS: usize = 32;
/// `meta` string values: at most this many bytes.
pub const MAX_META_STRING_BYTES: usize = 256;

/// The exact top-level envelope keys.
pub const ENVELOPE: &[&str] = &[
    "format",
    "workflow_run_id",
    "workflow",
    "environment",
    "job",
    "job_status",
    "source",
    "report",
];
pub const SOURCE: &[&str] = &["commit", "branch", "dirty"];
pub const REPORT: &[&str] = &[
    "suite_id",
    "report_id",
    "partition",
    "partition_count",
    "truncated",
    "records",
];
pub const JOB_STATUSES: &[&str] = &[
    "passed",
    "failed",
    "timed_out",
    "cancelled",
    "skipped",
    "unknown",
];

/// Runner-neutral outcomes for tests and groups, and the only `counts` keys.
pub const OUTCOMES: &[&str] = &["passed", "failed", "skipped", "errored"];

/// The shapes a core value can take.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldType {
    /// A string of 1 to [`MAX_TEXT_BYTES`] bytes.
    Text,
    /// [`FieldType::Text`] or null.
    OptText,
    /// The identifier grammar ([`valid_identifier`]).
    Identifier,
    /// An RFC 3339 UTC timestamp of at most [`MAX_TIMESTAMP_BYTES`] bytes.
    Timestamp,
    /// An integer from 0 to 2^53-1.
    UInt,
    /// [`FieldType::UInt`] or null.
    OptUInt,
    /// Null or an integer whose magnitude is at most 2^53-1.
    OptInt,
    /// `true` or `false`.
    Bool,
    /// Null or 1 to [`MAX_PARTITION_BYTES`] bytes of `[A-Za-z0-9._-]`.
    Partition,
    /// Null or an integer from 1 to 2^53-1.
    PartitionCount,
    /// An integer from 1 to 2^53-1.
    ExecutionId,
    /// Null or an integer from 1 to 2^53-1.
    Line,
    /// One of [`OUTCOMES`].
    Outcome,
    /// An object whose keys are a subset of [`OUTCOMES`] with [`FieldType::UInt`] values.
    Counts,
    /// An array of at most [`MAX_PATH_ENTRIES`] [`FieldType::Text`] strings, outermost first.
    Path,
    /// Exactly `{name: Identifier, version: null | string of at most 128 bytes}`.
    Runner,
    /// Null or exactly `{include, exclude}` of boolean identifier tags.
    Filters,
    /// Exactly [`REPORT_FORMAT`].
    Format,
}

pub type Field = (&'static str, FieldType);

use FieldType::*;

/// The key naming each record's event.
pub const EVENT: &str = "event";
/// The optional, flat runner metadata object allowed on every record.
pub const META: &str = "meta";
/// Keys every record carries besides `event` (and optional `meta`).
pub const COMMON: &[Field] = &[
    ("suite_id", Identifier),
    ("report_id", Identifier),
    ("received_offset_us", UInt),
];

pub const SUITE_STARTED: &[Field] = &[
    ("format", Format),
    ("started_at", Timestamp),
    ("runner", Runner),
    ("partition", Partition),
    ("partition_count", PartitionCount),
    ("seed", OptInt),
    ("filters", Filters),
];
pub const GROUP_STARTED: &[Field] = &[
    ("group", Text),
    ("file", OptText),
    ("execution_id", ExecutionId),
    ("started_offset_us", UInt),
];
pub const GROUP_FINISHED_EXTRA: &[Field] = &[
    ("duration_us", OptUInt),
    ("outcome", Outcome),
    ("counts", Counts),
];
pub const TEST_STARTED: &[Field] = &[
    ("group", Text),
    ("path", Path),
    ("name", Text),
    ("file", OptText),
    ("line", Line),
    ("execution_id", ExecutionId),
    ("started_offset_us", UInt),
];
pub const TEST_FINISHED_EXTRA: &[Field] = &[("duration_us", OptUInt), ("outcome", Outcome)];
pub const SUITE_FINISHED: &[Field] = &[
    ("aborted", Bool),
    ("unfinished_tests", UInt),
    ("unfinished_groups", UInt),
    ("counts", Counts),
    ("duration_us", OptUInt),
];

/// What a finished event pairs with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Group,
    Test,
}

/// One event's core keys, beyond [`EVENT`], [`COMMON`] and [`META`].
#[derive(Debug, Clone, Copy)]
pub struct Shape {
    /// Keys a finished event repeats from its started event, with identical
    /// values; for every other event, its whole core key set.
    pub base: &'static [Field],
    /// Keys a finished event adds to its started event's keys.
    pub extra: &'static [Field],
    /// Some for a started event (`started: true`) or a finished event.
    pub pairing: Option<(Kind, bool)>,
}

impl Shape {
    pub fn fields(&self) -> impl Iterator<Item = &'static Field> {
        self.base.iter().chain(self.extra)
    }

    pub fn contains(&self, key: &str) -> bool {
        self.fields().any(|(k, _)| *k == key)
    }
}

const fn event(
    base: &'static [Field],
    extra: &'static [Field],
    pairing: Option<(Kind, bool)>,
) -> Shape {
    Shape {
        base,
        extra,
        pairing,
    }
}

/// Every event the contract defines, with its core shape. This table is the
/// only list of events: [`shape`] and [`check_record`] both read it.
pub const EVENTS: &[(&str, Shape)] = &[
    ("suite_started", event(SUITE_STARTED, &[], None)),
    (
        "group_started",
        event(GROUP_STARTED, &[], Some((Kind::Group, true))),
    ),
    (
        "group_finished",
        event(
            GROUP_STARTED,
            GROUP_FINISHED_EXTRA,
            Some((Kind::Group, false)),
        ),
    ),
    (
        "test_started",
        event(TEST_STARTED, &[], Some((Kind::Test, true))),
    ),
    (
        "test_finished",
        event(TEST_STARTED, TEST_FINISHED_EXTRA, Some((Kind::Test, false))),
    ),
    ("suite_finished", event(SUITE_FINISHED, &[], None)),
    ("interrupted", event(&[], &[], None)),
    ("max_failures_reached", event(&[], &[], None)),
];

/// The core shape of an event, or `None` for an event outside the contract.
pub fn shape(event: &str) -> Option<Shape> {
    EVENTS
        .iter()
        .find(|(name, _)| *name == event)
        .map(|(_, shape)| *shape)
}

/// Whether `key` may appear on a record of `event`, or `None` for an unknown event.
pub fn allowed(event: &str, key: &str) -> Option<bool> {
    let shape = shape(event)?;
    Some(
        key == EVENT || key == META || COMMON.iter().any(|(k, _)| *k == key) || shape.contains(key),
    )
}

/// The identifier grammar for suite, report and runner ids, `meta` keys and
/// filter tags: 1 to [`MAX_IDENTIFIER_BYTES`] bytes of ASCII letters, digits,
/// `.`, `_` or `-`, starting with a letter or digit. Every byte is checked, so
/// unlike a regex anchored with `$` a trailing newline never matches.
pub fn valid_identifier(id: &str) -> bool {
    let bytes = id.as_bytes();
    bytes.len() <= MAX_IDENTIFIER_BYTES
        && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
        && identifier_bytes(bytes)
}

/// The one character rule shared by identifiers and `partition`: every byte
/// is an ASCII letter, digit, `.`, `_` or `-`.
fn identifier_bytes(bytes: &[u8]) -> bool {
    bytes
        .iter()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A `filters.include`/`filters.exclude` tag follows the identifier grammar;
/// its value is a boolean.
pub fn valid_filter_tag(tag: &str) -> bool {
    valid_identifier(tag)
}

fn safe_uint(value: &Value) -> Option<u64> {
    value.as_u64().filter(|n| *n <= MAX_SAFE_INTEGER)
}

fn safe_int(value: &Value) -> bool {
    match value {
        Value::Number(n) => {
            n.as_u64().is_some_and(|n| n <= MAX_SAFE_INTEGER)
                || n.as_i64()
                    .is_some_and(|n| n.unsigned_abs() <= MAX_SAFE_INTEGER)
        }
        _ => false,
    }
}

fn text(value: &Value, limit: usize) -> Result<(), &'static str> {
    match value {
        Value::String(s) if s.is_empty() => Err("invalid_field"),
        Value::String(s) if s.len() > limit => Err("field_too_long"),
        Value::String(_) => Ok(()),
        _ => Err("invalid_field"),
    }
}

/// `YYYY-MM-DDTHH:MM:SS[.fraction](Z|+00:00|-00:00)`, case-insensitive `T`/`Z`.
fn valid_timestamp(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |range: std::ops::Range<usize>| -> Option<u32> {
        let part = b.get(range)?;
        part.iter()
            .all(u8::is_ascii_digit)
            .then(|| part.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0')))
    };
    let (Some(year), Some(month), Some(day), Some(hour), Some(minute), Some(second)) = (
        digits(0..4),
        digits(5..7),
        digits(8..10),
        digits(11..13),
        digits(14..16),
        digits(17..19),
    ) else {
        return false;
    };
    if b[4] != b'-'
        || b[7] != b'-'
        || !matches!(b[10], b'T' | b't')
        || b[13] != b':'
        || b[16] != b':'
    {
        return false;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return false,
    };
    if day == 0 || day > days || hour > 23 || minute > 59 || second > 60 {
        return false;
    }
    let mut rest = &b[19..];
    if let Some(fraction) = rest.strip_prefix(b".") {
        let n = fraction.iter().take_while(|d| d.is_ascii_digit()).count();
        if n == 0 {
            return false;
        }
        rest = &fraction[n..];
    }
    matches!(rest, b"Z" | b"z" | b"+00:00" | b"-00:00")
}

fn filters(value: &Value) -> Result<(), &'static str> {
    let unsupported_filter = Err("unsupported_filter");
    let filters = match value {
        Value::Null => return Ok(()),
        Value::Object(filters) => filters,
        _ => return unsupported_filter,
    };
    if filters.keys().any(|k| k != "include" && k != "exclude") {
        return Err("unsupported_field");
    }
    for list in ["include", "exclude"] {
        let Some(entries) = filters.get(list).and_then(Value::as_array) else {
            return unsupported_filter;
        };
        if entries.len() > MAX_FILTER_ENTRIES {
            return unsupported_filter;
        }
        for entry in entries {
            let Value::Object(entry) = entry else {
                return unsupported_filter;
            };
            if entry.keys().any(|k| k != "tag" && k != "value") {
                return Err("unsupported_field");
            }
            let tag = entry.get("tag").and_then(Value::as_str);
            if !tag.is_some_and(valid_filter_tag)
                || !entry.get("value").is_some_and(Value::is_boolean)
            {
                return unsupported_filter;
            }
        }
    }
    Ok(())
}

/// Type one core value, returning the structural code of the first violation.
pub fn check(ty: FieldType, value: &Value) -> Result<(), &'static str> {
    let invalid = Err("invalid_field");
    let ok = |valid: bool| if valid { Ok(()) } else { Err("invalid_field") };
    match ty {
        Text => text(value, MAX_TEXT_BYTES),
        OptText => match value {
            Value::Null => Ok(()),
            value => text(value, MAX_TEXT_BYTES),
        },
        Identifier => match value.as_str() {
            Some(id) if valid_identifier(id) => Ok(()),
            _ => Err("invalid_identifier"),
        },
        Timestamp => match value.as_str() {
            Some(s) if s.len() > MAX_TIMESTAMP_BYTES => Err("field_too_long"),
            Some(s) => ok(valid_timestamp(s)),
            None => invalid,
        },
        UInt => ok(safe_uint(value).is_some()),
        OptUInt => ok(value.is_null() || safe_uint(value).is_some()),
        OptInt => ok(value.is_null() || safe_int(value)),
        Bool => ok(value.is_boolean()),
        Partition => match value {
            Value::Null => Ok(()),
            Value::String(p)
                if !p.is_empty()
                    && p.len() <= MAX_PARTITION_BYTES
                    && identifier_bytes(p.as_bytes()) =>
            {
                Ok(())
            }
            _ => Err("invalid_partition"),
        },
        PartitionCount => match value {
            Value::Null => Ok(()),
            value if safe_uint(value).is_some_and(|n| n >= 1) => Ok(()),
            _ => Err("invalid_partition_count"),
        },
        ExecutionId => ok(safe_uint(value).is_some_and(|n| n >= 1)),
        Line => ok(value.is_null() || safe_uint(value).is_some_and(|n| n >= 1)),
        Outcome => ok(value.as_str().is_some_and(|o| OUTCOMES.contains(&o))),
        Counts => {
            let Value::Object(counts) = value else {
                return invalid;
            };
            for (key, count) in counts {
                if !OUTCOMES.contains(&key.as_str()) {
                    return Err("unsupported_field");
                }
                if safe_uint(count).is_none() {
                    return invalid;
                }
            }
            Ok(())
        }
        Path => {
            let Value::Array(entries) = value else {
                return invalid;
            };
            if entries.len() > MAX_PATH_ENTRIES {
                return invalid;
            }
            entries
                .iter()
                .try_for_each(|entry| text(entry, MAX_TEXT_BYTES))
        }
        Runner => {
            let Value::Object(runner) = value else {
                return invalid;
            };
            if runner.keys().any(|k| k != "name" && k != "version") {
                return Err("unsupported_field");
            }
            let (Some(name), Some(version)) = (runner.get("name"), runner.get("version")) else {
                return Err("missing_field");
            };
            if !name.as_str().is_some_and(valid_identifier) {
                return invalid;
            }
            match version {
                Value::Null => Ok(()),
                Value::String(v) if v.len() > MAX_RUNNER_VERSION_BYTES => Err("field_too_long"),
                Value::String(_) => Ok(()),
                _ => invalid,
            }
        }
        Filters => filters(value),
        Format => {
            if value == REPORT_FORMAT {
                Ok(())
            } else {
                Err("unsupported_format")
            }
        }
    }
}

/// `meta` is flat and bounded: an object of at most [`MAX_META_KEYS`]
/// identifier keys whose values are null, booleans, safe integers or strings
/// of at most [`MAX_META_STRING_BYTES`] bytes. Any violation is `invalid_meta`.
pub fn valid_meta(value: &Value) -> bool {
    let Value::Object(meta) = value else {
        return false;
    };
    meta.len() <= MAX_META_KEYS
        && meta.iter().all(|(key, value)| {
            valid_identifier(key)
                && match value {
                    Value::Null | Value::Bool(_) => true,
                    Value::Number(_) => safe_int(value),
                    Value::String(s) => s.len() <= MAX_META_STRING_BYTES,
                    Value::Array(_) | Value::Object(_) => false,
                }
        })
}

/// Type one record against its event's table: unknown keys, missing core
/// keys, core types (in table order, so the code is deterministic) and `meta`.
/// Identity, order and pairing are checked across records by collection.
pub fn check_record(map: &Map<String, Value>) -> Result<Shape, &'static str> {
    let event = map.get(EVENT).ok_or("missing_field")?;
    let shape = event.as_str().and_then(shape).ok_or("unsupported_event")?;
    let event = event.as_str().unwrap_or_default();
    if map.keys().any(|key| allowed(event, key) != Some(true)) {
        return Err("unsupported_field");
    }
    for (key, ty) in COMMON.iter().chain(shape.fields()) {
        let value = map.get(*key).ok_or("missing_field")?;
        check(*ty, value)?;
    }
    if map.get(META).is_some_and(|meta| !valid_meta(meta)) {
        return Err("invalid_meta");
    }
    Ok(shape)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn keys(event: &str) -> Vec<&'static str> {
        shape(event).unwrap().fields().map(|(k, _)| *k).collect()
    }

    #[test]
    fn contract_is_versioned_and_names_its_format() {
        assert_eq!(CONTRACT_VERSION, "generalize-test-timings");
        assert_eq!(REPORT_FORMAT, "test-timings/1");
        assert_eq!(MAX_REQUEST_BYTES, 33_554_432);
        assert_eq!(MAX_FILTER_ENTRIES, 256);
        assert_eq!(MAX_IDENTIFIER_BYTES, 128);
        assert_eq!(ENVELOPE.len(), 8);
        assert_eq!(OUTCOMES, ["passed", "failed", "skipped", "errored"]);
    }

    #[test]
    fn every_event_has_its_core_key_set() {
        let common: Vec<_> = COMMON.iter().map(|(k, _)| *k).collect();
        assert_eq!(common, ["suite_id", "report_id", "received_offset_us"]);
        assert_eq!(
            keys("suite_started"),
            [
                "format",
                "started_at",
                "runner",
                "partition",
                "partition_count",
                "seed",
                "filters"
            ]
        );
        assert_eq!(
            keys("group_started"),
            ["group", "file", "execution_id", "started_offset_us"]
        );
        assert_eq!(
            keys("group_finished"),
            [
                "group",
                "file",
                "execution_id",
                "started_offset_us",
                "duration_us",
                "outcome",
                "counts"
            ]
        );
        assert_eq!(
            keys("test_started"),
            [
                "group",
                "path",
                "name",
                "file",
                "line",
                "execution_id",
                "started_offset_us"
            ]
        );
        assert_eq!(
            keys("test_finished"),
            [
                "group",
                "path",
                "name",
                "file",
                "line",
                "execution_id",
                "started_offset_us",
                "duration_us",
                "outcome"
            ]
        );
        assert_eq!(
            keys("suite_finished"),
            [
                "aborted",
                "unfinished_tests",
                "unfinished_groups",
                "counts",
                "duration_us"
            ]
        );
        assert!(keys("interrupted").is_empty());
        assert!(keys("max_failures_reached").is_empty());
        let events: Vec<_> = EVENTS.iter().map(|(event, _)| *event).collect();
        assert_eq!(
            events,
            [
                "suite_started",
                "group_started",
                "group_finished",
                "test_started",
                "test_finished",
                "suite_finished",
                "interrupted",
                "max_failures_reached"
            ]
        );
        for event in events {
            assert!(shape(event).is_some(), "{event}");
            assert_eq!(allowed(event, "meta"), Some(true));
            assert_eq!(allowed(event, "suite_id"), Some(true));
            assert_eq!(allowed(event, "run_id"), Some(false));
        }
        for retired in ["module_started", "module_finished", "log"] {
            assert!(shape(retired).is_none());
        }
        assert_eq!(allowed("test_started", "duration_us"), Some(false));
        assert_eq!(allowed("interrupted", "reason"), Some(false));
    }

    #[test]
    fn field_types_follow_the_table() {
        let types: Vec<_> = TEST_STARTED.iter().map(|(_, t)| *t).collect();
        assert_eq!(types, [Text, Path, Text, OptText, Line, ExecutionId, UInt]);
        let cases: &[(FieldType, Value, Result<(), &str>)] = &[
            (Text, json!("x"), Ok(())),
            (Text, json!(""), Err("invalid_field")),
            (Text, json!(null), Err("invalid_field")),
            (Text, json!("m".repeat(4096)), Ok(())),
            (Text, json!("é".repeat(2049)), Err("field_too_long")),
            (OptText, json!(null), Ok(())),
            (OptText, json!(1), Err("invalid_field")),
            (Identifier, json!("a b"), Err("invalid_identifier")),
            (Timestamp, json!("2026-11-01T00:00:00Z"), Ok(())),
            (Timestamp, json!("2026-11-01T00:00:00.123456Z"), Ok(())),
            (Timestamp, json!("2026-11-01T00:00:00+00:00"), Ok(())),
            (
                Timestamp,
                json!("2026-11-01T00:00:00+01:00"),
                Err("invalid_field"),
            ),
            (
                Timestamp,
                json!("2026-02-30T00:00:00Z"),
                Err("invalid_field"),
            ),
            (Timestamp, json!("2026-11-01"), Err("invalid_field")),
            (
                Timestamp,
                json!(format!("2026-11-01T00:00:00.{}Z", "1".repeat(50))),
                Err("field_too_long"),
            ),
            (UInt, json!(0), Ok(())),
            (UInt, json!(-1), Err("invalid_field")),
            (UInt, json!("12"), Err("invalid_field")),
            (UInt, json!(MAX_SAFE_INTEGER), Ok(())),
            (OptUInt, json!(null), Ok(())),
            (OptUInt, json!(-5), Err("invalid_field")),
            (OptInt, json!(-1843), Ok(())),
            (OptInt, json!(null), Ok(())),
            (
                OptInt,
                json!(-(MAX_SAFE_INTEGER as i64) - 1),
                Err("invalid_field"),
            ),
            (OptInt, json!(true), Err("invalid_field")),
            (Bool, json!(false), Ok(())),
            (Bool, json!(0), Err("invalid_field")),
            (Partition, json!("p.1_a-B"), Ok(())),
            (Partition, json!("a b"), Err("invalid_partition")),
            (Partition, json!(1), Err("invalid_partition")),
            (Partition, json!("x".repeat(65)), Err("invalid_partition")),
            (PartitionCount, json!(null), Ok(())),
            (PartitionCount, json!(0), Err("invalid_partition_count")),
            (ExecutionId, json!(1), Ok(())),
            (ExecutionId, json!(0), Err("invalid_field")),
            (ExecutionId, json!(null), Err("invalid_field")),
            (Line, json!(null), Ok(())),
            (Line, json!(0), Err("invalid_field")),
            (Outcome, json!("errored"), Ok(())),
            (Outcome, json!("todo"), Err("invalid_field")),
            (Outcome, json!("excluded"), Err("invalid_field")),
            (Counts, json!({}), Ok(())),
            (Counts, json!({"passed": 1, "errored": 0}), Ok(())),
            (Counts, json!({"excluded": 1}), Err("unsupported_field")),
            (Counts, json!({"passed": -1}), Err("invalid_field")),
            (Counts, json!([]), Err("invalid_field")),
            (Path, json!([]), Ok(())),
            (Path, json!(["math", "sum"]), Ok(())),
            (Path, Value::Array(vec![json!("d"); 32]), Ok(())),
            (
                Path,
                Value::Array(vec![json!("d"); 33]),
                Err("invalid_field"),
            ),
            (Path, json!([""]), Err("invalid_field")),
            (Path, json!(null), Err("invalid_field")),
            (Runner, json!({"name": "jest", "version": "30.0.0"}), Ok(())),
            (Runner, json!({"name": "exunit", "version": null}), Ok(())),
            (
                Runner,
                json!({"name": "jest runner", "version": null}),
                Err("invalid_field"),
            ),
            (Runner, json!({"name": "jest"}), Err("missing_field")),
            (
                Runner,
                json!({"name": "jest", "version": null, "x": 1}),
                Err("unsupported_field"),
            ),
            (
                Runner,
                json!({"name": "jest", "version": "1".repeat(129)}),
                Err("field_too_long"),
            ),
            (Filters, json!(null), Ok(())),
            (Filters, json!({"include": [], "exclude": []}), Ok(())),
            (Filters, json!({"include": []}), Err("unsupported_filter")),
            (Filters, json!([]), Err("unsupported_filter")),
            // Unknown keys in `filters` or in an entry are `unsupported_field`,
            // like unknown keys in `counts` and `runner`.
            (
                Filters,
                json!({"include": [], "exclude": [], "x": 1}),
                Err("unsupported_field"),
            ),
            (
                Filters,
                json!({"include": [{"tag": "slow", "value": true, "note": "x"}], "exclude": []}),
                Err("unsupported_field"),
            ),
            (Format, json!("test-timings/1"), Ok(())),
            (Format, json!("exunit-timings"), Err("unsupported_format")),
            (Format, json!("test-timings/2"), Err("unsupported_format")),
        ];
        for (ty, value, expected) in cases {
            assert_eq!(check(*ty, value), *expected, "{ty:?} {value}");
        }
    }

    #[test]
    fn runner_metadata_is_flat_and_bounded() {
        // ExUnit runner details.
        assert!(valid_meta(
            &json!({"elixir_version": "1.20.0", "otp_release": "28", "max_cases": 16})
        ));
        assert!(valid_meta(&json!({})));
        assert!(valid_meta(
            &json!({"a": null, "b": true, "c": -9007199254740991_i64})
        ));
        // Structured values.
        assert!(!valid_meta(&json!({"tags": ["a", "b"]})));
        assert!(!valid_meta(&json!({"x": {"x": 1}})));
        // Too many keys.
        let keys = |n: usize| Value::Object((0..n).map(|i| (format!("k{i}"), json!(1))).collect());
        assert!(valid_meta(&keys(32)));
        assert!(!valid_meta(&keys(33)));
        // Long strings.
        assert!(valid_meta(&json!({"s": "s".repeat(256)})));
        assert!(!valid_meta(&json!({"s": "s".repeat(257)})));
        // Keys follow the identifier grammar; integers stay in the safe range.
        assert!(!valid_meta(&json!({"skip reason": "x"})));
        assert!(!valid_meta(&json!({"": 1})));
        assert!(!valid_meta(&json!({"n": MAX_SAFE_INTEGER + 1})));
        // Not an object.
        assert!(!valid_meta(&json!([])));
        assert!(!valid_meta(&json!(null)));
    }
}
