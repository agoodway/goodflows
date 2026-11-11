# Spec Delta

## MODIFIED Requirements

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

## ADDED Requirements

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
