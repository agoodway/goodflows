# Spec Delta

## MODIFIED Requirements

### Requirement: Reporting is explicitly configured
The CLI SHALL support a top-level reporting block and per-job `reports = ["test-timings"]`. Configuration without reporting SHALL retain existing behavior. Unsupported or duplicate formats (including the retired `exunit-timings`), `reports` declared without a reporting block, inline tokens, a project not matching `[a-z0-9][a-z0-9_-]*` (1-64 bytes), a reporting job name longer than 64 UTF-8 bytes or containing control characters, `url_env`/`token_env` values that are not environment variable names, name a reserved goodflows variable (`GOODFLOWS_RUN_ID`, `GOODFLOWS_REPORT_DIR`, `GOODFLOWS_OUTPUT`, `GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT`, `GOODFLOWS_ENVIRONMENT`) or name the same variable, a job or step `env` entry for the configured `token_env`, and malformed reporting configuration MUST fail validation before jobs execute. Inspection commands MUST NOT create report directories or contact the server.

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

## ADDED Requirements

### Requirement: Contract violations map to stable structural codes
Collection SHALL reject a report that breaks the contract with one stable code: `unsupported_format`, `unsupported_event`, `unsupported_field`, `missing_field`, `invalid_field`, `field_too_long`, `invalid_order`, `identity_mismatch`, `invalid_identifier`, `invalid_partition`, `invalid_partition_count`, `unsupported_filter`, `invalid_meta`, `unpaired_event`, `unfinished_mismatch`, `duplicate_key`, `nul_character`, `non_integer_number`, `integer_out_of_range`, `nested_too_deeply`, `malformed_record`, `invalid_utf8`, `empty_report` or `missing_suite_started`. A code that collection already returns for one of these failures SHALL keep that name. Size and count limits SHALL keep `too_large`, `record_limit` and `request_too_large`, which are outside this set. The code SHALL NOT include submitted values. A filter that breaks the filter grammar SHALL be `unsupported_filter`, not `invalid_field`. An absent core key SHALL be `missing_field`. A second started event with an `execution_id` already in use for that kind SHALL be `unpaired_event`.

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

### Requirement: Snapshots in retired formats are not replayed
`goodflows reports upload` SHALL re-validate every saved snapshot against the current contract. A snapshot frozen in a retired format, such as `exunit-timings`, MUST NOT be sent: it SHALL be counted as `invalid_snapshot` and the command SHALL exit nonzero. Its file MUST remain intact.

#### Scenario: Replaying a pre-upgrade run directory
- **WHEN** a finalized manifest lists a pending `exunit-timings` snapshot
- **THEN** replay sends nothing for it, reports `invalid_snapshot`, leaves the file in place and exits nonzero

## REMOVED Requirements

### Requirement: Suite filter tags are boolean identifiers
**Reason**: The filter grammar moves to `test-timing-contract`, where `filters` is a required nullable core field and `null` is accepted. Leaving this requirement in place would reject every Jest report.
**Migration**: Enforce "Suite filters are boolean identifier tags" in `test-timing-contract`. A missing `filters` key is `missing_field`. A present value that breaks the grammar is `unsupported_filter`.
