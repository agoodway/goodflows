# Proposal

## Why

A workflow run leaves no history an agent can compare. `--log-dir` overwrites the same stdout and stderr files on the next run, and the `summary` event records wall time only after a job is launched, so queue wait, dependency wait, CPU time, and memory are invisible. Optimizing parallelism, partitions, and timeouts needs those signals kept across runs, in files small and stable enough to commit.

## What Changes

- Keep each `--log-dir` invocation in its own directory and retain the newest runs, default 10, instead of truncating the previous run's files. The cap is configurable. A run that does not finish still occupies a slot.
- Add an opt-in run history, enabled by a config setting or a CLI flag, and disabled by a CLI flag when the config enables it. Each enabled run writes one new immutable JSON file for that run only. Files are never rewritten. The default directory is `goodflows-history/` next to the config file.
- Cap history per workflow by file count (default 1000) and total bytes (default 10 MB). Either cap drops the oldest files. The byte cap is a default, not a maximum: a setting or flag may set it lower or higher. The caps bound the checkout. Records that were committed and later deleted remain in git history.
- Record, in that file and on the JSON `summary` event, the data an agent needs to tune the workflow: run outcome and wall time, configured parallelism, logical CPUs, host memory, max jobs in flight, a hash of the selected job graph, and per instance blocked time, queued time, wall time, CPU time, peak RSS when measurable, status, exit, truncated command, and per spawned command or step the same costs. Stdout, stderr, environment values, and absolute paths stay out of the file.
- Show the per-run log directory, and the history file when one was written, in the text run report.
- **BREAKING** for `--log-dir` consumers: log files move under a per-run directory, and `summary.log_dir` names that directory. Basenames stay the same. JSON `summary` additions are additive.

Out of scope: committing or pruning git history, fetching logs or history from a pool run, copying command output into the history file, and sampling a memory timeline.

## Capabilities

### New Capabilities

- `run-logs`: per-run stdout and stderr directories under `--log-dir`, and the configurable count of runs kept.
- `run-history`: opt-in immutable per-run optimizer records, their caps, and the fields shared with the `summary` event.

### Modified Capabilities

- `run-report`: `summary.log_dir` names the per-run log directory; `summary` gains the optimizer fields; the text report shows that log directory and, when history is written, the history file.
- `job-partitions`: each instance still gets its own stdout and stderr files, now inside the per-run directory, and a later run does not truncate a retained run.

## Impact

- `src/main.rs`: history and log-retention flags, summary fields, history write and prune after the run.
- `src/config.rs`: history and retention settings on shared, workflow, and profile settings, with the same precedence as parallelism.
- `src/runner.rs`: per-run log directory, blocked and queued time, per-command CPU time and peak RSS.
- `src/output.rs` and `src/output/report.rs`: render the new paths; leave the job table unchanged.
- `README.md`: log layout, history files, settings, and git guidance (history is safe to commit; log and spool directories stay gitignored).
- `tests/` for scheduling, output, and config. Add `sha2` for the job-graph hash. Pool execution is unchanged and does not retrieve these files.
