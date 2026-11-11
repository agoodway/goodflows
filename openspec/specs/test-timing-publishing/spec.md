# test-timing-publishing Specification

## Purpose

Publish test timing reports from Goodflows invocations to account-scoped storage while retaining local data and preserving check results.

## Requirements

### Requirement: Reporting is explicitly configured
The CLI SHALL support a top-level reporting block and per-job `reports = ["test-timings"]`. Configuration without reporting SHALL retain existing behavior. Unsupported or duplicate formats (including the retired `exunit-timings`), `reports` declared without a reporting block, inline tokens, a project not matching `[a-z0-9][a-z0-9_-]*` (1-64 bytes), a reporting job name longer than 64 UTF-8 bytes or containing control characters, `url_env`/`token_env` values that are not environment variable names, name a reserved goodflows variable (`GOODFLOWS_RUN_ID`, `GOODFLOWS_REPORT_DIR`, `GOODFLOWS_OUTPUT`, `GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT`, `GOODFLOWS_ENVIRONMENT`) or name the same variable, a job or step `env` entry for the configured `token_env`, a reporting `history` value that is not a boolean, and malformed reporting configuration MUST fail validation before jobs execute. The reporting block SHALL accept an optional boolean `history` (default `false`) that enables uploading each invocation's run record. Inspection commands (`list`, `validate`, `init`) MUST NOT create report directories or contact the server.

#### Scenario: Existing workflow
- **WHEN** a workflow has no reporting configuration
- **THEN** it runs without creating a reporting spool or making reporting requests

#### Scenario: Unsupported format
- **WHEN** a job selects an unsupported report format
- **THEN** validation identifies the configuration field and no jobs execute

#### Scenario: Retired format
- **WHEN** a job declares `reports = ["exunit-timings"]`
- **THEN** validation fails naming `test-timings` as the supported format and no jobs execute

#### Scenario: Duplicate format or missing reporting block
- **WHEN** a job lists `test-timings` twice, or declares `reports` while no reporting block exists
- **THEN** validation fails and no jobs execute

#### Scenario: Reporting job name too long
- **WHEN** a job declaring `reports` has a name longer than 64 UTF-8 bytes
- **THEN** validation names the job and no jobs execute

#### Scenario: Inline token
- **WHEN** the reporting block contains a token value rather than an environment variable name
- **THEN** validation fails and no jobs execute

#### Scenario: Reserved variable override
- **WHEN** any job or step `env` sets `GOODFLOWS_RUN_ID` or `GOODFLOWS_REPORT_DIR`
- **THEN** validation fails and no jobs execute

#### Scenario: Token variable set in job env
- **WHEN** a reporting block exists and a job or step `env` sets the configured `token_env`
- **THEN** validation fails naming the job, rather than the value being silently removed

#### Scenario: Credential variable names a partition variable
- **WHEN** the reporting block sets `url_env` or `token_env` to `GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT` or `GOODFLOWS_ENVIRONMENT`
- **THEN** validation fails and no jobs execute

#### Scenario: History upload setting
- **WHEN** the reporting block sets `history = "yes"`
- **THEN** validation fails naming `reporting.history` and no jobs execute

#### Scenario: History upload without report jobs
- **WHEN** the reporting block sets `history = true` and no job declares `reports`
- **THEN** validation succeeds and no job receives `GOODFLOWS_RUN_ID` or `GOODFLOWS_REPORT_DIR`

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

### Requirement: Publishing uses the shared timing contract
The publisher SHALL send one JSON request per report to `/api/v1/projects/<project>/test-reports`, where `<project>` is the configured project slug, using the `test-timings/1` wire format defined by the `test-timing-contract` capability. The body MUST NOT contain `project` or `account_id`. It MUST attach the lowercase workflow invocation UUID, workflow/environment, job result (one of `passed`, `failed`, `timed_out`, `cancelled`, `skipped` or `unknown`) and captured Git context, while retaining the producer's suite/report identities and microsecond units. `report_id` values MUST be unique within a job; duplicates SHALL be rejected locally rather than uploaded.

#### Scenario: Three partitions
- **WHEN** the test job produces three partition reports
- **THEN** three requests share the workflow invocation and job identity while retaining three distinct report identities

#### Scenario: Project selected by path
- **WHEN** a report is published for project `goodcases`
- **THEN** the request posts to `/api/v1/projects/goodcases/test-reports` and the JSON body has no `project` key

