# Spec Delta

## MODIFIED Requirements

### Requirement: Reporting is explicitly configured
The CLI SHALL support a top-level reporting block and per-job `reports = ["exunit-timings"]`. Configuration without reporting SHALL retain existing behavior. Unsupported or duplicate formats, `reports` declared without a reporting block, inline tokens, a project not matching `[a-z0-9][a-z0-9_-]*` (1-64 bytes), a reporting job name longer than 64 UTF-8 bytes or containing control characters, `url_env`/`token_env` values that are not environment variable names, name a reserved goodflows variable (`GOODFLOWS_RUN_ID`, `GOODFLOWS_REPORT_DIR`, `GOODFLOWS_OUTPUT`, `GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT`) or name the same variable, a job or step `env` entry for the configured `token_env`, and malformed reporting configuration MUST fail validation before jobs execute. Inspection commands MUST NOT create report directories or contact the server.

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

#### Scenario: Credential variable names a partition variable
- **WHEN** the reporting block sets `url_env` or `token_env` to `GOODFLOWS_PARTITION` or `GOODFLOWS_PARTITION_COUNT`
- **THEN** validation fails and no jobs execute
