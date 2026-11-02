# Spec Delta

## MODIFIED Requirements

### Requirement: Collection belongs to one invocation and job
Each reporting invocation SHALL have a unique workflow run identity and persistent report directories, one per unpartitioned reporting job and one per instance of a partitioned reporting job. The CLI MUST expose `GOODFLOWS_RUN_ID` and the matching `GOODFLOWS_REPORT_DIR` to report-producing commands, prevent configured overrides of these values, and collect only regular JSONL files in each directory, once, after child processes finish. Whenever a reporting block exists, values of these names inherited from the CLI's own environment MUST NOT reach any command or expression. Every report directory and its `pending/` and `acks/` state directories SHALL be created private (mode 0700) before any job or instance starts, and snapshot or acknowledgement writes MUST refuse a state directory replaced by a symlink.

#### Scenario: Historical files and simultaneous invocations
- **WHEN** historical reports exist and two invocations run concurrently
- **THEN** each collects only files produced in its own job directories

#### Scenario: File escapes the collection directory
- **WHEN** a report path is a symlink or resolves outside its job directory
- **THEN** collection rejects it without reading or uploading its contents

#### Scenario: Nested invocation
- **WHEN** goodflows runs inside a job of another goodflows invocation that exported `GOODFLOWS_RUN_ID` and `GOODFLOWS_REPORT_DIR`
- **THEN** no job or instance of the inner invocation receives the inherited values, and only its own report-producing jobs and instances receive its own values

#### Scenario: State directory replaced by a symlink
- **WHEN** a job or partition instance replaces its `pending/` or `acks/` directory with a symlink or a file
- **THEN** no snapshot or acknowledgement is written through it and the report is counted as `snapshot_write_failed` or left pending with `state_write_failed`

#### Scenario: Partitioned reporting job
- **WHEN** a reporting job with `partitions = 3` runs
- **THEN** each instance receives a distinct `GOODFLOWS_REPORT_DIR`, all three share one `GOODFLOWS_RUN_ID`, and each directory is collected exactly once

#### Scenario: Instance state directories
- **WHEN** two instances of a partitioned reporting job each produce one report
- **THEN** each report is frozen in its own instance's `pending/` directory, and neither snapshot overwrites the other

#### Scenario: Per-job caps span instances
- **WHEN** the instances of one partitioned job together hold more than 256 reports
- **THEN** at most 256 reports are collected for the job, consumed in partition order so the highest-numbered instances lose reports first, and the rest are rejected with a structural code

#### Scenario: Cap reached in a later instance
- **WHEN** instances 1 and 2 of a job each produce 130 valid reports
- **THEN** instance 1 collects 130, instance 2 collects 126 and counts `report_limit: 4`, and the manifest lists 256 reports

#### Scenario: Steps of an instance
- **WHEN** a partitioned reporting job has two steps, each with a condition
- **THEN** both step commands of instance `i` receive that instance's `p<i>/reports` directory, and neither step condition sees `GOODFLOWS_REPORT_DIR`

#### Scenario: Unpartitioned layout unchanged
- **WHEN** a reporting job without `partitions` runs
- **THEN** its report directory and manifest entry have the same layout as before partitions existed

