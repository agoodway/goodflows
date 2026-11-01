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

#### Scenario: Unpartitioned layout unchanged
- **WHEN** a reporting job without `partitions` runs
- **THEN** its report directory and manifest entry have the same layout as before partitions existed

## ADDED Requirements

### Requirement: Goodflows supplies partition metadata for instances
For reports collected from an instance, the envelope `partition` SHALL be the instance index as a decimal string and `partition_count` SHALL be the instance count, replacing producer-supplied values. `job_status` SHALL be that instance's own status. Reports from unpartitioned jobs SHALL keep the producer's partition values.

#### Scenario: Producer omits partition
- **WHEN** instance 2 of 3 produces a report whose suite_started has null partition fields
- **THEN** the envelope has `partition: "2"` and `partition_count: 3`

#### Scenario: Producer disagrees
- **WHEN** instance 2 of 3 produces a report claiming partition `"1"` of 4
- **THEN** the envelope has `partition: "2"` and `partition_count: 3`

#### Scenario: One instance fails
- **WHEN** instance 1 passes and instance 2 fails, and both produce reports
- **THEN** instance 1's envelope has `job_status: "passed"` and instance 2's has `job_status: "failed"`

### Requirement: Report identities are unique across a job's instances
`report_id` uniqueness SHALL be enforced across all instances of a job. When reports from any instances of one job share a `report_id`, all of them MUST be rejected with `duplicate_report_id` and left untouched.

#### Scenario: Duplicate across instances
- **WHEN** instances 1 and 3 of a job each produce a report with the same `report_id`
- **THEN** neither is uploaded, `duplicate_report_id` is emitted and both files remain

### Requirement: Reporting events and replay identify instances
`reports_collected`, `report_uploaded` and `report_upload_pending` events for instance reports SHALL include `partition: {"index": i, "count": N}`, and text output SHALL show `job (i/N)`. Saved manifests SHALL record each instance's partition. Replay SHALL accept manifests with and without partition entries, without changing stored metadata.

#### Scenario: JSON collection event
- **WHEN** instance 2 of 3 is collected with `--json`
- **THEN** its `reports_collected` event includes `"job": "test"` and `"partition": {"index": 2, "count": 3}`

#### Scenario: Replaying an older manifest
- **WHEN** `goodflows reports upload` reads a finalized manifest written before partitions existed
- **THEN** it replays the saved snapshots unchanged
