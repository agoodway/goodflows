# Spec Delta

## ADDED Requirements

### Requirement: Settings declare secret references
`[settings]`, a workflow's `settings` and a profile's `settings` SHALL accept a `secrets` table that maps an environment variable name to a secret reference. Layers SHALL merge by key in the order `[settings]`, workflow, profile, like `env`. A profile MUST NOT define a name that neither `[settings]` nor the workflow defines.

#### Scenario: Profile switches vault
- **WHEN** `[settings.secrets]` sets `DATABASE_URL = "op://dev/postgres/url"`, `[environments.ci.settings.secrets]` sets `DATABASE_URL = "op://ci/postgres/url"`, and the run passes `--env ci`
- **THEN** goodflows reads `op://ci/postgres/url` and not `op://dev/postgres/url`

#### Scenario: Profile adds a name
- **WHEN** `[environments.ci.settings.secrets]` sets `NPM_TOKEN` and no `[settings]` or workflow secrets define it
- **THEN** `goodflows validate` exits 2 with `config_invalid`

### Requirement: References are URIs and only op:// is supported
A secret reference SHALL be a URI whose scheme selects the provider. The only supported scheme SHALL be `op`. An `op://` reference MUST be at most 1024 bytes, contain no whitespace or control characters, and have three or four nonempty path segments, optionally followed by a `?` query. Any other value MUST be a `config_invalid` error whose message names the supported schemes.

#### Scenario: Unsupported scheme
- **WHEN** a secret is `0p://dev/postgres/url` or `hunter2`
- **THEN** `goodflows validate` exits 2 with `config_invalid`, and the message says only `op://` references are supported

#### Scenario: Section and query
- **WHEN** a secret is `op://dev/github/2fa/one-time password?attribute=otp`
- **THEN** `goodflows validate` rejects it for the space, and `op://dev/github/2fa/otp?attribute=otp` is accepted

### Requirement: Secret names are checked
A secret name MUST match `[A-Za-z_][A-Za-z0-9_]*` and MUST NOT be a goodflows reserved variable, the reporting `url_env` or `token_env`, or the `[cache] token_env`. One settings table MUST NOT set the same name in `env` and `secrets`. A job's or step's `env` MUST NOT set a name the job lists in `secrets`.

#### Scenario: Reporting token
- **WHEN** `[settings.secrets]` sets `GOODFLOWS_API_KEY` and `[reporting]` uses the default `token_env`
- **THEN** `goodflows validate` exits 2 with `config_invalid`

#### Scenario: Name in env and secrets
- **WHEN** `[settings.env]` and `[settings.secrets]` both set `DATABASE_URL`
- **THEN** `goodflows validate` exits 2 with `config_invalid`

### Requirement: Jobs opt in to secrets
A command or step job SHALL accept `secrets`, a list of names. Every listed name MUST be defined in `[settings.secrets]` or its workflow's `settings.secrets`, and MUST appear once. A call job MUST reject `secrets`. Only the listed secrets SHALL be set for the job's command and each of its steps. Condition commands, expressions, cache keys and other jobs MUST NOT receive a secret value. A listed secret SHALL replace a caller or `env` variable of the same name.

#### Scenario: Only the listing job receives it
- **WHEN** job `integration` lists `DATABASE_URL` and job `unit` does not, and both print `${DATABASE_URL:-unset}` to a file
- **THEN** `integration`'s file holds the resolved value and `unit`'s holds `unset`

#### Scenario: Expressions never see the value
- **WHEN** job `integration` lists `DATABASE_URL`, the caller's environment does not set it, and the job's condition is `(env.DATABASE_URL ?? "none") == "none"`
- **THEN** the condition is true and the job runs

#### Scenario: Undefined name
- **WHEN** a job lists `STRIPE_KEY` and no settings define it
- **THEN** `goodflows validate` exits 2 with `config_invalid` naming the job and `STRIPE_KEY`

### Requirement: Secrets are resolved once, before any job, only when needed
goodflows SHALL resolve, before the root invocation starts, each distinct reference listed by a selected root job or by any job in a workflow it calls. Each reference SHALL be read once per run with `op read --no-newline <reference>`. The first read SHALL run alone and the rest SHALL run in parallel, at most 8 at a time. A run whose selection lists no secret MUST NOT run or look for `op`. `validate` and `list` MUST NOT run `op`.

#### Scenario: No secrets selected
- **WHEN** only job `integration` lists a secret and the run passes `--jobs fmt`
- **THEN** `op` is never run, and the run succeeds with no `op` on `PATH`

#### Scenario: Shared reference
- **WHEN** two selected jobs in different called workflows list names that point at the same reference
- **THEN** `op` is run once for that reference

