# run-report Specification

## Purpose

Gives a person reading the terminal one closing report after a workflow run: which jobs passed, failed or were skipped, why the failures happened, how long the run took and how to rerun what failed.

## Requirements

### Requirement: Text runs end with a run report
In text mode, `goodflows run` SHALL render the `summary` event as a run report and print it last, after every job and reporting line. The report SHALL replace the previous `Summary:` count line and the `All jobs passed` / `All checks succeeded (some jobs skipped)` lines. JSON mode MUST NOT print the report.

#### Scenario: Report is the last output
- **WHEN** a workflow with reporting configured finishes in text mode
- **THEN** the run report appears after the reporting lines and nothing follows it

#### Scenario: JSON mode has no report
- **WHEN** the CLI runs with `--json`
- **THEN** every stdout line is a versioned JSON event and no report text is printed

### Requirement: Report header states the outcome and wall time
The report SHALL start with a header naming the workflow, the run outcome and the run's wall-clock time, which includes reporting after the last job. The outcome SHALL be `FAILED` if any instance failed or timed out, otherwise `CANCELLED` if any instance was cancelled or a signal arrived before the summary was emitted, otherwise `PASSED`. The signal state shown SHALL be the same one that determines the exit code.

#### Scenario: Passing run
- **WHEN** every job in workflow `ci` passes or is skipped
- **THEN** the header names `ci`, shows `PASSED` and shows the run's total duration

#### Scenario: Interrupted run
- **WHEN** a signal cancels running jobs and no instance failed or timed out
- **THEN** the header shows `CANCELLED`, the cancelled job's row shows `CANCEL`, and there is no Failures section or rerun command

#### Scenario: Signal after every job finished
- **WHEN** every job passed but a signal arrived before the summary was emitted, so the run exits with `128+n`
- **THEN** the header shows `CANCELLED`, not `PASSED`

### Requirement: Report lists one row per configured job
The report SHALL list one row per selected configured job, including transitive prerequisites, in configuration order. Each row SHALL show the job's status, its duration if any instance started, and, for a skipped job, the distinct nonempty skip reasons in instance order without duplicates. Start state SHALL come from the summary result's `started` flag, meaning execution entered `run_job`, rather than from status or a positive duration. A row for a job that never started MUST NOT show a duration; a started job with a recorded duration of zero SHALL show `0ms`.

#### Scenario: Skipped dependent
- **WHEN** `deploy` is skipped because a dependency did not pass
- **THEN** its row shows a skipped status and the reason `dependency did not pass`, with no duration

#### Scenario: Started condition skip
- **WHEN** a job enters execution and its condition causes it to be skipped
- **THEN** its row shows the condition skip reason and its recorded duration, even if that duration is zero

#### Scenario: Cancellation before launch
- **WHEN** a queued job is cancelled before entering execution
- **THEN** its row shows cancelled with no duration

### Requirement: Partition instances are rolled up into one row
A partitioned job's row SHALL show the most severe instance status, in the order failed, timed out, cancelled, passed, skipped. When any instance did not pass, the row SHALL show each nonzero non-passed status count in that order (for example `2/3 failed`); passed counts are implicit. All-passed rows and unpartitioned rows SHALL have no instance breakdown. The row's duration SHALL be the longest duration among started instances, omitted when none started.

#### Scenario: Some instances fail
- **WHEN** a 3-partition `test` has 2 failed instances and 1 passed instance
- **THEN** the report shows a single `test` row with a failed status and the breakdown `2/3 failed`, and its duration is the longest of the three

#### Scenario: All instances pass
- **WHEN** every instance of a 3-partition `test` passes
- **THEN** the `test` row shows a passed status and no breakdown

#### Scenario: Passed and skipped instances
- **WHEN** one instance of a 3-partition `test` passes and two are skipped by condition
- **THEN** the row shows passed with `2/3 skipped`, and its duration is the longest duration among started instances

#### Scenario: Different skip reasons
- **WHEN** all instances are skipped and their result errors include different reasons
- **THEN** the row lists each distinct reason once in instance order, with `N/N skipped`, and shows a duration only if an instance started

### Requirement: Report counts configured jobs by status
The report SHALL show counts of configured jobs by row status: passed, failed, timed out, skipped and cancelled. Zero counts MUST be omitted. A partitioned job SHALL count once, under its row status.

#### Scenario: Partitioned failure counts once
- **WHEN** `fmt` and `lint` pass, both instances of a 2-partition `test` fail and `deploy` is skipped
- **THEN** the counts read 2 passed, 1 failed and 1 skipped