#### Scenario: Project does not exist on the account
- **WHEN** the server responds 404 for the project path
- **THEN** the report is not retried, remains pending with code `project_not_found` and the check exit status is unchanged

#### Scenario: Duplicate report identity in one job
- **WHEN** two collected reports in one job share a `report_id`
- **THEN** neither is uploaded, a `duplicate_report_id` code is emitted and both files are untouched

#### Scenario: Working directory is not a Git checkout
- **WHEN** Git context cannot be obtained
- **THEN** source fields are null and otherwise valid reports remain publishable

#### Scenario: Envelope declares the versioned format
- **WHEN** any report is frozen
- **THEN** its envelope `format` is `test-timings/1`

### Requirement: Completed and failed jobs both produce reports
The CLI SHALL collect reports from jobs that executed, regardless of their result. Reporting MUST NOT depend on a separate success-dependent job. Partial reports SHALL remain partial and MUST NOT receive synthesized suite completion events.

#### Scenario: Tests fail
- **WHEN** a test command exits nonzero after producing reports
- **THEN** reports are collected and publication is attempted while the original failed job result is preserved

#### Scenario: Job never starts
- **WHEN** dependency failure skips a job
- **THEN** it produces no fabricated timing report

#### Scenario: Executed job writes no reports
- **WHEN** a reporting job executes but its report directory is missing or contains no JSONL files
- **THEN** an explicit no-reports collection outcome is emitted, no request is sent and the job result is unchanged

### Requirement: Publishing is bounded and independent of check status
Automatic publishing SHALL be best-effort with a 30-second invocation-wide budget, a 3-second connect timeout and a per-request limit of 10 seconds plus one second per started MiB of request body, always clipped to the remaining budget. It SHALL attempt a report at most three times, retrying transport failures (other than TLS failures, reported as `tls_error`), 429 and 5xx responses within that budget. A delta-seconds `Retry-After` replaces the backoff; when it is not shorter than the remaining budget the report SHALL be left pending immediately; an HTTP-date `Retry-After` falls back to the normal backoff. Only 200/201 acknowledgements with a valid server id and matching report_id SHALL mark a report uploaded. Redirects and non-429 4xx responses MUST NOT be retried. 400, 409, 413, 415 and 422 are permanent rejections of the frozen bytes: the CLI SHALL record them locally as rejected by the server; 401, 403, 404 and other 4xx SHALL leave the report pending. Before any request each frozen snapshot SHALL be re-read without following symlinks, bounded to 32 MiB and re-validated against the contract and the run's identity. Publication MUST NOT alter check results or their exit status; reporting output write failures MUST NOT change the exit status or prevent the manifest from being finalized; cancellation MUST stop network work promptly.

#### Scenario: Offline successful run
- **WHEN** tests pass but the server cannot be reached
- **THEN** the command retains the success exit status, reports delivery as pending and preserves replayable local data

#### Scenario: Cancellation
- **WHEN** a signal cancels the workflow or publishing
- **THEN** the CLI preserves its signal exit code without waiting for the publishing budget to expire

#### Scenario: Immutable conflict
- **WHEN** the server responds 409 for a report
- **THEN** the report is not retried, is not marked uploaded, and is recorded as rejected by the server (`acks/NNN.rejected.json`) so replay does not resend it

#### Scenario: Retry-After beyond the budget
- **WHEN** a 429 or 5xx response carries a delta-seconds `Retry-After` at least as long as the remaining budget
- **THEN** the report is left pending without waiting

#### Scenario: Signal during automatic publishing
- **WHEN** a signal arrives while a publish request is in flight
- **THEN** the request is abandoned, the report is pending with reason `cancelled`, no acknowledgement is recorded, the summary is still the last event and the signal exit code is returned

#### Scenario: Mismatched acknowledgement
- **WHEN** a 200 or 201 response carries a different report_id
- **THEN** the report remains pending

#### Scenario: Spool cannot be created
- **WHEN** the configured spool directory cannot be created
- **THEN** checks run without reporting environment injection, `reporting_skipped` is emitted with reason `spool_unavailable` and the check exit status is unchanged