#### Scenario: Validate
- **WHEN** `goodflows validate` checks a config with secrets
- **THEN** `op` is never run

### Requirement: Resolution failures stop the run
When `op` is not on `PATH`, a read exits nonzero, a read exceeds its timeout (120 seconds for the first, 30 seconds for the rest), or a value is not valid UTF-8 or contains NUL, goodflows MUST stop before any job starts, print an `error` event with code `secret_unavailable` and field `secret` set to the name, and exit 1. The message MUST name the variable and the reference and MUST NOT contain any resolved value.

#### Scenario: Unknown item
- **WHEN** `op read` for `DATABASE_URL` exits 1 with `"postgres" isn't an item in the "ci" vault`
- **THEN** no job starts, the run exits 1, and the `error` event has `code` `secret_unavailable`, `secret` `DATABASE_URL`, and a message that names `op://ci/postgres/url` and includes `op`'s first stderr line

#### Scenario: op missing
- **WHEN** a selected job lists a secret and `op` is not on `PATH`
- **THEN** no job starts and the run exits 1 with `secret_unavailable`

### Requirement: Resolved values are masked
goodflows SHALL replace every resolved value of at least 4 bytes with `[redacted]` in all job stdout and stderr before it reaches the terminal, `job_output` events, log files, failure output heads and tails, the summary, the last-run record, MCP results and report uploads, and in published job outputs. A value split across reads or lines SHALL still be masked. A value shorter than 4 bytes SHALL be delivered unmasked with one warning that names the secret.

#### Scenario: Value in failure output
- **WHEN** a job that lists `DATABASE_URL` prints the value and fails
- **THEN** the terminal, the log file, `goodflows last` and the summary's `output_tail` show `[redacted]` and never the value

#### Scenario: Split across chunks
- **WHEN** a job writes the first half of a value, sleeps, then writes the second half
- **THEN** the log file and `job_output` events contain `[redacted]` and neither half alone

#### Scenario: Another job prints it
- **WHEN** a job that lists no secret prints a resolved value it read from a file
- **THEN** its output shows `[redacted]`

### Requirement: Published job outputs are masked
goodflows SHALL mask resolved values in every key and every string value of a job's published outputs, and SHALL keep the job error for an invalid output file free of resolved values in any form, before any event, expression, summary or last-run record sees them. A number value SHALL be replaced by the string `[redacted]` when the text the job wrote, or its parsed rendering, contains a resolved value. Two keys that mask to the same text SHALL fail the job.

#### Scenario: Numeric output and output key
- **WHEN** a job that lists `PIN` (value `12345678`) writes `{"pin": 12345678, "12345678": true}` to `GOODFLOWS_OUTPUT`
- **THEN** the `job_finished` event, the summary and the last-run record show the outputs as `{"pin": "[redacted]", "[redacted]": true}` and never the value

#### Scenario: Number written in a form the parser changes
- **WHEN** a job writes a resolved value `12345678901234567890123` or `1234.50` as a JSON number output
- **THEN** the output is `"[redacted]"`

#### Scenario: Keys that collide after masking
- **WHEN** a job writes two keys that are different resolved values
- **THEN** the job fails with `output '[redacted]' is duplicated after masking` and neither value appears

#### Scenario: Output file that is a JSON string
- **WHEN** a job writes a resolved value as a JSON string or number (such as `1234.50`) instead of an object
- **THEN** the job fails with `expected a JSON object at line 1 column N` and its error contains neither the value nor its parsed form

### Requirement: A run holding secrets is protected from same-user processes
On Linux, a local run or Sprite controller that needs any secret SHALL make itself non-dumpable before the first read, so that other processes of the same user, including its jobs, cannot read its memory or `/proc/<pid>/environ`. A run that needs no secret SHALL NOT change this.

#### Scenario: A job probes its parent
- **WHEN** on Linux, as a user without `CAP_SYS_PTRACE`, a job reads `/proc/$PPID/environ` in a run where another job lists `DATABASE_URL`
- **THEN** the read fails, while in the same run with no job listing a secret it succeeds

### Requirement: Sprite runs resolve on the controller
For `run --sprite`, the controller SHALL resolve the run's secrets and send their values in the private environment frame, never in argv or the header. The Sprite MUST NOT run `op`. Setup and resume hooks MUST NOT receive secret values. Resolved values SHALL be treated as forwarded values: a source file that contains one is excluded from the snapshot, as forwarded values are, and they are masked in diagnostics.

#### Scenario: Remote job gets the secret
- **WHEN** a Sprite run selects a job that lists `DATABASE_URL`
- **THEN** the remote job receives the value, the fake Sprite never runs `op`, and the setup hook's environment has no `GOODFLOWS_SPRITE_SECRETS` and no value
