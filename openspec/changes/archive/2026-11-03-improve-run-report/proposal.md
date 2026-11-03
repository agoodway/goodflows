# Proposal

## Why

After `goodflows run` finishes in text mode, the only closing output is one line of status counts. Which jobs failed, why, what was skipped and how long the run took have all scrolled past in interleaved live output. The `summary` event already carries every job result, but the text renderer ignores everything except the counts.

## What Changes

- Replace the text `Summary:` line with a run report rendered from the `summary` event:
  - a header with the workflow name, overall outcome and total wall-clock time;
  - a status table with one row per configured job, where partition instances are rolled up into a single row;
  - status counts by configured job, not by instance;
  - a Failures section with one entry per failed or timed-out instance, showing the failing step, command, error and output tails;
  - the log directory when `--log-dir` is set;
  - a shell-safe rerun hint (`goodflows run <workflow> --jobs=...`) naming the failed or timed-out jobs and the dependents they blocked, and preserving environment selection and an explicitly passed `--config`; when a selected name contains a comma that the existing selector cannot represent, explain the limitation and offer a full-workflow rerun.
  - job-provided text (commands, errors, skip reasons, tails) with control characters escaped.
- Trim the text rendering of a failed or timed-out `job_finished`. It keeps the status line and the error line; the command and output tails move to the report.
- Add `duration_ms` (run wall-clock time) and, when a log directory is in use, `log_dir` to the JSON `summary` event. Add `started` to each summary result to distinguish a scheduled instance that entered execution from one skipped or cancelled before launch. These fields are additive; existing fields, including per-instance counts and result fields, keep their meaning. Live `job_finished` event shapes stay unchanged.
- Reject control characters in job names (workflow, environment and step names already reject them).
- **BREAKING** (text output only): the `Summary: N passed, ...` line and the `All jobs passed` / `All checks succeeded (some jobs skipped)` lines are replaced by the report. JSON output is not broken.

Out of scope: naming which dependency caused a skip, a quiet or compact live mode, and an in-place live dashboard.

## Capabilities

### New Capabilities
- `run-report`: the human-readable report printed at the end of a workflow run in text mode, plus the run-level fields the JSON `summary` event carries for it.

### Modified Capabilities
- `job-partitions`: "Output identifies each instance" currently requires run summary counts to count instances. Text report counts become per configured job; JSON `summary` counts and `run_started` keep counting instances.

## Impact

- `src/output.rs`, new `src/output/report.rs`: new rendering for `summary`, trimmed and escaped `job_finished` failure text.
- `src/config.rs`: job names with control characters fail validation.
- `src/main.rs`: measures run wall time, adds `duration_ms` and `log_dir`, projects each result's `started` marker into the `summary` event, and passes the explicit `--config` path and signal state to the text report.
- `src/runner.rs`: tracks whether each result entered `run_job`, separately from the existing command-spawn `executed` flag used by reporting; the new marker is not serialized in live events.
- `tests/output.rs`, `tests/reporting.rs`, `tests/scheduling.rs`: assertions on `Summary:`, `All jobs passed` and the mid-stream `stdout tail` / `stderr tail` text need updating.
- `src/reporting/replay.rs` (`reports upload`) emits its own `reports_upload_summary` and is unaffected.
- No new dependencies.
