# Design

## Context

See proposal.md. In the implemented `upload-test-timings` code (`src/reporting/`, spec `test-timing-publishing`), `create_run` creates `<run>/<path_component(job)>/` with `reports/`, `pending/` and `acks/` (mode 0700) per reporting job. It fills `Run.env: HashMap<String, Vec<(String, String)>>`, keyed by job name, which `Session::runner_env` hands to the runner as `ReportingEnv.jobs`. `Session::finish_with` matches results to `manifest.jobs` by name (`results.iter().find(|r| r.name == name)`). `collect_run` then calls `collect::collect_dir` once per entry, with a `JobContext` and a `SpoolSink` whose `freeze` writes `pending/NNN.json` as each body is built (index counter starting at 0), so at most one body is in memory. `parse_report` copies `partition`/`partition_count` from the first record. `validate_records` checks their grammar. `envelope` writes them into the envelope, and `validate_envelope` fails with `identity_mismatch` unless the envelope values equal the first record's. The server enforces the same equality. Duplicate `report_id` detection is per `collect_dir` call: duplicates are frozen and then removed with `Sink::discard`. The caps `MAX_REPORTS_PER_JOB`, `MAX_JOB_SOURCE_BYTES` and `MAX_ENTRIES_PER_JOB` also apply per call. Replay (`replay.rs`) requires `ManifestJob.dir == path_component(name)`, derives `JobDirs` from it, and checks report_id uniqueness per manifest entry. `add-job-partitions` makes `runner::execute` return one `JobResult` per instance with an optional `partition`, and rejects `partitions` with `reports` in `validate_job`.

## Goals / Non-Goals

**Goals:** One report directory and manifest entry per instance. Goodflows-owned partition metadata. Job-wide duplicate detection. Unchanged layout and envelopes for unpartitioned jobs. Backward-compatible replay.

**Non-Goals:** Changing the wire contract. Balancing partitions. Changing producer behavior.

## Decisions

### D1. Directory layout

Partitioned jobs (including `partitions = 1`) use `<run>/<path_component(job)>/p<index>/` with its own `reports/`, `pending/` and `acks/`. Unpartitioned jobs keep `<run>/<path_component(job)>/{reports,pending,acks}/`. Per-instance `pending/` and `acks/` are required because each instance's `SpoolSink` numbers snapshots from 0, so a shared `pending/` would collide. The `p<index>` segment cannot collide with an encoded job name because job components are siblings at the run root, never nested. The job directory and every instance directory are created privately up front in `create_run`, like today's job directories, so a missing directory still means "spool failure" rather than "instance never started". `JobDirs` gains the instance path, and its `real_dir` checks cover both the job and the instance directory, so neither may be replaced by a symlink.

*Alternative:* encode the partition into the job component (`test%23p2`). Rejected: it muddles the reversible job-name encoding and scatters a job's instances across the run root.

### D2. Keys and manifest

`ReportingEnv.jobs` becomes keyed by `ReportingKey = (String, Option<Partition>)`, and `run_job` looks it up with its unit's partition, so neither side converts the partition. `ReportingEnv::reserved`/`inject` and `JobExecution::command` take the partition alongside `&job.name`. Manifest job entries (`ManifestJob`, `deny_unknown_fields`) gain an optional `partition: { index, count }` with `#[serde(default, skip_serializing_if = "Option::is_none")]`, so unpartitioned entries keep their exact shape and older manifests deserialize with `None`. The saved form is its own type, `reporting::ManifestPartition` (`deny_unknown_fields`, `From<Partition>` and back), so the strict on-disk schema does not depend on `config::Partition`, which is only serialized into events and results. `dir` stays `path_component(name)`, and the instance location `<dir>/p<index>` is derived from the partition, never read from the manifest. Terminal results are matched to entries by `(name, partition)`.

The manifest `version` is 1 when no entry has a partition, so unpartitioned manifests are unchanged and still readable by every earlier goodflows. A manifest with at least one partition entry is written as version 2: a goodflows that predates partitions then refuses it with its clear unsupported-version message instead of an unknown-field schema error. Replay reads versions 1 and 2 and requires the version to match the entries (2 exactly when some entry has a partition).

Replay validation (`replay::validate`) additionally requires, for each partition entry: 1 ≤ index ≤ count ≤ 64, no repeated `(name, index)`, one shared count per job name, and no mix of partitioned and unpartitioned entries for one name. Otherwise it refuses the manifest as tampered. Entry order is not checked, and a job may list fewer than `count` instances: replay sends what the listed entries name. Replay's report_id uniqueness check spans all entries of one job name: replay counts each id across the job's entries first and, like collection, refuses every copy of a repeated id (`invalid_snapshot`) rather than sending the first.

### D3. Envelope partition override

