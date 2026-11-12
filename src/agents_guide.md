This guide ships in the binary, so it describes the goodflows that printed
it. Pass `--config PATH` to every command below when the run used it.

## Exit codes

| Exit | Meaning | Next step |
|---|---|---|
| `0` | Every selected job passed or was skipped | Green |
| `1` | A job failed or timed out, or goodflows hit a runtime error after it accepted the command and config | Read the `Failures` section; the command is right |
| `2` | A usage or config error: bad arguments, no config, a config that does not parse or validate, an unknown workflow, job, environment or run id, or no record for `last` or `run --failed` | Fix the command or the config, then run it again |
| `75` | Only from `wait` and `cancel`: the run is still going. Neither a pass nor a fail | Run the same command again |
| `130`, `143` | Interrupted by SIGINT or SIGTERM; `cancel` makes a detached run exit `143` | Rerun when nothing interrupts it |

`last` and `wait` exit with the exit code of the run they report, so `1`
means that run failed. Their own usage errors, and `last` with no record,
exit `2`.

## Error codes

With `--json`, a command that cannot do what it was asked ends with one
`{"type":"error","code":...}` event. Text mode prints the message after
`Error:`, and a suggestion after `Hint:` when there is one, such as a close
match for a misspelled job.

| `code` | Exit | Meaning |
|---|---|---|
| `usage` | `2` | The arguments did not parse, or combine flags that do not go together |
| `missing_config` | `2` | No `goodflows.toml` or `.goodflows.toml`, or the `--config` file cannot be read |
| `config_parse` | `2` | The file is not valid TOML, or has an unknown key or a wrong type |
| `config_invalid` | `2` | The file parsed but failed validation; `problems` lists every problem |
| `no_default_workflow` | `2` | The file has no default workflow and the command named none |
| `unknown_workflow` | `2` | The workflow name is not in the file; the message lists the names that exist |
| `unknown_job` | `2` | A `--jobs` name is not in the selected workflow; the message lists the names that exist |
| `unknown_environment` | `2` | The `--env` name is not in the file |
| `no_last_run` | `2` | `last` or `run --failed` found no recorded local run of that config, or `run --failed` named a different workflow or `--env` |
| `no_failed_jobs` | `2` | `run --failed` found nothing to rerun |
| `unknown_run` | `2` | `status`, `wait` or `cancel` found no detached run with that id beside that config |
| `config_exists` | `2` | `init` would overwrite an existing config; pass `--force` |
| `ambiguous_harness` | `2` | `skills install` detected harnesses that load different directories; pass `--harness` |
| `runtime` | `1` | Anything else that stopped goodflows |

`goodflows list` and `goodflows list --workflows` show the names that exist.

## Read a failure

A failed job is not a skipped one. A job is skipped when a dependency did not
pass, when its condition was false, or when fail-fast stopped new jobs after a
failure.

The text report ends a run: one row per job, the counts, then a `Failures`
section when a job failed or timed out. Each entry names the job, its failed
step when it has steps, its command, its error, and its output:

- Short output shows whole, in a `stdout:` block and a `stderr:` block.
- Longer output is one `stdout and stderr:` block in the order goodflows read
  the streams: the first 4 KiB, a `… N bytes omitted …` line, then the last
  4 KiB. Errors printed first, such as compiler errors, are in the start.
- With `--log-dir DIR`, `stdout log:` and `stderr log:` give the paths of the
  full output.

A failed root run ends with `Rerun failed:` and the command that reruns the
failed jobs, or `Rerun workflow:` when a job name contains a comma, which
`--jobs` cannot select. Copy either line as printed: a bare
`goodflows --jobs <names>` selects the default workflow and the default config.

Without `--output`, a run uses `--output=failures` when stdout is not a
terminal or a coding agent runs it: one status line per job, no job output,
then the report. `--output=stream` also prints every output chunk after
`[job stdout]` or `[job stderr]` (`[job/step stdout]` for a step).

## Show the last run again: `goodflows last`

`goodflows last` prints the last local run's report again, `Failures` section
included, without running anything, and exits with that run's exit code. A run
that called workflows shows each called workflow's report before the root's.
`goodflows last --json` prints the run's `summary` events. The record is kept
beside the config file, one per config file, and every local run replaces it.