### Requirement: Report details each failed instance
When any instance failed or timed out, the report SHALL include a Failures section with one entry per such instance, in result order. Each entry SHALL name the instance as `<name>` or `<name> (<index>/<count>)` and show its failed or timed-out status. It SHALL identify the first step with a failed or timed-out `status` when one exists, and show the exit code, command and error only when present. Nonempty job-level stderr and stdout tails SHALL be shown with truncation markers when applicable; these tails MUST NOT be described as exclusively from the failing step. Cancelled and skipped instances MUST NOT get an entry.

#### Scenario: Failed partition instances
- **WHEN** instances 1 and 2 of `test` fail with distinct stderr output
- **THEN** the Failures section has an entry for `test (1/2)` and one for `test (2/2)`, each with its own stderr tail

#### Scenario: Failed step
- **WHEN** step `integration` of job `test` fails
- **THEN** the entry for `test` names step `integration` and shows that step's command

#### Scenario: Failure outside a step
- **WHEN** a stepped job fails during preparation, job-condition evaluation or job-output evaluation, without a failed step
- **THEN** its entry shows the failed status and available job-level error, without inventing a failing step, command or exit code

#### Scenario: Timeout without an exit code
- **WHEN** an instance times out and has no exit code
- **THEN** its entry shows timed out and the available command, error and tails without a fabricated exit code

#### Scenario: Timed-out step
- **WHEN** step `a` of job `test` passes and step `slow` times out
- **THEN** the entry for `test` shows timed out, names step `slow` and shows no exit code

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

### Requirement: Job-provided text cannot rewrite the terminal
In text mode, the report and the live failure and step error lines SHALL print control characters other than tab in commands, errors, skip reasons and output tails as visible escapes (for example `\u{1b}` or `\r`), keeping tail line breaks as separate lines. Job names containing control characters MUST fail validation; other job-name rules are unchanged.

#### Scenario: Escape sequence in a tail
- **WHEN** a failed job's stderr tail contains `ESC [2J` and a bare carriage return
- **THEN** the report shows them as `\u{1b}[2J` and `\r` and writes no raw control characters other than newlines

#### Scenario: Control character in a job name
- **WHEN** a job name contains an escape character or a tab
- **THEN** validation fails with an error naming the job and its invalid control characters, while names with spaces or commas stay valid

### Requirement: Live failure lines stay short in text mode
In text mode, a failed or timed-out `job_finished` line SHALL show the instance, status, duration, and exit code and error when available, only. The command and output tails MUST NOT be printed there, because the run report shows them. JSON `job_finished` events MUST keep their command and tail fields.

#### Scenario: Failure while the run continues
- **WHEN** `test (1/2)` fails while other jobs are still running, in text mode
- **THEN** its finished line and error line are printed, without the command or `stdout tail` / `stderr tail` blocks

#### Scenario: Timeout while the run continues
- **WHEN** a job with `timeout_secs = 1` prints `out` and then sleeps past its timeout, in text mode
- **THEN** its live finished line shows `TIMED OUT` without a command or tail block, and the report's entry shows `stdout tail:` with `out`

### Requirement: JSON summary carries run duration and log directory
The JSON `summary` event SHALL include integer `duration_ms`, measured immediately before `runner::execute` until summary construction after reporting finishes, integer `execute_ms`, measured from the same start until `runner::execute` returns, and, when a log directory is in use, `log_dir` with that run's log directory as specified by `run-logs`, converted lossily when the path is not valid UTF-8. Without a log directory, `log_dir` MUST be absent. The event SHALL include the optimizer fields specified by `run-history`, including `history_file` only when a history file was written, as the resolved path the run wrote, converted lossily when the path is not valid UTF-8. Each `summary.results` entry SHALL additionally include boolean `started`, true if it entered `run_job` and false if skipped or cancelled before launch; this field MUST NOT be added to live `job_finished` events. Optimizer fields MUST NOT be added to live `job_finished` events. Every existing `summary` field MUST keep its name and meaning, including per-instance counts, except that `log_dir` names the per-run directory.

#### Scenario: Summary fields
- **WHEN** a workflow runs with `--json --log-dir logs`
- **THEN** the `summary` event has an integer `duration_ms`, a `log_dir` naming that run's log directory, and its `passed`, `failed`, `timed_out`, `skipped`, `cancelled` and `results` fields are unchanged in meaning

#### Scenario: Wall time covers jobs and reporting
- **WHEN** a job runs `sleep 0.2`
- **THEN** `duration_ms` is at least 200 and at least every result's `duration_ms`, and it also covers reporting retries after the last job, while `execute_ms` is at least 200 and does not cover them

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