### Requirement: Saved invocations can be retried without tests
`goodflows reports upload <run-directory>` SHALL replay immutable saved request snapshots with their original identities and metadata, without running jobs or reading current Git state. It MUST validate the manifest before building any request: the manifest version, a lowercase canonical `workflow_run_id`, the project slug rule, the configuration rules for `url_env`/`token_env`, the workflow, environment and job name rules, and the consistency of partition entries. A manifest SHALL be written as version 1 when no job entry has a partition and as version 2 when at least one does; replay MUST accept versions 1 and 2 and MUST refuse a manifest whose version does not match its entries. Partition entries MUST have an index from 1 to their count and a count from 1 to 64, MUST NOT repeat an index within one job, MUST share one count per job, and MUST NOT share a job name with an unpartitioned entry. Entry order is not significant, and a job MAY list fewer entries than its count: replay sends the reports of the entries present. Snapshot, acknowledgement and rejection locations SHALL be derived from the job name's encoded directory, the entry's `p<index>` instance directory when it has a partition, and a report index, never read as paths from the manifest, and symlinked components SHALL be refused. Exact server acknowledgements SHALL mark reports uploaded atomically; transport failures, timeouts and other unacknowledged or ambiguous responses MUST remain replayable. Reports recorded as rejected by the server (400, 409, 413, 415, 422) SHALL NOT be resent and SHALL be reported with reason `rejected_by_server`; deleting the rejection marker makes such a report replayable again. An optional `--budget <seconds>` (1-3600) SHALL replace the 30-second publishing budget. A replay command SHALL return nonzero if reports remain unsent, rejected by the server or invalid, where invalid includes each file rejected during the original collection once, excluding secondary codes such as `snapshot_discard_failed` that annotate a file already counted under another code.

#### Scenario: Server committed but acknowledgement was lost
- **WHEN** the user retries the saved invocation
- **THEN** the same identity and payload are sent and the server's idempotent acknowledgement completes local delivery

#### Scenario: Reports remain pending
- **WHEN** a replay finishes with any report unacknowledged or invalid
- **THEN** the command exits nonzero

#### Scenario: Everything already acknowledged
- **WHEN** every report frozen in the saved invocation is already acknowledged and its collection recorded no rejected files
- **THEN** no requests are sent and the command exits 0

#### Scenario: Collection rejected some files
- **WHEN** the original collection rejected a file (for example a malformed report) and every frozen report is acknowledged
- **THEN** no requests are sent for acknowledged reports, the rejected files are counted as invalid and the command exits nonzero

#### Scenario: Leftover duplicate snapshot
- **WHEN** a manifest entry records `duplicate_report_id: 1` and `snapshot_discard_failed: 1` and its `pending/` still holds the undiscarded snapshot
- **THEN** replay counts one invalid report, never sends the leftover snapshot and exits nonzero

#### Scenario: Server rejected a report
- **WHEN** a report was recorded as rejected by the server during an earlier attempt
- **THEN** replay sends nothing for it, reports `rejected_by_server` and exits nonzero

#### Scenario: Tampered run directory
- **WHEN** the manifest has an unsupported version, a version that does not match its partition entries, an invalid identity, project or environment variable name, inconsistent partition entries, a job directory that does not match its job name, or a snapshot that no longer passes contract validation
- **THEN** replay sends nothing for the affected reports (nothing at all for manifest-level violations) and exits nonzero

#### Scenario: Replaying a partitioned run
- **WHEN** a finalized version 2 manifest lists instances 1 to 3 of job `test`
- **THEN** each instance's snapshots are read from `test/p<index>/pending/`, re-validated against that instance's partition, and acknowledged in `test/p<index>/acks/`

#### Scenario: Reordered or incomplete instance entries
- **WHEN** a manifest lists a job's instances in the order 3, 1, 2, or lists only instances 1 and 3 of 3
- **THEN** replay accepts it and sends the reports of the listed entries

#### Scenario: Manifest not finalized
- **WHEN** the run directory's manifest was never finalized
- **THEN** replay refuses to build requests from it and leaves the raw files in place

#### Scenario: Signal during replay
- **WHEN** a signal interrupts the replay
- **THEN** no new request starts and the signal exit code takes precedence

## ADDED Requirements

### Requirement: Goodflows supplies partition metadata for instances
For reports collected from an instance, `partition` SHALL be the instance index as a decimal string and `partition_count` SHALL be the instance count, in both the envelope and the frozen initial `suite_started` record, replacing producer-supplied values. No other record field SHALL change. Malformed producer partition fields MUST still reject the report. Snapshot re-validation before sending and during replay MUST reject an instance snapshot whose partition differs from its manifest entry. `job_status` SHALL be that instance's own status. Reports from unpartitioned jobs SHALL keep the producer's partition values.

