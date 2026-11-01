# test-timing-publishing Specification

## Purpose

Publish test timing reports from Goodflows invocations to account-scoped storage while retaining local data and preserving check results.

## Requirements

### Requirement: Reporting is explicitly configured
The CLI SHALL support a top-level reporting block and per-job `reports = ["exunit-timings"]`. Configuration without reporting SHALL retain existing behavior. Unsupported or duplicate formats, `reports` declared without a reporting block, inline tokens, a project not matching `[a-z0-9][a-z0-9_-]*` (1-64 bytes), a reporting job name longer than 64 UTF-8 bytes or containing control characters, `url_env`/`token_env` values that are not environment variable names, name a reserved goodflows variable (`GOODFLOWS_RUN_ID`, `GOODFLOWS_REPORT_DIR`, `GOODFLOWS_OUTPUT`) or name the same variable, a job or step `env` entry for the configured `token_env`, and malformed reporting configuration MUST fail validation before jobs execute. Inspection commands MUST NOT create report directories or contact the server.

#### Scenario: Existing workflow
- **WHEN** a workflow has no reporting configuration
- **THEN** it runs without creating a reporting spool or making reporting requests

#### Scenario: Unsupported format
- **WHEN** a job selects an unsupported report format
- **THEN** validation identifies the configuration field and no jobs execute

#### Scenario: Duplicate format or missing reporting block
- **WHEN** a job lists `exunit-timings` twice, or declares `reports` while no reporting block exists
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

### Requirement: Collection belongs to one invocation and job
Each reporting invocation SHALL have a unique workflow run identity and persistent job-specific report directories. The CLI MUST expose `GOODFLOWS_RUN_ID` and `GOODFLOWS_REPORT_DIR` to report-producing commands, prevent configured overrides of these values, and collect only regular JSONL files in that job directory after child processes finish. Whenever a reporting block exists, values of these names inherited from the CLI's own environment MUST NOT reach any command or expression. The job's `reports/`, `pending/` and `acks/` directories SHALL be created private (mode 0700) before any job starts, and snapshot or acknowledgement writes MUST refuse a state directory replaced by a symlink.

#### Scenario: Historical files and simultaneous invocations
- **WHEN** historical reports exist and two invocations run concurrently
- **THEN** each collects only files produced in its own job directories

#### Scenario: File escapes the collection directory
- **WHEN** a report path is a symlink or resolves outside its job directory
- **THEN** collection rejects it without reading or uploading its contents

#### Scenario: Nested invocation
- **WHEN** goodflows runs inside a job of another goodflows invocation that exported `GOODFLOWS_RUN_ID` and `GOODFLOWS_REPORT_DIR`
- **THEN** no job of the inner invocation receives the inherited values, and only its own report-producing jobs receive its own values

#### Scenario: State directory replaced by a symlink
- **WHEN** a job replaces its `pending/` or `acks/` directory with a symlink or a file
- **THEN** no snapshot or acknowledgement is written through it and the report is counted as `snapshot_write_failed` or left pending with `state_write_failed`

### Requirement: Publishing uses the shared timing contract
The publisher SHALL send one JSON request per report to `/api/v1/projects/<project>/test-reports`, where `<project>` is the configured project slug, using the server companion change's wire contract. The body MUST NOT contain `project` or `account_id`. It MUST attach the lowercase workflow invocation UUID, workflow/environment, job result (one of `passed`, `failed`, `timed_out`, `cancelled`, `skipped` or `unknown`) and captured Git context, while retaining the producer's suite/report identities and microsecond units. `report_id` values MUST be unique within a job; duplicates SHALL be rejected locally rather than uploaded.

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
`goodflows reports upload <run-directory>` SHALL replay immutable saved request snapshots with their original identities and metadata, without running jobs or reading current Git state. It MUST validate the manifest before building any request: the manifest version, a lowercase canonical `workflow_run_id`, the project slug rule, the configuration rules for `url_env`/`token_env`, and the workflow, environment and job name rules. Snapshot, acknowledgement and rejection locations SHALL be derived from the job name's encoded directory and a report index, never read as paths from the manifest, and symlinked components SHALL be refused. Exact server acknowledgements SHALL mark reports uploaded atomically; transport failures, timeouts and other unacknowledged or ambiguous responses MUST remain replayable. Reports recorded as rejected by the server (400, 409, 413, 415, 422) SHALL NOT be resent and SHALL be reported with reason `rejected_by_server`; deleting the rejection marker makes such a report replayable again. An optional `--budget <seconds>` (1-3600) SHALL replace the 30-second publishing budget. A replay command SHALL return nonzero if reports remain unsent, rejected by the server or invalid, where invalid includes files rejected during the original collection.

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

#### Scenario: Server rejected a report
- **WHEN** a report was recorded as rejected by the server during an earlier attempt
- **THEN** replay sends nothing for it, reports `rejected_by_server` and exits nonzero

#### Scenario: Tampered run directory
- **WHEN** the manifest has an unsupported version, an invalid identity, project or environment variable name, a job directory that does not match its job name, or a snapshot that no longer passes contract validation
- **THEN** replay sends nothing for the affected reports (nothing at all for manifest-level violations) and exits nonzero

#### Scenario: Manifest not finalized
- **WHEN** the run directory's manifest was never finalized
- **THEN** replay refuses to build requests from it and leaves the raw files in place

#### Scenario: Signal during replay
- **WHEN** a signal interrupts the replay
- **THEN** no new request starts and the signal exit code takes precedence

### Requirement: Report processing is bounded and structural
Collection SHALL enforce the server's 32 MiB request limit, 100,000-record report limit and timing-only field contract, taken from one versioned local copy of the server contract. A source file larger than 33 MiB (the request limit plus a 1 MiB allowance) SHALL be rejected without parsing, and parsing SHALL stop once the records alone exceed the request limit. Each valid request SHALL be frozen as it is built, so at most one report body is held in memory. It SHALL cap collection at 256 reports and 256 MiB of source data per job. A malformed final JSONL fragment SHALL be discarded and the envelope SHALL set `truncated: true` (the server derives incomplete status); earlier malformed records MUST reject that report. Original files MUST remain intact.

#### Scenario: Worker is killed during its final write
- **WHEN** valid timing records end with one truncated JSON fragment
- **THEN** the valid prefix is retained with `truncated: true` without inventing a finish event

#### Scenario: Malformed earlier record
- **WHEN** a record before the final line is not valid JSON
- **THEN** the report is rejected with a structural code, nothing is uploaded for it and the file is untouched

#### Scenario: Report exceeds limits
- **WHEN** a report would exceed 32 MiB encoded or 100,000 records
- **THEN** it is not uploaded, the file is retained and a structural code is emitted

### Requirement: Reporting output preserves the existing output protocol
Reporting SHALL emit structural collection/delivery status in both text and versioned JSON event modes. Existing job durations, statuses, dependency scheduling and summary semantics MUST remain unchanged. Raw timing records, response bodies, source contents and secrets MUST NOT be printed as diagnostics. Reporting events are best-effort: failing to write one MUST NOT change the check exit status, and the manifest SHALL be finalized before collection events are emitted.

#### Scenario: JSON output with upload failure
- **WHEN** the CLI runs with `--json` and publication fails
- **THEN** every stdout line remains a valid versioned event and the existing workflow summary still describes check results
