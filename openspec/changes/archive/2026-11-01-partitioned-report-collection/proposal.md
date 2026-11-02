# Proposal

## Why

`add-job-partitions` rejects `reports` on partitioned jobs. Report collection is keyed by job name, so every instance would share one report directory and the collector would scan it once per instance, rejecting every report as a duplicate. Giving each instance its own directory, and taking partition identity from Goodflows, lets partitioned test jobs publish timings with partition metadata the CLI can trust. That metadata is what timing-balanced partitions will later learn from.

## What Changes

- Give each instance of a partitioned reporting job its own report directory (with its own `reports/`, `pending/` and `acks/`) and `GOODFLOWS_REPORT_DIR`. Unpartitioned reporting jobs keep their current layout.
- Record one manifest entry per instance with its partition index, count and own terminal status, and collect each instance's directory once.
- Set `partition` (the index as a decimal string) and `partition_count` from Goodflows for partitioned jobs, in both the envelope and the frozen `suite_started` record, overriding producer values. The server and the local snapshot re-validation both require the two to agree. Unpartitioned jobs keep the producer's values.
- Enforce `report_id` uniqueness across all instances of a job, matching the server identity key `(project, workflow_run_id, job, report_id)`. The existing per-job collection caps (256 reports, 256 MiB of source, and the entry cap) span all instances of a job.
- Add `partition` to reporting events for instances. Replay reads both old manifests (no partition) and new ones.
- Remove the validation rule that rejects `partitions` combined with `reports`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `test-timing-publishing`: collection becomes per instance for partitioned jobs, partition metadata comes from Goodflows, and duplicate detection spans instances.
- `job-partitions`: partitioned jobs may declare `reports`.

## Impact

- CLI: `src/reporting/mod.rs` (`create_run`, `ManifestJob`, `JobDirs`, `Session::finish_with`, `collect_run`, `Item`/`Head` expectations, `deliver` events), `src/reporting/collect.rs` (`JobContext`, `envelope`, `collect_dir` caps and duplicate pass), `src/reporting/replay.rs` (manifest validation of partition entries, per-instance locations, job-wide report_id check), `src/runner.rs` (`ReportingEnv` keyed by job and partition), `src/config.rs` (remove the rejection), `src/output.rs` (partition in reporting events), tests and the README "Test timing reports" section.
- Ordering: `upload-test-timings` is implemented and archived (`openspec/specs/test-timing-publishing/`). `add-job-partitions` must be implemented and archived first, so that the `job-partitions` base spec exists for this change's REMOVED delta.
- Server: no wire-contract change. `partition` and `partition_count` already exist in the envelope, and the server rejects (422) an envelope whose values differ from the initial `suite_started` record (`GoodFlows.TestReports.Consistency`). That is why the override rewrites both.
- GoodCases: its partition migration (`goodcases/openspec/changes/adopt-goodflows-partitions/`) can declare `reports` on the partitioned test job only after this change ships.
