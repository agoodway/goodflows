# Design

## Context

See proposal.md for why this exists. Today `Options.log_dir` is the workflow log directory (`--log-dir` plus `path_component` for a named workflow, or `--log-dir` itself for legacy top-level jobs). `run_job` creates `<index>-<name>[.pNN].stdout.log` and `.stderr.log` with `File::create`, so the next run truncates them. `duration_ms` starts when `run_job` starts. The scheduler in `runner::execute` is the only place that knows a job was waiting on dependencies or on a free slot. `summary` is built in `src/main.rs` after reporting and rendered by `src/output/report.rs`. Settings already merge shared, workflow, and profile values; CLI overrides for parallelism and fail-fast are arguments to `Settings::resolve`, called from the per-workflow path and the aggregate `list`/`validate` path in `src/main.rs`. Commands are spawned with `tokio::process::Command` (`kill_on_drop`, `process_group(0)`) and reaped through tokio `Child::wait`. `libc` is a Unix-only dependency and is already used without a cfg guard, so Windows is not a supported target. Reporting already creates a `workflow_run_id` UUID. There is no hash crate and no date crate.

## Goals / Non-Goals

**Goals:** One run id shared by the log directory and the history file. Prune by filename so mtime and git checkout do not choose the victim. Publish optimizer fields from the same structs the history file serializes. Measure CPU and peak RSS without a new sampler thread when output is flowing, and with a slow poll when it is not.

**Non-Goals:** See proposal.md. Also out of scope: editing the user's `.gitignore` or `.gitattributes`, restoring the old flat log paths, and hashing environment values.

## Decisions

### One run identity, lexical names

Create one lowercase v4 UUID and one UTC timestamp, truncated to a second, at the start of a run. The UUID is also the reporting `workflow_run_id` when reporting is configured, so an invocation has one run id. Format the timestamp from `SystemTime::now()` since `UNIX_EPOCH` with a small, unit-tested days-to-civil helper rather than a new crate (for example 2026-11-04T00:00:00Z → `20261104T000000Z`); `started_at` is the same instant in RFC 3339. Two runs in the same second sort by their random UUID, so their relative prune order is arbitrary. Format the directory and file stem as `YYYYMMDDTHHMMSSZ-<uuid>`. Logs go in `<effective log dir>/<stem>/`. History goes in `<history dir>/<path_component(workflow)>/<stem>.json`, and legacy top-level jobs use the workflow name `default` only for history. Log placement otherwise stays as it is today, so a legacy config does not gain a `default/` log subdirectory.

Prune compares those names as strings. Never delete the stem just created, even if the clock steps backward and an older name sorts newer. Never delete a name that does not match the stem pattern, and never follow or unlink a symlink. Create the log run directory before jobs; if that fails, the run fails as it does today. Each run then takes an exclusive `flock` on `<stem>/.goodflows.lock` and holds it until exit. Prune logs immediately after that, skipping any directory whose lock cannot be taken with `LOCK_NB`, so a concurrent run sharing `--log-dir` never loses its live directory and a crash loop still stays within the cap. Snapshot the signal state once when the summary fields are finalized; that snapshot drives the outcome in the history file, `summary`, the report header, and the exit code, in text and JSON mode alike. Write and prune history after that snapshot and before the summary is emitted, so `history_file` is present only when the write succeeded. History is written exactly when a summary is emitted, including cancelled runs; a run that errors before a summary (for example `runner::execute` returning `Err`) writes none. History and log prune errors are reported on stderr and do not change the exit code.

Alternatives considered: pruning by mtime is wrong after `git checkout`. One rewritten `history.jsonl` conflicts on every branch and rewrites bytes git already stored. A timestamp alone collides when two runs start in the same second.

### Settings and flags use the existing precedence

Add `history: bool` (default false), `history_dir: Option<PathBuf>`, `log_runs`, `history_runs`, and `history_bytes` to `Settings`, `SettingsOverrides`, and `EffectiveSettings`. Extend `Settings::resolve` with a CLI-overrides struct carrying the history and retention flags and update both call sites. Profile and workflow overrides use `Option<bool>` for `history` so an explicit `false` wins over a lower `true`. Resolve directories the way workdir and `spool_dir` already do: config `history_dir` (shared, workflow, and profile) is joined to the config file's directory inside `Config::load`, and the default `goodflows-history` resolves the same way; `--history-dir` and `--log-dir` resolve from the invocation directory. `list` and `validate` therefore show the resolved path, as they already do for `workdir`.

CLI: `--history`, `--no-history` (they conflict), `--history-dir`, `--log-runs`, `--history-runs`, `--history-bytes`. `--no-history` forces off. Otherwise `--history` or `--history-dir` forces on. Otherwise the resolved setting applies. Numeric flags override the resolved setting. The flags are global like `--parallelism`, so `list` and `validate` serialize the resolved values with CLI overrides applied; they never write history. `reports upload` rejects all six flags alongside its existing run-only flag list. Reject 0 for the three caps at config validation, and at parse time with `value_parser!(u64).range(1..)` (exit 2), the same pattern as `--budget`. Do not add an upper bound on `history_bytes`.

### Optimizer fields live on the result, not a side channel

Extend `JobResult` with `blocked_ms`, `queued_ms`, and `commands`, all `#[serde(skip)]` like `started`. The `summary_results` projection in `src/main.rs`, which already re-adds `started` by hand, adds them to each summary result. Record command costs on a small struct the summary result and the history file both serialize: `kind` (`condition` for a job or step shell condition, `command` for a job or step command), optional `step`, `wall_ms`, and the optional CPU and RSS fields; `commands` is omitted when empty.

