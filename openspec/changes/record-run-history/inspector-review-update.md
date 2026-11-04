# Inspector Review-Update — record-run-history

**Reviewed:** 2026-11-04
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement

## Summary

The change gives each `--log-dir` run its own retained directory and adds opt-in, immutable per-run history files plus optimizer fields on the JSON `summary`. The biggest problem was in design: a `wait4` on a pid that tokio also reaps. Other gaps were concurrent runs pruning each other's live log directories, history fields left undefined (exit code, command, error, run id, host shape), and an outcome rule that broke in `--json` mode. All of these are patched. Two pool-related findings were withdrawn after `run-workflows-on-pools` was removed. `openspec validate record-run-history --strict` passes.

**Original counts:** Critical: 2 · Warning: 21 · Suggestion: 22
**Patches:** Auto-patched: 25 · User-guided: 0 · Model-recommended: 18 · Skipped: 0 · Withdrawn: 2

## Scope inspected

- Proposal: `openspec/changes/record-run-history/proposal.md`
- Design: `openspec/changes/record-run-history/design.md`
- Tasks: `openspec/changes/record-run-history/tasks.md`
- Deltas:
  - `openspec/changes/record-run-history/specs/run-history/spec.md`
  - `openspec/changes/record-run-history/specs/run-logs/spec.md`
  - `openspec/changes/record-run-history/specs/run-report/spec.md`
  - `openspec/changes/record-run-history/specs/job-partitions/spec.md`
- Canonical specs consulted:
  - `openspec/specs/run-report/spec.md`
  - `openspec/specs/job-partitions/spec.md`
  - `openspec/specs/settings-environment/spec.md`
- Other active changes consulted: `generalize-test-timings` (`run-workflows-on-pools` was consulted, then removed)

## Patches applied

25 findings were auto-patched, 0 were patched after user guidance, 18 were patched from model recommendations, and 0 were skipped. 2 pool-related findings were withdrawn when `run-workflows-on-pools` was removed.

### Auto-patched

1. **Summary scenario claimed run-level `blocked_ms`/`queued_ms`** (Critical): `specs/run-history/spec.md` "JSON summary without history" now says run-level `max_in_flight`, `parallelism`, and `graph_sha256`, with per-result `blocked_ms` and `queued_ms`.
2. **`log_dir` meaning change contradicted "keep meaning"**: `specs/run-report/spec.md:49` and the run-history summary requirement now carry an explicit `log_dir` exception.
3. **`kind` values undefined in spec**: run-history "History records command cost" now defines `condition` and `command`, and a "Step kinds" scenario was added.
4. **"Creating the directory SHALL fail the run"**: `specs/run-logs/spec.md` now reads "A failure to create the run directory…".
5. **Scenario title mismatch**: renamed "Profile isolates workflows" to "Workflow setting isolates workflows".
6. **"Flag wins" could not be checked**: the WHEN now states five runs.
7. **"Quiet platform" title**: renamed to "Platform without RSS".
8. **`max_in_flight` defined in the per-instance requirement**: moved to "History records the run".
9. **Oversized newest file had no scenario**: added "Oversized newest file is kept".
10. **Explicit `history = false` had no scenario, and `init` had no task**: added "Profile disables shared history" and put `init` in task 4.3.
11. **Report ordering not in requirement text**: `specs/run-report/spec.md:6` now puts the history path after the log line.
12. **Disk error scenario did not assert `history_file` absent**: the THEN was extended.
13. **10 MB vs 10 MiB**: `proposal.md:11` now reads 10 MiB (10485760 bytes).
14. **Stem notation written three ways**: now `YYYYMMDDTHHMMSSZ-<run-id>` throughout.
15. **History serialized `JobResult` tails and outputs**: design now specifies a separate history instance struct with an allowlist. Task 4.2's secret test covers tails and `GOODFLOWS_OUTPUT`.
16. **Outcome rule undefined in `--json` mode**: run-history now states the outcome rule explicitly (`src/output.rs:82-87` sets `INTERRUPTED` in text mode only). Timed-out and post-job-signal scenarios were added.
17. **Tests that hard-code flat log paths**: added task 2.5 (process, runner, scheduling, steps, output, and the `/dev/full` symlink test) and widened task 5.1.
18. **`reports upload` not covered**: run-history now forbids it from writing history and rejects the six flags there. Task 1.2 was updated (`src/main.rs:207-219`).
19. **`cargo test --lib` fails on a bin-only crate**: task 1.1 now uses `cargo test --bin goodflows config::`.
20. **`blocked_ms` start point undefined**: measured from the start of `runner::execute` to the moment the last dependency result is recorded.
21. **`summary_results` projection not named**: design and task 3.4 now name the `src/main.rs` projection and `#[serde(skip)]`.
22. **"CLI overrides applied in `main`" was inaccurate**: design now names `Settings::resolve` and both call sites. Task 1.2 matches.
23. **CPU lower bound and RSS group scope**: added to the run-history command-cost requirement, the design, and the README task 4.4.
24. **Stale README lines**: task 2.4 now rewrites README.md:679 and the `log_dir` meaning near :1141.
25. **Non-Unix CPU branch was moot, and "atomic replace" conflicted with immutability**: design now says Windows is unsupported and macOS omits RSS and memory. The write uses `write_atomic(path, bytes, false)` (`persist_noclobber`, `src/reporting/mod.rs:185`).