`JobContext` gains `partition: Option<Partition>`. When it is present, `collect.rs` validates the producer's suite_started as today (malformed producer fields still reject the report, keeping producer validation identical across job kinds). It then sets `partition` to `index.to_string()` and `partition_count` to `count` in both the envelope and the first (`suite_started`) record before `envelope` serializes the body. No other record field changes. Rewriting both is required: the server (`GoodFlows.TestReports.Consistency`) and `validate_envelope` (`identity_mismatch`) both reject an envelope whose values differ from the initial suite_started record. A mismatch with producer values is not an error, because Goodflows is authoritative for instances it scheduled. The server's partition grammar accepts decimal strings, and 1..=64 is within its count range, so no new grammar validation is needed.

`Head` gains `partition` and `partition_count`. For instance items, `Item::load` and replay expect the manifest entry's values, so a snapshot edited to another partition fails re-validation as `invalid_snapshot`. For unpartitioned entries the expectation is "whatever the snapshot carries", which preserves today's behavior.

### D4. Job-wide duplicate detection

Freezing stays streaming: each body is still frozen into its instance's `pending/` as it is built, which preserves the canonical "at most one report body in memory" rule. `collect_dir` returns its parsed identities (frozen or not) rather than resolving duplicates itself. After every directory of one job is collected, `collect_run` computes `report_id` multiplicities across that job's instance collections. Every report whose id appears more than once is discarded through its instance's sink (`Sink::discard` removes the snapshot) and counted as `duplicate_report_id` in that instance's collection, before the manifest is finalized. `Sink::discard` reports failure: a snapshot that cannot be removed (for example because its `pending/` was replaced by a symlink) is additionally counted as `snapshot_discard_failed`, so the leftover file is visible. It is still never listed in the manifest or sent.

Every rejected file counts under exactly one primary code, so a collection's rejection total is a file count. A duplicate whose snapshot was never written is already counted as `snapshot_write_failed`; it still makes the other copies duplicates but is not also counted as `duplicate_report_id`. `snapshot_discard_failed` is a secondary code: it annotates a file already counted as `duplicate_report_id`. The secondary codes are listed in one constant (`collect::SECONDARY_REJECTIONS`), and `collect::rejected_files` totals a collection's rejections without them. Replay's `invalid` total uses that function, so a leftover snapshot is not counted twice. The existing per-directory check becomes the single-instance case of this pass. No snapshot of a rejected report remains, and none is listed in the manifest or sent.

*Alternative:* hold every body, or parse every file twice, so that nothing is frozen before the pass. Rejected: the first breaks the one-body memory bound, and the second doubles the parse cost for the rare duplicate case.

### D4a. Per-job caps span instances

`MAX_REPORTS_PER_JOB` (256), `MAX_JOB_SOURCE_BYTES` (256 MiB) and `MAX_ENTRIES_PER_JOB` apply to the job as a whole, as the canonical spec states. `collect_dir` takes the remaining report, byte and entry allowance, and `collect_run` passes what is left to the instances in index order. A 64-partition job therefore cannot read 64 × 256 MiB. Consumption is in order, not proportional: when the instances together exceed a cap, the highest-numbered instances lose reports first (for example, two instances with 130 reports each collect 130 and 126). `collect_run` relies on `create_run` listing a job's entries consecutively in index order and checks that with a debug assertion.

### D5. Events and validation

`reports_collected` (from `finish_with`), and `report_uploaded`/`report_upload_pending` (from `deliver` and from `replay::upload_with`), add `partition` from the manifest entry, carried on `Item`. Text rendering of these events in `src/output.rs` reuses the `name (i/N)` helper from `add-job-partitions`. `validate_job` drops the `partitions` + `reports` rejection. The reporting job-name rules are unchanged.

## Risks / Trade-offs

- [A producer that ignores `GOODFLOWS_REPORT_DIR` and writes to a shared path] -> Collection stays directory-scoped, so those files are never seen. Unchanged from today and documented.
- [Override hides producer partition bugs] -> The producer's values are still grammar-validated. Override is limited to instances Goodflows scheduled.
- [Older CLI replaying a newer manifest] -> A manifest with partition entries is version 2, which an older CLI refuses with its unsupported-version message. Replay with the CLI that wrote the spool or a newer one, which is documented. Unpartitioned manifests stay version 1.
- [Rewriting `suite_started` alters producer bytes] -> Only `partition`/`partition_count` of the first record change, which the server requires to agree with the envelope. Suite and report identities, timings and other records are kept as supplied.
- [Job-wide caps starve later instances] -> Caps are already far above GoodCases' volume. When a cap is hit, the highest-numbered instances lose reports first and record `report_limit`, `job_byte_limit` or `entry_limit` rejections exactly as today. This is documented.

## Migration Plan

Implement after `add-job-partitions` (`upload-test-timings` is already implemented and archived). Release the CLI, then GoodCases can declare `reports` on its partitioned test job. Rollback: remove `reports` from partitioned jobs. Spools written by this version remain replayable by it.
