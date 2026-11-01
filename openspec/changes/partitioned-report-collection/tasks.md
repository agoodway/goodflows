# Tasks

## 1. Per-instance directories and manifest

- [ ] 1.1 Key `ReportingEnv` by job name and partition, create `<job>/p<index>/reports/` per instance in `create_run`, and look up each unit's reserved env in `run_job`; verify with `tests/reporting.rs` that three instances receive distinct `GOODFLOWS_REPORT_DIR` values and one shared `GOODFLOWS_RUN_ID`, and that an unpartitioned job's directory path is unchanged.
- [ ] 1.2 Add an optional `partition` to manifest job entries, match terminal results by `(name, partition)`, and collect each instance directory once; verify with `tests/reporting.rs` that a 3-partition job yields three manifest entries with their own statuses, that unpartitioned manifest JSON is unchanged, and that an older manifest fixture without `partition` still deserializes.
- [ ] 1.3 Remove the `partitions` + `reports` rejection from `validate_job` and update the README reporting section with the per-instance layout; verify that the inspection test asserting the rejection is replaced by one asserting `goodflows validate` accepts the combination.

## 2. Envelope metadata and duplicate detection

- [ ] 2.1 Pass the instance partition through `JobContext` and override envelope `partition`/`partition_count` after producer validation; verify with `collect.rs` unit tests for producer null values, producer mismatch, malformed producer partition (still rejected) and unpartitioned pass-through.
- [ ] 2.2 Move `duplicate_report_id` detection to a job-wide pass across instance collections before freezing; verify with tests that duplicates across two instances reject all copies with files left untouched and no snapshots written, and that same-directory duplicates still reject.

## 3. Events, replay and docs

- [ ] 3.1 Add `partition` to `reports_collected`, `report_uploaded` and `report_upload_pending` events and render `job (i/N)` in text mode; verify with `tests/output.rs` and `tests/reporting.rs` for text and `--json`, including unchanged events for unpartitioned jobs.
- [ ] 3.2 Verify replay of a saved 3-partition invocation against the mock server sends three requests with partitions `"1"`..`"3"` and count 3, and that replaying an older-format fixture is unchanged; document per-instance replay in README.

## 4. Integration check

- [ ] 4.1 Run `cargo fmt --check`, `cargo clippy --locked -- -D warnings`, `cargo test --locked` and the repository's `goodflows` checks. Then run the built CLI on a scratch config with a 3-partition reporting job whose command writes a synthetic JSONL report into `$GOODFLOWS_REPORT_DIR`, against a local mock endpoint. Completion requires all checks to pass and three acknowledged uploads with distinct partitions.
