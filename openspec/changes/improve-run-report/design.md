# Design

## Context

Text rendering lives in one function, `render` in `src/output.rs`. It runs on a dedicated writer thread and turns each JSON event into text. It is stateless: each event is rendered on its own, with no memory of earlier events. `Output::emit` adds `workflow` and, when set, `environment` to every event before rendering.

`src/main.rs` emits `summary` as the final event of `goodflows run`. Its `results` array holds one `JobResult` per scheduled unit, in unit order: configured job order, then partition index (`src/runner.rs`, `results[index]`). Each result already carries status, duration, exit code, error, command, steps, partition and output tails. For a failed step, the runner sets the result's `command` to the step command and prefixes `error` with `step '<id>': `.

The report needs three things the event lacks today: run wall-clock time, the log directory and an explicit per-result start marker. `JobResult.executed` is not serialized and tracks main/step command spawning, not entry into job execution, so it cannot serve as that marker.

## Goals / Non-Goals

**Goals:**
- Build the report entirely from the `summary` event, so the renderer stays stateless.
- Keep JSON output backward compatible: fields are only added.

**Non-Goals:**
- Recording which dependency caused a skip. That needs runner changes.
- Reproducing `--config`, `--parallelism` or fail-fast flags in the rerun hint.
- Any change to live streaming of `job_output` lines.

## Decisions

### Render from the summary event only
The report is computed from `summary.results` (including each result's new `started` flag) plus the event's `workflow`, `environment`, `duration_ms` and `log_dir`.
*Alternative:* accumulate state in the writer thread from `job_finished` events. Rejected: it duplicates data the summary already has, and state would leak between the runs of one process (tests call `render` directly).

### Roll up by `name` over consecutive results
Results are already grouped by job, so grouping consecutive results with the same `name` yields rows in configuration order without needing the config. Selected jobs include transitive prerequisites. The row status is the most severe status in the group, in the order failed > timed_out > cancelled > passed > skipped. For partitioned jobs, show nonzero non-passed status counts in that order (`2/3 failed`, `1/3 failed, 1/3 timed out`); passed counts are implicit. Show no breakdown when all instances pass, and no partition breakdown for unpartitioned jobs.

The row duration is the maximum `duration_ms` among results with `started: true`, omitted only when none started. Zero is a valid duration. A condition-skipped job that entered execution therefore has a duration; queued dependency/fail-fast skips and queued cancellations do not. For a skipped row, show distinct nonempty result errors in partition order, deduplicated, so mixed skip reasons are retained.

Add an internal `JobResult.started` boolean, default false and set true on entry to `run_job`, with `#[serde(skip)]`. In `main.rs`, serialize results for the summary and insert this boolean into each result object. Do not change `job_finished` serialization or the existing reporting `executed` flag. This keeps the renderer stateless and preserves live JSON event shapes.

### Failure entries reuse existing result fields
The failing step is the first entry in `steps` whose flattened `status` is failed or timed out, if any. A stepped job can fail before any step (preparation or job condition) or after all steps (job output evaluation); do not invent a step in those cases. Command, error, tails and truncation flags come straight from the result. Always show the failure status, add an exit code only when present, and omit unavailable command/error fields rather than printing null or an empty placeholder. Tails are job-level bounded tails and can contain earlier step output; do not label them as belonging exclusively to the failing step. This is the text the trimmed `job_finished` block no longer prints.

### Wall time and log directory measured in `main.rs`
`duration_ms` is measured with an `Instant` taken immediately before `runner::execute` and read when the summary is built, so it includes the time spent finishing reporting. `log_dir` is the effective `options.log_dir` (including the encoded workflow subdirectory for named workflows, or the original directory for legacy top-level jobs), and is omitted when unset.
*Alternative:* sum or max job durations. Rejected: neither equals wall time under parallelism.

### Rerun hint built from the event
Usual format: `goodflows run <workflow> [--env=<env>] --jobs=<a>,<b>`. The workflow is always included, because an explicit `default` also works for legacy top-level-job configs. Shell-quote each value: nonempty tokens entirely within `[A-Za-z0-9_.:/-]` can remain bare; otherwise single-quote and replace each embedded apostrophe with the POSIX shell sequence `'"'"'`. Quote the joined job list as one value. Use equals-form options so values beginning with `-` are not parsed as flags. When the workflow begins with `-`, place it last after `--` instead of before the options.

Jobs are the rows whose status is failed or timed_out, in row order. `--jobs` also selects their dependencies, which matches what a rerun needs. A valid job name can contain a comma, which Clap splits even when shell-quoted. If any failed job name contains a comma, omit `--jobs` and emit a full-workflow rerun instead, preceded by a note explaining why selective rerun is unavailable. Do not change name validation or CLI parsing in this change.

### Layout
```
default  FAILED  in 710ms
  PASS  fmt        204ms
  PASS  lint       110ms
  FAIL  test         3ms  2/2 failed
  SKIP  deploy            dependency did not pass
2 passed, 1 failed, 1 skipped

Failures
--- test (1/2)  FAILED  exit 1
  command: ...
  error: exited with 1
  stderr tail:
    ...

--- test (2/2)  FAILED  exit 1
  command: ...
  error: exited with 1
  stderr tail:
    ...

Logs: <dir>
Rerun failed: goodflows run default --jobs=test
```
Names are padded to the longest name. Durations use `ms` below one second, `x.ys` below one minute and `XmYYs` above. Only the status label and the header outcome are coloured, with `colored` as in today's `job_finished`.

## Risks / Trade-offs

- [The run ends without a summary (runner error, or output suppressed after an interrupt with a stuck consumer), so the trimmed tails never appear] → Raw lines are still streamed live and are in `--log-dir` files. JSON keeps the tails.
- [Text output changes break scripts that grep `Summary:` or `All jobs passed`] → `--json` is the supported machine interface. Call out the change in release notes.
- [The rerun hint omits `--config`, so it is wrong for a run that used a non-default config path] → Accepted for now. The hint is advisory; adding the config path would mean adding another summary field.
- [A long failure list makes the report long] → Tails are already bounded by the runner's tail limits. Accepted.

## Migration Plan

Ship in the next minor release with a README compatibility note on the text output change (there is no existing changelog file); carry that note into the release description. Rollback is a revert. No persisted data changes.
