# Spec Delta

## MODIFIED Requirements

### Requirement: Report points at logs and a rerun command
When the run used a log directory, the report SHALL show that run's log directory. When a history file was written, the report SHALL show that file's path, after the log directory line when both are shown. When any job's row status is failed or timed out, the report SHALL end with a rerun command `goodflows run <workflow> [--config=<path>] [--env=<env>] [--sprite=<identity>] --jobs=<names>` that lists, once each and in configuration order, every job whose row is failed or timed out and every skipped job with the skip reason `dependency did not pass`, and keeps the workflow and any `--env` selection of the original run. A remote run SHALL preserve its original caller-supplied Sprite identity using `--sprite=<identity>`; a local run SHALL omit this flag. `--config` SHALL be included, with the path as given, only when the original run passed `--config` explicitly; this path MUST NOT be added to the JSON `summary` event. Values SHALL be POSIX shell-quoted with embedded apostrophes escaped. Option values SHALL use equals form; a workflow beginning with `-` SHALL appear last after `--`. If any job selected for the rerun has a name containing a comma, the report SHALL instead explain that selective rerun is unavailable and end with a full-workflow rerun command preserving workflow, config, environment and any Sprite identity but omitting `--jobs`.

#### Scenario: Rerun hint for a partitioned failure
- **WHEN** `lint` precedes `test` in configuration order, and two instances of `test` and the job `lint` fail
- **THEN** the report ends with a rerun command selecting `lint` and `test`, each named once

#### Scenario: Shell and option characters in names
- **WHEN** workflow, environment or failed-job names contain spaces, apostrophes, shell metacharacters or leading hyphens, and failed-job names contain no commas
- **THEN** shell tokenization and CLI parsing of the hint preserve the exact workflow, environment and failed-job names without treating name content as commands or flags

#### Scenario: Comma in a failed-job name
- **WHEN** a failed configured job is named `test,unit`
- **THEN** the report explains the selector limitation and ends with a full-workflow rerun command without `--jobs`

#### Scenario: Comma fallback keeps environment and workflow position
- **WHEN** a failed job named `test,unit` runs in workflow `-wf` with `--env prod`
- **THEN** the report ends with `Rerun workflow: goodflows run --env=prod -- -wf`

#### Scenario: Skipped dependents are rerun
- **WHEN** `build` fails, `deploy` (which depends on `build`) is skipped with reason `dependency did not pass`, and `late` is skipped by fail-fast
- **THEN** the rerun command selects `build` and `deploy` but not `late`

#### Scenario: Timed-out job only
- **WHEN** the only non-passing job `slow` timed out
- **THEN** the report ends with a rerun command selecting `slow`

#### Scenario: Explicit config path
- **WHEN** the run used `--config 'my ci.toml'` and a job failed
- **THEN** the rerun command includes `--config='my ci.toml'`, and running it through `sh` uses that config; a run without `--config` gets a hint without it

#### Scenario: No hint on success
- **WHEN** every job passes
- **THEN** the report shows no Failures section and no rerun command

#### Scenario: History path
- **WHEN** history is enabled and the file is written
- **THEN** the report shows that file's path after the log directory line, or as the only such path when `--log-dir` was not set

#### Scenario: History disabled
- **WHEN** history is off
- **THEN** the report has no history path

#### Scenario: Remote selective rerun
- **WHEN** a run with `--sprite T-example --env test --config custom.toml` has a failed job
- **THEN** the rerun command preserves that identity, profile, and original controller config path rather than pointing at the remote staged config

#### Scenario: Remote comma fallback
- **WHEN** a remote run with workflow `-wf`, `--env prod`, and `--sprite T-example` has a failed job named `test,unit`
- **THEN** the full-workflow hint preserves `--sprite=T-example` before the final `-- -wf` and omits `--jobs`