#### Scenario: Producer omits partition
- **WHEN** instance 2 of 3 produces a report whose suite_started has null partition fields
- **THEN** the envelope and the frozen `suite_started` record both have `partition: "2"` and `partition_count: 3`

#### Scenario: Producer disagrees
- **WHEN** instance 2 of 3 produces a report claiming partition `"1"` of 4
- **THEN** the envelope and the frozen `suite_started` record both have `partition: "2"` and `partition_count: 3`, so the server's envelope/suite_started agreement check passes

#### Scenario: Malformed producer partition
- **WHEN** an instance produces a report whose suite_started `partition` is `"a b"`
- **THEN** the report is rejected with a structural code and no snapshot remains

#### Scenario: One instance fails
- **WHEN** instance 1 passes and instance 2 fails, and both produce reports
- **THEN** instance 1's envelope has `job_status: "passed"` and instance 2's has `job_status: "failed"`

### Requirement: Report identities are unique across a job's instances
`report_id` uniqueness SHALL be enforced across all instances of a job. When reports from any instances of one job share a `report_id`, all of them MUST be rejected with `duplicate_report_id`, their source files MUST be left untouched, and no snapshot of them SHALL remain or be listed in the manifest. If an already-frozen snapshot of such a report cannot be removed, collection MUST additionally count it as `snapshot_discard_failed` and MUST NOT list or send it. Each rejected file SHALL be counted under exactly one primary code: a duplicate whose snapshot could not be written SHALL be counted only as `snapshot_write_failed`, while still rejecting the other copies as duplicates. Replay MUST likewise refuse every copy of a `report_id` listed more than once across a job's manifest entries.

#### Scenario: Duplicate across instances
- **WHEN** instances 1 and 3 of a job each produce a report with the same `report_id`
- **THEN** neither is uploaded, `duplicate_report_id` is emitted, both source files remain and neither instance's `pending/` keeps a snapshot of them

#### Scenario: Duplicate snapshot cannot be removed
- **WHEN** a duplicate's frozen snapshot cannot be discarded
- **THEN** that instance's `reports_collected.rejected` counts both `duplicate_report_id` and `snapshot_discard_failed`, and the snapshot is neither listed in the manifest nor uploaded

#### Scenario: Duplicate snapshot could not be written
- **WHEN** one copy of a duplicated `report_id` fails to freeze and another copy was frozen
- **THEN** the first is counted only as `snapshot_write_failed`, the second as `duplicate_report_id`, and neither is listed or uploaded

#### Scenario: Repeated report_id in a saved manifest
- **WHEN** two manifest entries of one job list the same `report_id`
- **THEN** replay sends neither copy and reports each as `invalid_snapshot`

### Requirement: Reporting events and replay identify instances
`reports_collected`, `report_uploaded` and `report_upload_pending` events for instance reports SHALL include `partition: {"index": i, "count": N}`, and text output SHALL show `job (i/N)`. Saved manifests SHALL record each instance's partition. Replay SHALL accept manifests with and without partition entries, without changing stored metadata, and SHALL refuse inconsistent partition entries as described for saved invocations.

#### Scenario: JSON collection event
- **WHEN** instance 2 of 3 is collected with `--json`
- **THEN** its `reports_collected` event includes `"job": "test"` and `"partition": {"index": 2, "count": 3}`

#### Scenario: Replaying an older manifest
- **WHEN** `goodflows reports upload` reads a finalized manifest written before partitions existed
- **THEN** it replays the saved snapshots unchanged

#### Scenario: Tampered partition entries
- **WHEN** a finalized manifest lists two entries for job `test` with the same partition index
- **THEN** replay sends nothing and exits nonzero
