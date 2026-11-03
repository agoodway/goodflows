//! The local copy of the server's test-report wire contract.
//!
//! Every server-defined vocabulary, limit and identifier grammar (suite,
//! report and filter-tag ids) the publisher enforces locally lives here, tied
//! to one contract version. The authoritative definition is
//! the GoodFlows server change `2026-11-01-store-test-timings` (design D1), as
//! amended by `2026-11-01-generalize-test-report-filters` (filter tags are
//! boolean identifiers and each filter list holds at most [`MAX_FILTER_ENTRIES`]).
//!
//! Coupling: collection rejects anything outside these lists *before* a
//! request snapshot is frozen, so a report the server would accept but this
//! table does not (for example an event or field the server added later) is
//! rejected locally and never becomes replayable; only its raw JSONL stays on
//! disk. When the server contract changes, update this module, bump
//! [`CONTRACT_VERSION`] and release the CLI before producers emit the new
//! vocabulary.

/// Identifies the server contract revision these tables were copied from.
pub const CONTRACT_VERSION: &str = "2026-11-01-generalize-test-report-filters";

pub const REPORT_FORMAT: &str = "exunit-timings";

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

/// Keys every event carries.
pub const COMMON: &[&str] = &["event", "run_id", "report_id", "received_offset_us"];
pub const SUITE_STARTED: &[&str] = &[
    "format",
    "started_at",
    "partition",
    "partition_count",
    "seed",
    "max_cases",
    "schedulers",
    "elixir_version",
    "otp_release",
    "filters",
];
pub const TEST_STARTED: &[&str] = &[
    "module",
    "name",
    "description",
    "file",
    "line",
    "describe",
    "async",
    "parameter_id",
    "execution_id",
    "started_offset_us",
];
pub const TEST_FINISHED_EXTRA: &[&str] = &["duration_us", "outcome"];
pub const MODULE_STARTED: &[&str] = &[
    "module",
    "file",
    "async",
    "parameter_id",
    "execution_id",
    "started_offset_us",
    "setup_all",
];
pub const MODULE_FINISHED_EXTRA: &[&str] = &["observed_duration_us", "outcome", "counts"];
pub const SUITE_FINISHED: &[&str] = &[
    "aborted",
    "unfinished_tests",
    "unfinished_modules",
    "counts",
    "files",
    "times_us",
];
pub const OUTCOMES: &[&str] = &["passed", "failed", "skipped", "excluded", "invalid"];
pub const TIMES: &[&str] = &["run", "async", "sync", "load", "total"];
pub const BUCKET: &[&str] = &[
    "test_count",
    "module_count",
    "counts",
    "test_duration_us",
    "observed_module_duration_us",
];
pub const FILE_EXTRA: &[&str] = &["file", "async", "sync", "unknown"];
/// Fields limited to 4,096 bytes.
pub const LONG_FIELDS: &[&str] = &["module", "name", "description", "describe", "file"];
/// Fields limited to 128 bytes.
pub const VERSION_FIELDS: &[&str] = &["elixir_version", "otp_release"];
/// Maximum entries in each of `filters.include` and `filters.exclude`.
pub const MAX_FILTER_ENTRIES: usize = 256;
/// Suite, report and filter-tag identifiers: at most this many bytes.
pub const MAX_IDENTIFIER_BYTES: usize = 128;

/// The server's identifier grammar for suite and report ids: 1 to
/// [`MAX_IDENTIFIER_BYTES`] bytes of ASCII letters, digits, `.`, `_` or `-`,
/// starting with a letter or digit. Every byte is checked, so unlike a regex
/// anchored with `$` a trailing newline never matches.
pub fn valid_identifier(id: &str) -> bool {
    let bytes = id.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= MAX_IDENTIFIER_BYTES
        && bytes[0].is_ascii_alphanumeric()
        && bytes
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// A `filters.include`/`filters.exclude` tag follows the identifier grammar
/// (server change `2026-11-01-generalize-test-report-filters`); its value is a boolean.
pub fn valid_filter_tag(tag: &str) -> bool {
    valid_identifier(tag)
}

/// Allowed keys for an event, or `None` for an unknown event.
pub fn allowed(event: &str, key: &str) -> Option<bool> {
    let sets: &[&[&str]] = match event {
        "suite_started" => &[SUITE_STARTED],
        "test_started" => &[TEST_STARTED],
        "test_finished" => &[TEST_STARTED, TEST_FINISHED_EXTRA],
        "module_started" => &[MODULE_STARTED],
        "module_finished" => &[MODULE_STARTED, MODULE_FINISHED_EXTRA],
        "suite_finished" => &[SUITE_FINISHED],
        "max_failures_reached" | "interrupted" => &[],
        _ => return None,
    };
    Some(COMMON.contains(&key) || sets.iter().any(|set| set.contains(&key)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_is_versioned_and_matches_the_server_vocabulary() {
        assert_eq!(
            CONTRACT_VERSION,
            "2026-11-01-generalize-test-report-filters"
        );
        assert_eq!(MAX_REQUEST_BYTES, 33_554_432);
        assert_eq!(MAX_FILTER_ENTRIES, 256);
        assert_eq!(MAX_IDENTIFIER_BYTES, 128);
        assert_eq!(ENVELOPE.len(), 8);
        assert_eq!(allowed("suite_started", "filters"), Some(true));
        assert_eq!(allowed("test_started", "duration_us"), Some(false));
        assert_eq!(allowed("log", "event"), None);
    }
}