### Requirement: Credentials remain owned by the publisher
The endpoint and Bearer key SHALL be resolved through configured environment variable names. Keys MUST NOT appear in TOML, manifests, HTTP diagnostics, child-process environments (including variables inherited from the CLI process), expression `env` lookups, job outputs or emitted events. Publishing MUST allow HTTPS (trusting both the bundled WebPKI roots and the operating system's roots) and loopback-only HTTP, disable redirects and retain local reports when credentials are absent or invalid.

Threat model: these rules keep the key away from jobs, logs and saved state. They do not isolate the CLI from other processes running as the same OS user, which can generally read its memory or its inherited environment, nor from privileged processes. As a mitigation, on Linux the CLI marks itself non-dumpable (`PR_SET_DUMPABLE` 0) once it has read a key, so same-user processes, including jobs, cannot read `/proc/<pid>/environ` or ptrace it; `execve` resets the flag, so jobs themselves are unaffected. Other platforms get no such protection, and the shell or CI agent that exported the key remains readable as before. A replayed manifest selects which environment variables become the endpoint and the key, so only run directories produced by trusted invocations should be replayed.

#### Scenario: Missing key
- **WHEN** a reporting job runs without a configured API key
- **THEN** reports remain local, publication status is explicit and test execution continues

#### Scenario: Token inherited from the CLI environment
- **WHEN** the reporting token variable is exported in the shell that launches the CLI
- **THEN** no job, step or condition command receives it and `${{ env.<token_env> }}` resolves to null

#### Scenario: Invalid endpoint origin
- **WHEN** the endpoint variable contains a non-loopback HTTP URL, a path, userinfo, a query or a fragment
- **THEN** no request is sent, `reporting_skipped` is emitted with reason `invalid_url` and the check exit status is unchanged

#### Scenario: Redirect response
- **WHEN** the server redirects a publish request
- **THEN** the CLI does not forward credentials and leaves the report pending with a sanitized diagnostic

### Requirement: Saved invocations can be retried without tests
`goodflows reports upload <run-directory>` SHALL replay immutable saved request snapshots with their original identities and metadata, without running jobs or reading current Git state. It MUST validate the manifest before building any request: the manifest version, a lowercase canonical `workflow_run_id`, the project slug rule, the configuration rules for `url_env`/`token_env`, the workflow, environment and job name rules, and the consistency of partition entries. A manifest SHALL be written as version 3 when it lists a run record, otherwise as version 1 when no job entry has a partition and as version 2 when at least one does; replay MUST accept versions 1, 2 and 3 and MUST refuse a manifest whose version does not match its entries and run record. Partition entries MUST have an index from 1 to their count and a count from 1 to 64, MUST NOT repeat an index within one job, MUST share one count per job, and MUST NOT share a job name with an unpartitioned entry. Entry order is not significant, and a job MAY list fewer entries than its count: replay sends the reports of the entries present. Snapshot, acknowledgement and rejection locations SHALL be derived from the job name's encoded directory, the entry's `p<index>` instance directory when it has a partition, and a report index, never read as paths from the manifest, and symlinked components SHALL be refused. Exact server acknowledgements SHALL mark reports uploaded atomically; transport failures, timeouts and other unacknowledged or ambiguous responses MUST remain replayable. Reports recorded as rejected by the server (400, 409, 413, 415, 422) SHALL NOT be resent and SHALL be reported with reason `rejected_by_server`; deleting the rejection marker makes such a report replayable again. An optional `--budget <seconds>` (1-3600) SHALL replace the 30-second publishing budget. A listed run record SHALL be replayed after the reports, as specified by "Run records are replayed with their reports". A replay command SHALL return nonzero if reports or the run record remain unsent, rejected by the server or invalid, where invalid includes each file rejected during the original collection once, excluding secondary codes such as `snapshot_discard_failed` that annotate a file already counted under another code.

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

### Requirement: Report processing is bounded and structural
Collection SHALL enforce the 32 MiB request limit, 100,000-record report limit and the complete type rules of the `test-timing-contract` capability, taken from one versioned local definition that the server mirrors. A source file larger than 33 MiB (the request limit plus a 1 MiB allowance) SHALL be rejected without parsing, and parsing SHALL stop once the records alone exceed the request limit. Each valid request SHALL be frozen as it is built, so at most one report body is held in memory. It SHALL cap collection at 256 reports and 256 MiB of source data per job. A malformed final JSONL fragment SHALL be discarded and the envelope SHALL set `truncated: true` (the server derives incomplete status); earlier malformed records MUST reject that report. Original files MUST remain intact.

#### Scenario: Worker is killed during its final write
- **WHEN** valid timing records end with one truncated JSON fragment
- **THEN** the valid prefix is retained with `truncated: true` without inventing a finish event

#### Scenario: Malformed earlier record
- **WHEN** a record before the final line is not valid JSON
- **THEN** the report is rejected with a structural code, nothing is uploaded for it and the file is untouched

#### Scenario: Report exceeds limits
- **WHEN** a report would exceed 32 MiB encoded or 100,000 records
- **THEN** it is not uploaded, the file is retained and a structural code is emitted

#### Scenario: Type violation is caught locally
- **WHEN** a `test_finished` record has `duration_us: "12"`
- **THEN** the report is rejected with `invalid_field` before a snapshot is frozen, rather than being frozen and rejected by the server

### Requirement: Reporting output preserves the existing output protocol
Reporting SHALL emit structural collection/delivery status in both text and versioned JSON event modes. Existing job durations, statuses, dependency scheduling and summary semantics MUST remain unchanged. Raw timing records, response bodies, source contents and secrets MUST NOT be printed as diagnostics. Reporting events are best-effort: failing to write one MUST NOT change the check exit status, and the manifest SHALL be finalized before collection events are emitted.

#### Scenario: JSON output with upload failure
- **WHEN** the CLI runs with `--json` and publication fails
- **THEN** every stdout line remains a valid versioned event and the existing workflow summary still describes check results

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

### Requirement: Contract violations map to stable structural codes
Collection SHALL reject a report that breaks the contract with one stable code: `unsupported_format`, `unsupported_event`, `unsupported_field`, `missing_field`, `invalid_field`, `field_too_long`, `invalid_order`, `identity_mismatch`, `invalid_identifier`, `invalid_partition`, `invalid_partition_count`, `unsupported_filter`, `invalid_meta`, `unpaired_event`, `unfinished_mismatch`, `duplicate_key`, `nul_character`, `non_integer_number`, `integer_out_of_range`, `nested_too_deeply`, `malformed_record`, `invalid_utf8`, `empty_report` or `missing_suite_started`. A code that collection already returns for one of these failures SHALL keep that name. Size and count limits SHALL keep `too_large`, `record_limit` and `request_too_large`, which are outside this set. The code SHALL NOT include submitted values. A non-null `filters` that is not an object, lacks an `include` or `exclude` array, has a list over 256 entries, or holds an entry that is not an object or whose `tag` or `value` breaks the filter grammar SHALL be `unsupported_filter`, not `invalid_field`. An unknown key inside `filters` or inside a filter entry SHALL be `unsupported_field`, as for unknown keys in `counts` and `runner`, preserving the code collection already returns. An absent core key SHALL be `missing_field`. A second started event with an `execution_id` already in use for that kind SHALL be `unpaired_event`. Collection SHALL validate each record as its line is decoded, so a report with several faults SHALL get the code of the first faulty line in file order (and, within a record, of the first faulty key in the contract's table order), and the lines after it SHALL NOT be decoded.

#### Scenario: Several faults
- **WHEN** line 2 of a report has a 5,000-byte `name` and line 3 has a duplicate key
- **THEN** the report is rejected with `field_too_long`, the code of line 2

#### Scenario: Structured meta value
- **WHEN** a record's `meta` holds an array value
- **THEN** the report is rejected with `invalid_meta` and the raw file is untouched

#### Scenario: Finish without start
- **WHEN** a `group_finished` has no matching `group_started`
- **THEN** the report is rejected with `unpaired_event`

#### Scenario: Unfinished count disagrees
- **WHEN** `suite_finished.unfinished_tests` differs from the number of unfinished tests in the report
- **THEN** the report is rejected with `unfinished_mismatch`

#### Scenario: Retired format in a new run
- **WHEN** a producer writes a report whose `suite_started.format` is `exunit-timings`
- **THEN** the report is rejected with `unsupported_format` and no snapshot is written

#### Scenario: Missing core key
- **WHEN** a `test_finished` record omits `duration_us`
- **THEN** the report is rejected with `missing_field` and the raw file is untouched

#### Scenario: Duplicate start
- **WHEN** two `test_started` records share one `execution_id`
- **THEN** the report is rejected with `unpaired_event`

#### Scenario: Filter shape
- **WHEN** a filter entry's value is not a boolean, its tag is outside the identifier grammar, or either list has 257 entries
- **THEN** the report is rejected with `unsupported_filter`, no snapshot is written and the raw file is untouched

#### Scenario: Unknown key in filters
- **WHEN** `filters` is `{"include": [], "exclude": [], "x": 1}`, or a filter entry is `{"tag": "slow", "value": true, "note": "x"}`
- **THEN** the report is rejected with `unsupported_field`, not `unsupported_filter`, and no snapshot is written

### Requirement: Snapshots in retired formats are not replayed
`goodflows reports upload` SHALL re-validate every saved snapshot against the current contract. A snapshot frozen in a retired format, such as `exunit-timings`, MUST NOT be sent: it SHALL be counted as `invalid_snapshot` and the command SHALL exit nonzero. Its file MUST remain intact.

#### Scenario: Replaying a pre-upgrade run directory
- **WHEN** a finalized manifest lists a pending `exunit-timings` snapshot
- **THEN** replay sends nothing for it, reports `invalid_snapshot`, leaves the file in place and exits nonzero

### Requirement: Run records are frozen only when they can be sent
A run record SHALL be frozen only when upload is enabled and, at invocation start, the reporting key and URL variables are set and the URL is a valid endpoint origin. Otherwise the invocation SHALL emit `run_upload_skipped` with reason `credentials_unavailable` or `invalid_url`, and SHALL build and keep nothing for that record. A remote Sprite invocation is the exception described in "Remote run records wait for the controller". Check results and the exit code MUST NOT change.

#### Scenario: Laptop without a key
- **WHEN** `[reporting] history = true` and the key variable is unset
- **THEN** `run_upload_skipped` is emitted with reason `credentials_unavailable`, no spool directory is created for the record, and the exit code is unchanged

#### Scenario: Invalid endpoint
- **WHEN** upload is enabled and the URL variable holds a non-loopback `http` URL
- **THEN** `run_upload_skipped` is emitted with reason `invalid_url` and no request is sent

### Requirement: Remote run records wait for the controller
When upload is enabled, a remote Sprite invocation SHALL freeze its run record without credentials and SHALL NOT send it. It SHALL emit `run_upload_pending` with reason `credentials_unavailable`, so that the controller can publish the record after collection.

#### Scenario: Remote Sprite invocation
- **WHEN** `[reporting] history = true` and an invocation runs on a Sprite, where the key variable is never available
- **THEN** `run-record.json` is frozen in that invocation's remote spool, no request is sent, and `run_upload_pending` is emitted with reason `credentials_unavailable`

### Requirement: Run records are frozen next to the invocation's reports
A valid run record SHALL be written once, as the exact bytes to send, to `run-record.json` in the invocation's spool run directory. The file SHALL be created new with mode 0600, and symlinks SHALL be refused. The directory SHALL be created even when no job declares `reports`. The finalized manifest SHALL then list the run record. If the spool cannot be written, `run_upload_skipped` SHALL be emitted with reason `spool_unavailable` and nothing SHALL be sent.

#### Scenario: No report jobs
- **WHEN** upload is enabled, credentials are ready, and no job declares `reports`
- **THEN** the spool holds `<run_id>/manifest.json`, version 3 with no job entries, and `<run_id>/run-record.json`

#### Scenario: Job named like the record
- **WHEN** a reporting job is named `run-record.json`
- **THEN** its directory and the run record file do not collide

#### Scenario: Spool unavailable
- **WHEN** the spool directory cannot be created
- **THEN** `run_upload_skipped` is emitted with reason `spool_unavailable`, nothing is sent, and the exit code is unchanged

### Requirement: Run records are sent after the outcome is fixed
A run record SHALL be frozen and sent after the invocation's reports are published and its outcome snapshot is taken, and before its `summary`. It SHALL use the remaining part of the same 30-second invocation-wide budget, connect timeout, per-request limit, three-attempt limit, and `Retry-After` handling as reports. A signal during the upload SHALL leave the record pending with reason `cancelled` and MUST NOT change the outcome.

#### Scenario: Budget spent by reports
- **WHEN** report uploads use the whole budget
- **THEN** the run record is not sent, `run_upload_pending` has reason `budget_exhausted`, and the snapshot stays replayable

#### Scenario: Signal during the run upload
- **WHEN** a signal arrives while the run record request is in flight
- **THEN** the request is abandoned, the record is pending with reason `cancelled`, the summary is still the last event, and the history outcome is the one snapshotted before the upload

#### Scenario: Summary duration covers the upload
- **WHEN** the run record upload takes 2 seconds
- **THEN** `summary.duration_ms` covers it and `summary.execute_ms` does not

### Requirement: Run record responses have their own outcomes
A 200 or 201 response SHALL mark a run record uploaded only if its `data` has a UUID `id`, a `run_id` equal to the record's, and a boolean `duplicate`. 408, 429, 5xx, timeouts and transport errors other than TLS SHALL be retried. 409 SHALL be recorded as a terminal conflict, and 400 and 415 as terminal rejections. Neither SHALL be resent. 413, 422, TLS failures, redirects and other 4xx SHALL leave the record pending.

#### Scenario: Stored
- **WHEN** the server responds 201 with a matching acknowledgement
- **THEN** `run-record.ack.json` is written and `run_uploaded` is emitted with the server id and `duplicate` false

#### Scenario: Exact retry already stored
- **WHEN** the server responds 200 with `duplicate` true
- **THEN** the record is marked uploaded and `run_uploaded` has `duplicate` true

#### Scenario: Conflict
- **WHEN** the server responds 409
- **THEN** the record is not retried, `run-record.rejected.json` is written, and `run_upload_pending` has reason `conflict`, HTTP status 409, and `terminal` true

#### Scenario: Server does not yet accept the record
- **WHEN** the server responds 422
- **THEN** the record is not retried in this attempt, no rejection marker is written, and `run_upload_pending` has reason `rejected` and HTTP status 422 without `terminal`

#### Scenario: Auth, project, and size failures leave the run's exit code
- **WHEN** the server responds 401, 403, 404, or 413 during the run upload
- **THEN** text output names `unauthorized (HTTP 401)`, `forbidden (HTTP 403)`, `project not found (HTTP 404)`, or `request too large (HTTP 413)`, the record stays pending, and the run's exit code is unchanged

#### Scenario: Request timeout
- **WHEN** the server responds 408 and then 201 within the budget
- **THEN** the record is marked uploaded

#### Scenario: Mismatched acknowledgement
- **WHEN** a 201 response carries a different `run_id`
- **THEN** the record remains pending with reason `mismatched_acknowledgement`

### Requirement: Run records are replayed with their reports
When a manifest lists a run record, `reports upload` SHALL send it after the reports, within the same budget, with its original bytes and `run_id`. It SHALL first re-read and re-validate the record and check that its `run_id`, `workflow` and `environment` match the manifest. An acknowledged record SHALL NOT be resent, and a rejected one SHALL be reported with reason `rejected_by_server`.

#### Scenario: Replaying a pending record
- **WHEN** a version 3 manifest lists a run record that has no acknowledgement or rejection marker
- **THEN** replay sends `run-record.json` unchanged and, on a 200 or 201 acknowledgement, writes `run-record.ack.json`

#### Scenario: Acknowledged record
- **WHEN** `run-record.ack.json` names the record's `run_id`
- **THEN** no request is sent for it and it counts as acknowledged

#### Scenario: Tampered record
- **WHEN** `run-record.json` fails validation or its `run_id` differs from the manifest's `workflow_run_id`
- **THEN** it is not sent, counts as invalid, and replay exits nonzero

#### Scenario: Missing record file
- **WHEN** the manifest lists a run record and `run-record.json` is missing or is a symlink
- **THEN** nothing is sent for it, it counts as invalid, and replay exits nonzero

#### Scenario: Replay summary
- **WHEN** replay finishes for a version 3 manifest
- **THEN** `reports_upload_summary` has `run_record` set to `acknowledged`, `pending`, `rejected` or `invalid`, and to null for versions 1 and 2

### Requirement: Run upload events are structural
Run uploads SHALL emit `run_uploaded` (`run_id`, `server_id`, `duplicate`), `run_upload_pending` (`run_id`, `reason`, `http_status`, and `terminal` true for terminal rejections), and `run_upload_skipped` (`run_id`, `reason`, and `path` for `invalid_record`) in JSON mode. Text mode SHALL print one line for each. Response bodies, record contents, URLs and keys MUST NOT appear in them, and failing to write an event MUST NOT change the exit code.

#### Scenario: Text pending line
- **WHEN** a run record is left pending after HTTP 503 in text mode
- **THEN** one line reports the run history upload as pending with `service unavailable (HTTP 503)`

#### Scenario: JSON event shape
- **WHEN** a run record is uploaded in `--json` mode
- **THEN** a `run_uploaded` event with `schema_version`, `run_id`, `server_id` and `duplicate` precedes that invocation's `summary`