## Rerun what did not pass: `goodflows run --failed`

`goodflows run --failed` reruns the failed and timed-out jobs of the last local
run, the jobs skipped because a dependency did not pass, and the jobs they
depend on, in the last run's workflow and `--env`. Do not add the workflow
name or `--env`: one that differs exits `2` with `no_last_run`. `--failed`
rejects `--jobs` and `--sprite`. Other run flags, such as `--log-dir` or
`--no-fail-fast`, are not kept, so pass them again when they matter. It exits
`2` with `no_failed_jobs` when the last run has nothing to rerun.

A `--sprite` run writes no record, so `last` and `--failed` see the local run
before it. Read that run's own report, and rerun it with its `Rerun failed:`
line, which keeps the workflow, `--config`, `--env` and `--sprite`.

## Detached runs

A coding agent's shell often stops a command after about 2 minutes, and a
stopped run leaves no result. Run a workflow that may take longer detached:

1. `goodflows run --detach`, or `goodflows run NAME --detach`, with the run's
   other flags. It returns at once and prints the run id. With `--json` it
   prints one `run_detached` event with `run_id`, `pid` and `run_dir`.
   `--detach` is a flag of `run`: a bare `goodflows --detach` exits `2`. It
   rejects `--sprite`. A usage or config error still exits `2` before anything
   starts.
2. `goodflows wait RUN_ID --timeout 90`, with the id written out, since the
   next command may run in a new shell. While it exits `75`, run the same
   `wait` again; stopping `wait` does not stop the run. Once the run finished,
   `wait` prints its report, or its `summary` events with `--json`, and exits
   with the run's exit code. A run that died, for example from SIGKILL, makes
   `wait` exit `1` with `stopped without a result` and the path of its
   `stderr.log`.
3. `goodflows status RUN_ID` shows the run's state and which jobs are running,
   passed, failed, timed out, skipped or cancelled.
4. `goodflows cancel RUN_ID` stops a run that is still going, cancelling its
   running and waiting jobs, and exits `0` once it stopped. A run that already
   stopped is reported as it is.

`status`, `wait` and `cancel` find the run beside the config, so pass the run's
`--config`; without it they exit `2` with `unknown_run`. Starting a detached
run keeps the newest 10 finished runs of that config and removes older ones.

## JSON

`--json` prints JSON Lines on stdout; every event has `schema_version` 1.
`--json=summary` on a run prints only `run_started`, any `error`,
`sprite_unresolved` when a Sprite run cannot report, and the `summary`, which
is last. A run that called workflows prints each called workflow's `summary`,
with `parent_run_id` and `caller_job`, before the root's.

The `summary` event:

- `success`: true only when every selected job passed or was skipped.
  `outcome` is `passed`, `failed` or `cancelled`.
- `passed`, `failed`, `timed_out`, `skipped`, `cancelled`: job counts.
- `results`: each job instance with `name`, `status`, `command`, `exit_code`
  and `error`, `partition` for a partitioned job, and `steps` for a job with
  steps.
- A failed or timed-out result has `output_head` and `output_tail`, the first
  and last 4 KiB of its stdout and stderr together in the order goodflows read
  them; `output_truncated` is true when output between them was left out, and
  `output_omitted_bytes` says how much. `stdout_tail` and `stderr_tail` are the
  last 4 KiB of each stream, with `stdout_truncated` and `stderr_truncated`.
- With `--log-dir`, each job that ran has `stdout_log` and `stderr_log`.
- A failed root run has `rerun_command`, the command of its `Rerun failed:` or
  `Rerun workflow:` line. A called workflow's summary has none.

The `run_status` event from `status`, `cancel`, and a `wait` that timed out
has `state` (`running`, `finished` or `lost`), `exit_code` once finished,
`jobs` (the job count), `elapsed_ms`, and a list of job names for each of
`running`, `passed`, `failed`, `timed_out`, `skipped` and `cancelled`. From
`cancel` it adds `was_running`, true when `cancel` stopped the run.

## MCP

`goodflows mcp` serves the same commands as MCP tools: `run` (with `detach`),
`wait`, `status`, `cancel`, `last`, `list` and `validate`. For a run that may
take more than a minute or two, call `run` with `detach` and then `wait` with
a short `timeout` until it returns the run's result.