### Model-recommended patches

1. **`wait4` races tokio's reaper** (Critical): `design.md` gets a new "Reaping on Unix" section, and task 3.2 is updated.
   - **Chose:** On Unix, spawn with `std::process::Command`, wrap the pipes as tokio readers, reap via `spawn_blocking(wait4)`, and send `killpg`+`kill` on timeout or cancel. Never call tokio wait or kill on that pid.
   - **Rationale:** `src/runner.rs:832-955` reaps through tokio, so a second waiter causes ECHILD or pid-reuse kills. Owning the reap is the only race-free way to get per-command rusage.
2. **Concurrent runs prune each other's live log directories**: `specs/run-logs/spec.md` retention requirement, design, and task 2.2.
   - **Chose:** An exclusive `flock` on `<stem>/.goodflows.lock` held for the whole run. Prune skips locked directories, and an "Overlapping runs" scenario was added.
   - **Rationale:** Logs open lazily per job (`src/runner.rs:1015`), so deleting a live directory fails jobs and breaks "prune MUST NOT change the exit code". `libc` is already present.
3. **"No absolute paths" is impossible for error and command strings**: run-history "omits command output and secrets".
   - **Chose:** Redact a leading workdir or per-run log prefix in `error` to `<workdir>`/`<log>`. Store configured command text verbatim, truncated.
   - **Rationale:** Log-open errors embed absolute paths (`src/runner.rs:1015-1017`). Redacting goodflows-owned prefixes keeps the intent without rewriting user text.
4. **Per-instance `exit_code`, `command`, `error` missing**: run-history "records each instance".
   - **Chose:** Add all three (null when none), truncated, with a "Failed job" scenario.
   - **Rationale:** `proposal.md:12` promised exit and the truncated command. The truncation rule had no fields to apply to.
5. **`run_id`/`started_at` key names and not on `summary`**: run-history run and summary requirements, and task 3.4.
   - **Chose:** Keys `run_id` and `started_at` (RFC 3339 UTC to the second, equal to the stem), both on `summary` too.
   - **Rationale:** An agent can then correlate `summary`, `log_dir`, and the history file without parsing paths.
6. **`host` object shape unspecified**:
   - **Chose:** A `host` object with `logical_cpus` and optional `mem_total_bytes`/`mem_available_bytes`.
   - **Rationale:** It matches the "host object" wording already in tasks.md.
7. **Outcome could differ between history, summary, and exit code**:
   - **Chose:** Snapshot the signal state once, and let it drive history, `summary.outcome`, the header, and the exit code.
   - **Rationale:** Without it, a signal between the history write and summary emission would produce conflicting outcomes.
8. **`history_file` value form unspecified**:
   - **Chose:** The resolved path the run wrote, converted lossily, never written inside the file.
   - **Rationale:** This mirrors `log_dir` (`src/main.rs:364-367`).
9. **Two run UUIDs per invocation**: run-logs, design, and task 2.1.
   - **Chose:** Reuse the stem UUID as the reporting `workflow_run_id` when reporting is configured.
   - **Rationale:** `src/reporting/mod.rs:57` already mints one, and two unrelated ids would confuse correlation with uploaded timings.
10. **UTC formatting without a date crate**: design, proposal Impact, and task 2.1.
    - **Chose:** A hand-rolled days-to-civil helper with a known-epoch unit test.
    - **Rationale:** `Cargo.toml` has no `chrono` or `time`, and this keeps `sha2` as the only new dependency.
