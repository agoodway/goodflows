# Design

## Context

See proposal.md. In the `upload-test-timings` implementation, `create_run` creates `<run>/<path_component(job)>/reports/` per reporting job and returns a `ReportingEnv` whose `jobs` map is keyed by job name. `finish_with` iterates `manifest.jobs`, collects each directory with a `JobContext` built from that entry, and `freeze` writes snapshots. `collect.rs` copies `partition`/`partition_count` from suite_started into the envelope after validating their grammar. Duplicate `report_id` detection is per collected directory. `add-job-partitions` makes `runner::execute` return one `JobResult` per instance with an optional `partition`, and rejects `partitions` with `reports` in `validate_job`.

## Goals / Non-Goals

**Goals:** One report directory and manifest entry per instance. Goodflows-owned partition metadata. Job-wide duplicate detection. Unchanged layout and envelopes for unpartitioned jobs. Backward-compatible replay.

**Non-Goals:** Changing the wire contract. Balancing partitions. Changing producer behavior.

## Decisions

### D1. Directory layout

Partitioned jobs use `<run>/<path_component(job)>/p<index>/reports/`. Unpartitioned jobs keep `<run>/<path_component(job)>/reports/`. The `p<index>` segment cannot collide with an encoded job name because job components are siblings at the run root, never nested. Instance directories are created up front in `create_run`, like today's job directories, so a missing directory still means "spool failure" rather than "instance never started".

*Alternative:* encode the partition into the job component (`test%23p2`). Rejected: it muddles the reversible job-name encoding and scatters a job's instances across the run root.

### D2. Keys and manifest

`ReportingEnv.jobs` becomes keyed by `(String, Option<u32>)`, and `run_job` looks it up with its unit's partition. Manifest job entries gain an optional `partition: { index, count }`, serialized only when present, so unpartitioned manifests stay byte-compatible and older manifests deserialize with `None`. Terminal results are matched to entries by `(name, partition)`.

### D3. Envelope partition override

`JobContext` gains `partition: Option<Partition>`. When it is present, `collect.rs` validates the producer's suite_started as today (malformed producer fields still reject the report, keeping producer validation identical across job kinds). It then sets the envelope's `partition` to `index.to_string()` and `partition_count` to `count`. A mismatch with producer values is not an error: Goodflows is authoritative for instances it scheduled. The server's partition grammar accepts decimal strings, and 1..=64 is within its count range, so no new validation is needed.

### D4. Job-wide duplicate detection

After collecting every directory of one job, and before freezing, compute `report_id` multiplicities across that job's instance collections. Every report whose id appears more than once moves to rejected with `duplicate_report_id`. The existing per-directory check then becomes a special case of the job-wide one. Freezing happens only after this pass, so no snapshot is ever written for a report that is later rejected.

### D5. Events and validation

Reporting events add `partition` from the manifest entry, and text rendering reuses the `name (i/N)` helper from `add-job-partitions`. `validate_job` drops the `partitions` + `reports` rejection. The reporting job-name rules are unchanged.

## Risks / Trade-offs

- [A producer that ignores `GOODFLOWS_REPORT_DIR` and writes to a shared path] -> Collection stays directory-scoped, so those files are never seen. Unchanged from today and documented.
- [Override hides producer partition bugs] -> The producer's values are still grammar-validated. Override is limited to instances Goodflows scheduled.
- [Older CLI replaying a newer manifest] -> Unknown `partition` keys would be rejected by strict manifest parsing. Replay with the CLI that wrote the spool, which is documented.

## Migration Plan

Implement after `upload-test-timings` and `add-job-partitions`. Release the CLI, then GoodCases can declare `reports` on its partitioned test job. Rollback: remove `reports` from partitioned jobs. Spools written by this version remain replayable by it.
