# Design

## Context

See proposal.md for why this exists. Today `Options.log_dir` is the workflow log directory (`--log-dir` plus `path_component` for a named workflow, or `--log-dir` itself for legacy top-level jobs). `run_job` creates `<index>-<name>[.pNN].stdout.log` and `.stderr.log` with `File::create`, so the next run truncates them. `duration_ms` starts when `run_job` starts. The scheduler in `runner::execute` is the only place that knows a job was waiting on dependencies or on a free slot. `summary` is built in `src/main.rs` after reporting and rendered by `src/output/report.rs`. Settings already merge shared, workflow, and profile values, with CLI overrides applied in `main` for parallelism and fail-fast. `libc` is available on Unix. There is no hash crate yet.

## Goals / Non-Goals

**Goals:** One run id shared by the log directory and the history file. Prune by filename so mtime and git checkout do not choose the victim. Publish optimizer fields from the same structs the history file serializes. Measure CPU and peak RSS without a new sampler thread when output is flowing, and with a slow poll when it is not.

**Non-Goals:** See proposal.md. Also out of scope: editing the user's `.gitignore` or `.gitattributes`, restoring the old flat log paths, and hashing environment values.

## Decisions

### One run identity, lexical names

Create one lowercase UUID and one UTC timestamp, truncated to a second, at the start of a run. Format the directory and file stem as `YYYYMMDDTHHMMSSZ-<uuid>`. Logs go in `<effective log dir>/<stem>/`. History goes in `<history dir>/<path_component(workflow)>/<stem>.json`, and legacy top-level jobs use the workflow name `default` only for history. Log placement otherwise stays as it is today, so a legacy config does not gain a `default/` log subdirectory.

Prune compares those names as strings. Never delete the stem just created, even if the clock steps backward and an older name sorts newer. Never delete a name that does not match the stem pattern, and never follow or unlink a symlink. Create the log run directory before jobs; if that fails, the run fails as it does today. Prune logs immediately after that create so a crash loop stays within the cap. Write and prune history after the summary fields are known and before the summary is emitted, so `history_file` is present only when the write succeeded. History and log prune errors are reported on stderr and do not change the exit code.

Alternatives considered: pruning by mtime is wrong after `git checkout`. One rewritten `history.jsonl` conflicts on every branch and rewrites bytes git already stored. A timestamp alone collides when two runs start in the same second.

### Settings and flags use the existing precedence

Add `history: bool` (default false), `history_dir: Option<PathBuf>`, `log_runs`, `history_runs`, and `history_bytes` to `Settings`, `SettingsOverrides`, and `EffectiveSettings`. Profile and workflow overrides use `Option<bool>` for `history` so an explicit `false` wins over a lower `true`. Resolve directories the way workdir and `spool_dir` already do: config paths from the config file's directory, `--history-dir` and `--log-dir` from the invocation directory.

CLI: `--history`, `--no-history` (they conflict), `--history-dir`, `--log-runs`, `--history-runs`, `--history-bytes`. `--no-history` forces off. Otherwise `--history` or `--history-dir` forces on. Otherwise the resolved setting applies. Numeric flags override the resolved setting. `list` and `validate` serialize the resolved values. Reject 0 for the three caps at validation. Do not add an upper bound on `history_bytes`.

### Optimizer fields live on the result, not a side channel

Extend `JobResult` with `blocked_ms` and `queued_ms`. Record command costs on a small struct the summary result and the history file both serialize: `kind` (`condition` or `command`), optional `step`, `wall_ms`, and the optional CPU and RSS fields. Leave live `job_finished` serialization unchanged by skipping the new fields there, the same way `started` is skipped today.

The scheduler records, per unit, when dependencies became satisfied and when the unit was launched, skipped, or cancelled. `blocked_ms` ends at dependency satisfaction or at an earlier skip or cancel. `queued_ms` is the time after that until `run_job` or until a later skip or cancel. `max_in_flight` is the high water mark of units inside `run_job`.

`graph_sha256` is SHA-256 (new `sha2` dependency) of compact JSON, keys in this order, nulls explicit:

```json
{"jobs":[{"name":"","command":null,"depends_on":[],"partitions":null,"timeout_secs":null,"shell":true,"condition":null,"steps":[{"id":"","command":""}]}]}
```

`condition` is null, or `{"kind":"expression"|"shell","text":"..."}`. The document covers the selected jobs after profile partition counts, not unselected jobs and not env values. Truncate history command and error strings to 1024 bytes on a char boundary. Leave the existing summary `command` and `error` strings alone so stdout JSON does not lose text.

Host memory is Linux `/proc/meminfo` `MemTotal` and `MemAvailable`, in bytes. Omit both when that file cannot be read. `logical_cpus` comes from `std::thread::available_parallelism`, or 1 when that fails.

### CPU from `wait4`, RSS from a Linux process-group sample

On Unix, `wait4` the spawned pid and store `ru_utime` and `ru_stime` as whole milliseconds. That rusage includes descendants the child waited for, which is the command's CPU for `sh -c`. On non-Unix, omit the CPU fields.

Peak RSS is Linux-only. The runner already puts the child in its own process group. While the command runs, sample the sum of `VmRSS` for every `/proc` pid whose process group is that child. Sample on each stdout or stderr read and at least every 200ms, and keep the maximum sum. Omit `peak_rss_bytes` on other platforms rather than report the direct child's `ru_maxrss`, which misses grandchildren and uses different units. A quiet command still gets the 200ms poll. A command shorter than the poll interval is sampled once before wait.

Alternatives considered: `ru_maxrss` of the direct child is portable and wrong for `cargo` and `mix`. A cgroup would measure the group exactly and is not something this CLI can require. A per-second trace would blow the 10 MB budget.

### The history file is not pretty-printed

Serialize with `serde_json` compact encoding, one trailing LF, atomic replace via the existing temp-file pattern. `schema_version` is the integer 1. Key order is the struct field order. Readers parse JSON; git sees a new file per run, so later runs do not rewrite earlier bytes.

README documents committing `goodflows-history/`, gitignoring the log directory, and a `.gitattributes` line `goodflows-history/** text eol=lf linguist-generated`. The CLI does not write either file.

## Risks / Trade-offs

- [Log paths move under a per-run directory] → Basenames stay the same. README calls out the break. No compatibility flag; `log_runs = 1` still uses a new directory.
- [RSS samples can miss a spike between polls, and can miss processes that exit before a sample] → The poll is 200ms and also runs on output. The field is a peak of samples, not a cgroup peak.
- [CPU and RSS can disagree about grandchildren] → CPU includes descendants the child reaped. RSS sums the group only while they are alive to be sampled.
- [The checkout cap does not shrink `.git`] → README says so. Prune only drops files from the next commit's tree.
- [Clock stepped backward] → The active stem is never pruned by the run that created it.
- [History write fails] → The run's exit code is unchanged and `history_file` is absent, so a full disk does not fail the checks.

## Migration Plan

No stored data to migrate. The first `--log-dir` run after the change creates a new directory beside the old flat files; those old files are not matching stems, so prune leaves them. Users delete the old files by hand. Rollback is reverting the binary. History files from `schema_version` 1 stay valid if a later version adds fields only in new files.

## Open Questions

None.