11. **RSS `/proc` scan per 4 KiB read is too costly, and short commands are flaky**: design and task 3.2.
    - **Chose:** Sample on output at most every 50ms, otherwise every 200ms, and omit the field when no live sample was taken. The test uses `sleep 0.3`.
    - **Rationale:** This bounds the scan cost, and the presence test becomes deterministic.
12. **Displayed `history_dir` contradicts `Config::load` resolution**: run-history inspection requirement and tasks 1.1/1.3.
    - **Chose:** Resolve in `Config::load` like `workdir`/`spool_dir` and display the absolute path (`/work/goodflows-history`).
    - **Rationale:** It matches how `workdir` is already shown (`src/config.rs:490-523`).
13. **Graph hash omitted step shell, step condition, and partition strategy**: run-history hash requirement, the design document, and task 3.3.
    - **Chose:** Include all three, hash `LegacyShell` the same as `Shell`, and add a "Step condition change" scenario.
    - **Rationale:** These inputs change what runs (`src/config.rs:360-375`), so leaving them out would make the hash lie.
14. **`commands` empty vs absent**:
    - **Chose:** Omit when empty. The skip scenario says "absent".
    - **Rationale:** It matches the existing `skip_serializing_if = "Vec::is_empty"` pattern.
15. **CLI value 0 for caps**: run-logs, run-history, and task 1.2.
    - **Chose:** `value_parser!(u64).range(1..)`, exit 2, with scenarios.
    - **Rationale:** It is the same pattern as `--budget` (`src/main.rs:94`).
16. **Whether `list`/`validate` honor the new flags**:
    - **Chose:** The flags are global and applied the same way `--parallelism` is. Neither command writes history.
    - **Rationale:** `--parallelism` already flows into `list`/`validate` settings (`src/main.rs:263`). This diverges from Specialist A's run-only suggestion so that it follows the codebase pattern.
17. **History for cancelled runs and pre-summary errors**: run-history "written once" requirement, design, and task 4.1.
    - **Chose:** Write history exactly when a `summary` is emitted, including cancelled runs, and never on a pre-summary error.
    - **Rationale:** This gives a single, testable trigger, and cancelled runs are still useful optimizer data.
18. **Same-second ordering of random UUIDs**: design.
    - **Chose:** Keep v4 and document that same-second prune order is arbitrary. UUIDv7 was not adopted.
    - **Rationale:** Item 9 reuses this id as the reporting `workflow_run_id`. Keeping v4 avoids a format change toward the server for a negligible ordering benefit.

### Skipped

_None._

### Withdrawn

1. **Pool flag handling and worst-case summary fixture**: these targeted `run-workflows-on-pools`, which was removed on 2026-11-04. There is no pool execution in `src/`, so nothing in this change needs to handle it.

## Critical

_None._

## Warning

_None._

## Suggestion

_None._

## Alignment notes

- **Other active changes:** `generalize-test-timings` keeps `workflow_run_id`, which is now shared with this change's run id, so there is no conflict.
- **Canonical specs:** The MODIFIED requirements in run-report and job-partitions keep the canonical text and scenarios, extended. The `log_dir` meaning change is now explicit.
- **Codebase assumptions verified:** tokio spawn and reap (`src/runner.rs:832-955`), `process_group(0)`, `write_atomic` with `persist_noclobber` (`src/reporting/mod.rs:185`), `workflow_run_id` (`src/reporting/mod.rs:57`), `Config::load` path resolution (`src/config.rs:485-525`), `Condition` variants and `Step` fields (`src/config.rs:360-375`), `Partitions { count, strategy }`, the `reports upload` flag rejection (`src/main.rs:207-219`), `Settings::resolve` call sites, `INTERRUPTED` set in text mode only, no date crate, `uuid` v4 only, and a bin-only crate.

## What looks good

- Pruning is by lexical name with the active stem protected, so it is robust to `git checkout` mtimes and clock steps.
- A new immutable file per run avoids rewriting bytes that git has already stored, and avoids branch conflicts.
- The optimizer fields are kept off live `job_finished` events, which preserves streaming consumers.
- The hash definition is explicit and canonical, so it excludes env values.
- The break for `--log-dir` consumers is called out, and old flat files are left in place.
