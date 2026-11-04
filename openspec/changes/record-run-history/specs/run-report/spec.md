# Spec Delta

## MODIFIED Requirements

### Requirement: Report points at logs and a rerun command
When the run used a log directory, the report SHALL show that run's log directory. When a history file was written, the report SHALL show that file's path, after the log directory line when both are shown. When any job's row status is failed or timed out, the report SHALL end with a rerun command `goodflows run <workflow> [--config=<path>] [--env=<env>] --jobs=<names>` that lists, once each and in configuration order, every job whose row is failed or timed out and every skipped job with the skip reason `dependency did not pass`, and keeps the workflow and any `--env` selection of the original run. `--config` SHALL be included, with the path as given, only when the original run passed `--config` explicitly; this path MUST NOT be added to the JSON `summary` event. Values SHALL be POSIX shell-quoted with embedded apostrophes escaped. Option values SHALL use equals form; a workflow beginning with `-` SHALL appear last after `--`. If any job selected for the rerun has a name containing a comma, the report SHALL instead explain that selective rerun is unavailable and end with a full-workflow rerun command preserving workflow, config and environment but omitting `--jobs`.

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

### Requirement: JSON summary carries run duration and log directory
The JSON `summary` event SHALL include integer `duration_ms`, measured immediately before `runner::execute` until summary construction after reporting finishes, and, when a log directory is in use, `log_dir` with that run's log directory as specified by `run-logs`, converted lossily when the path is not valid UTF-8. Without a log directory, `log_dir` MUST be absent. The event SHALL include the optimizer fields specified by `run-history`, including `history_file` only when a history file was written, as the resolved path the run wrote, converted lossily when the path is not valid UTF-8. Each `summary.results` entry SHALL additionally include boolean `started`, true if it entered `run_job` and false if skipped or cancelled before launch; this field MUST NOT be added to live `job_finished` events. Optimizer fields MUST NOT be added to live `job_finished` events. Every existing `summary` field MUST keep its name and meaning, including per-instance counts, except that `log_dir` names the per-run directory.

#### Scenario: Summary fields
- **WHEN** a workflow runs with `--json --log-dir logs`
- **THEN** the `summary` event has an integer `duration_ms`, a `log_dir` naming that run's log directory, and its `passed`, `failed`, `timed_out`, `skipped`, `cancelled` and `results` fields are unchanged in meaning

#### Scenario: Wall time covers jobs and reporting
- **WHEN** a job runs `sleep 0.2`
- **THEN** `duration_ms` is at least 200 and at least every result's `duration_ms`, and it also covers reporting retries after the last job

#### Scenario: Non-UTF-8 log directory
- **WHEN** `--log-dir` names a path that is not valid UTF-8
- **THEN** the jobs' logs are written there and the run still ends with a `summary` whose `log_dir` is the lossy conversion of that path

#### Scenario: No log directory
- **WHEN** a workflow runs with `--json` without `--log-dir`
- **THEN** the `summary` event has no `log_dir` field

#### Scenario: Explicit start state
- **WHEN** a run contains a started instance, a condition-skipped instance that entered execution and a queued instance cancelled before launch
- **THEN** their summary results have `started` values true, true and false respectively, and their live `job_finished` events have no `started` field

#### Scenario: Per-run log directory
- **WHEN** two runs of workflow `ci` use `--json --log-dir logs`
- **THEN** each `summary.log_dir` names a different directory under `logs/ci/` and that directory contains the run id