The history file does not serialize `JobResult`, because it carries `stdout_tail`, `stderr_tail`, and job and step `outputs`. It uses a separate history instance struct with an explicit allowlist: `name`, `partition`, `status`, `started`, `exit_code`, `command` (truncated), `error` (truncated and prefix-redacted), `blocked_ms`, `queued_ms`, `duration_ms`, `commands`. In `error`, a leading workdir or per-run log directory prefix becomes `<workdir>` or `<log>`; configured command text is stored verbatim.

The scheduler records, per unit, measured from the start of `runner::execute`, when dependencies became satisfied (the moment the last dependency's result is recorded) and when the unit was launched, skipped, or cancelled. `blocked_ms` ends at dependency satisfaction or at an earlier skip or cancel. `queued_ms` is the time after that until `run_job` or until a later skip or cancel. `max_in_flight` is the high water mark of units inside `run_job`.

`graph_sha256` is SHA-256 (new `sha2` dependency) of compact JSON, keys in this order, nulls explicit:

```json
{"jobs":[{"name":"","command":null,"depends_on":[],"partitions":null,"timeout_secs":null,"shell":true,"condition":null,"steps":[{"id":"","command":"","shell":null,"condition":null}]}]}
```

`partitions` is null or `{"count":N,"strategy":"index"}`. `condition` (job or step) is null, or `{"kind":"expression"|"shell","text":"..."}`; `Condition::LegacyShell` and `Condition::Shell` both hash as `shell`. The document covers the selected jobs after profile partition counts, not unselected jobs and not env values. Truncate history command and error strings to 1024 bytes on a char boundary. Leave the existing summary `command` and `error` strings alone so stdout JSON does not lose text.

Host memory is Linux `/proc/meminfo` `MemTotal` and `MemAvailable`, in bytes. Omit both when that file cannot be read. `logical_cpus` comes from `std::thread::available_parallelism`, or 1 when that fails.

### CPU from `wait4`, RSS from a Linux process-group sample

On Unix, `wait4` the spawned pid and store `ru_utime` and `ru_stime` as whole milliseconds. That rusage includes descendants the child waited for, which is the command's CPU for `sh -c`. For a `timed_out` or `cancelled` command the group is killed before `sh` reaps its children, so CPU is a lower bound. macOS records CPU and omits `peak_rss_bytes` and the host memory fields.

#### Reaping on Unix

Tokio reaps a `tokio::process::Child` itself, so a second `wait4` on the same pid would race it (ECHILD, or a later kill of a reused pid). On Unix, goodflows owns the reap instead: spawn with `std::process::Command` plus `process_group(0)`, wrap the stdout and stderr fds as tokio pipe readers (`tokio::net::unix::pipe::Receiver::from_owned_fd`), and wait in `spawn_blocking(wait4(pid, &status, 0, &rusage))`, using that future in the existing select. On timeout or cancel, `killpg` and then `kill(pid)`; the blocking wait then returns. Build the exit status with `ExitStatusExt::from_raw`. Never call tokio `wait`, `try_wait`, or `kill` on that pid.

Peak RSS is Linux-only. The runner already puts the child in its own process group. While the command runs, sample the sum of `VmRSS` for every `/proc` pid whose process group is that child. Sample on a stdout or stderr read only when at least 50ms have passed since the last sample, and otherwise every 200ms, and keep the maximum sum; a full `/proc` scan per 4 KiB read would be too costly for chatty commands. Omit `peak_rss_bytes` on other platforms rather than report the direct child's `ru_maxrss`, which misses grandchildren and uses different units. A quiet command still gets the 200ms poll. A command that exits before any live sample leaves only a zombie, so `peak_rss_bytes` is omitted when no sample saw a live process. Descendants that leave the group with `setsid` or `setpgid` are not counted.

Alternatives considered: `ru_maxrss` of the direct child is portable and wrong for `cargo` and `mix`. A cgroup would measure the group exactly and is not something this CLI can require. A per-second trace would blow the 10 MB budget.

### The history file is not pretty-printed

Serialize with `serde_json` compact encoding, one trailing LF, atomic create via `reporting::write_atomic(path, bytes, false)` (`persist_noclobber`), so a name collision fails rather than overwrites. `schema_version` is the integer 1. Key order is the struct field order. Readers parse JSON; git sees a new file per run, so later runs do not rewrite earlier bytes.

README documents committing `goodflows-history/`, gitignoring the log directory, and a `.gitattributes` line `goodflows-history/** text eol=lf linguist-generated`. The CLI does not write either file.

## Risks / Trade-offs

- [Log paths move under a per-run directory] → Basenames stay the same. README calls out the break. No compatibility flag; `log_runs = 1` still uses a new directory.
- [RSS samples can miss a spike between polls, and can miss processes that exit before a sample] → The poll is 200ms and also runs on output. The field is a peak of samples, not a cgroup peak.
- [CPU and RSS can disagree about grandchildren] → CPU includes descendants the child reaped. RSS sums the group only while they are alive to be sampled.
- [The checkout cap does not shrink `.git`] → README says so. Prune only drops files from the next commit's tree.
- [Clock stepped backward] → The active stem is never pruned by the run that created it.
- [History write fails] → The run's exit code is unchanged and `history_file` is absent, so a full disk does not fail the checks.
- [Concurrent runs share `--log-dir`] → Live directories are locked and skipped by prune, so the cap can be exceeded until the overlapping run exits.

## Migration Plan

No stored data to migrate. The first `--log-dir` run after the change creates a new directory beside the old flat files; those old files are not matching stems, so prune leaves them. Users delete the old files by hand. Rollback is reverting the binary. History files from `schema_version` 1 stay valid if a later version adds fields only in new files.

## Open Questions

None.
