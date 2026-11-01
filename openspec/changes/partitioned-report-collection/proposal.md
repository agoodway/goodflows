# Proposal

## Why

`add-job-partitions` rejects `reports` on partitioned jobs. Report collection is keyed by job name, so every instance would share one report directory and the collector would scan it once per instance, rejecting every report as a duplicate. Giving each instance its own directory, and taking partition identity from Goodflows, lets partitioned test jobs publish timings with partition metadata the CLI can trust. That metadata is what timing-balanced partitions will later learn from.

## What Changes

- Give each instance of a partitioned reporting job its own report directory and `GOODFLOWS_REPORT_DIR`. Unpartitioned reporting jobs keep their current layout.
- Record one manifest entry per instance with its partition index, count and own terminal status, and collect each instance's directory once.
- Set envelope `partition` (the index as a decimal string) and `partition_count` from Goodflows for partitioned jobs, overriding producer values. Unpartitioned jobs keep the producer's values.
- Enforce `report_id` uniqueness across all instances of a job, matching the server identity key `(project, workflow_run_id, job, report_id)`.
- Add `partition` to reporting events for instances. Replay reads both old manifests (no partition) and new ones.
- Remove the validation rule that rejects `partitions` combined with `reports`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `test-timing-publishing`: collection becomes per instance for partitioned jobs, partition metadata comes from Goodflows, and duplicate detection spans instances.
- `job-partitions`: partitioned jobs may declare `reports`.

## Impact

- CLI: `src/reporting/mod.rs` (run creation, manifest, per-instance collection), `src/reporting/collect.rs` (partition override, cross-instance duplicate check), `src/runner.rs` (`ReportingEnv` keyed by job and partition), `src/config.rs` (remove the rejection), `src/output.rs` (partition in reporting events), tests and README.
- Ordering: requires `upload-test-timings` and `add-job-partitions` to be implemented first. Archive it after both, so the base specs exist.
- Server: no wire-contract change. `partition` and `partition_count` already exist in the envelope.
- GoodCases: its partition migration (`goodcases/openspec/changes/adopt-goodflows-partitions/`) can declare `reports` on the partitioned test job only after this change ships.
