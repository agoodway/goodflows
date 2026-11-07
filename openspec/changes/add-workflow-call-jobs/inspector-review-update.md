# Inspector Review-Update — add-workflow-call-jobs

**Reviewed:** 2026-11-06
**Reviewer:** inspector-review-update (manual fallback; no inspector tool or skill was available)
**Mode:** auto
**Verdict:** Ready to implement, with recorded product constraints
**Base:** `43e525a349e15b1f4258377f4a539040aba8f428` (local `main` and `origin/main`)

## Summary

The change adds same-file `workflow = "name"` jobs, ordered only by ordinary `depends_on`, with independent child invocations under one scheduler. The plan is feasible on the current runner, config, history, and report code. The review patched places where the artifacts contradicted that code or left an implementer free to invent a second settings or partition model. No feature was added. Workflow-level prerequisites stay dropped.

**Original counts:** Critical: 0 · Warning: 7 · Suggestion: 3
**Patches:** Auto-patched: 7 · User-guided: 0 · Model-recommended: 3 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/add-workflow-call-jobs/proposal.md`
- Design: `openspec/changes/add-workflow-call-jobs/design.md`
- Tasks: `openspec/changes/add-workflow-call-jobs/tasks.md`
- Deltas: `specs/workflow-calls/spec.md`, `specs/run-history/spec.md`, `specs/run-report/spec.md`
- Canonical specs: `openspec/specs/run-history/spec.md`, `openspec/specs/run-report/spec.md`, `openspec/specs/job-partitions/spec.md`, `openspec/specs/settings-environment/spec.md`, `openspec/specs/run-logs/spec.md`
- Code: `src/config.rs` (`Job`, `validate`, `workflow`, `select`, `SettingsOverrides::apply`), `src/main.rs`, `src/runner.rs` (`execute`, `RssMonitor`), `src/output.rs`, `src/output/report.rs`, `src/history.rs` (`graph_sha256`, `Record`/`Instance`), `src/reporting/mod.rs`, `src/retention.rs`
- Tests consulted: `tests/output.rs` exact event key lists, `tests/workflows.rs` profile partition cases
- Other active changes: none. Archived changes were not treated as competing requirements.
- Git: the change directory is untracked. Pre-existing source and canonical specs were not edited.

## Patches applied

7 findings were auto-patched. 0 were patched after user guidance. 3 were patched from a model recommendation. 0 were skipped. Citations below refer to the revised files.

### Auto-patched

1. **Warning: Caller settings isolation contradicted per-key merge** — `design.md` decision 2, `specs/workflow-calls/spec.md` "Child settings are resolved independently", `proposal.md`, tasks 1.3, 1.4, and 2.4. `SettingsOverrides::apply` (`src/config.rs`) merges shared settings with the target workflow per key. An omitted child key inherits the shared value. Copying the caller's resolved settings, or treating an omitted child key as empty, would change that contract. The artifacts now require shared inheritance and forbid copying caller-only overrides.
2. **Warning: Profile partitions are global by job name** — same design section and workflow-calls spec, plus tasks 1.3 and 2.4. `Config::validate` accepts a profile entry when any workflow partitions that job name, and `Config::workflow` fails when the selected workflow has an unpartitioned job of that name (`src/config.rs`, confirmed by `tests/workflows.rs`). Reachable-call checks must reuse that rule. A per-workflow partition namespace was not added.
3. **Warning: Live text does not print `workflow`** — `design.md` decision 6, workflow-calls "Live text distinguishes concurrent invocations", task 4.2. `src/output.rs` `instance()` prints the job name and optional partition only. JSON `workflow` on the event does not make concurrent same-named child jobs distinguishable in text. Call-aware text must add the owning workflow name and a short invocation distinguisher. No-call labels stay as they are. `list` text must show the call target; today it prints `command`, which a call does not have.
4. **Warning: Graph hash field order is the compatibility contract** — `design.md` decision 7, `specs/run-history/spec.md` "Graph hash ignores environment values", task 4.4. `history::graph_sha256` serde-serializes a fixed field list, including null `command` and empty `steps`. The call target is now an optional object omitted for ordinary jobs, appended without reordering or dropping existing fields. A false call condition still fingerprints the resolved target and creates no child.
5. **Warning: Call outcome must use the existing signal snapshot** — `design.md` decision 4, workflow-calls "A call completes with its child outcome", task 2.5. `history::outcome` cancels a run when the snapshotted signal is set even if every instance passed (`src/history.rs`, `src/main.rs`). Mapping only child instance statuses would let a call pass after SIGTERM. The call now uses that same rule, and the root keeps 130/143.
6. **Warning: Self-call was an unspecified cycle** — proposal, `design.md` decision 2, workflow-calls validation, task 1.2. A job with `workflow` equal to its own workflow is a direct cycle and is rejected with the same diagnostic. No recursion depth or re-entry mode was added.
7. **Warning: Duration and queue clocks were underspecified against current types** — `design.md` decision 4, run-history cost requirement, run-report call rows, tasks 2.3 and 2.5. `JobResult.duration_ms` is `u128` milliseconds, and text rendering reads it with `as_u64` (`src/output.rs`, `src/output/report.rs`). Call results retain integer durations. The review initially proposed root-relative wait metrics; the user-approved integration correction below replaces that proposal with invocation-relative metrics, matching the existing per-execution contract. A call-condition shell is one temporary slot, released before the child starts, not a second permanent instance. A stepped job remains one slot.

### Model-recommended patches

1. **Suggestion: One RSS sampler for the tree** — proposal impact, `design.md` decision 3 and risks, workflow-calls capacity requirement, task 2.3. `RssMonitor::start` spawns one Linux `/proc` sampler per `execute`. Nested samplers would multiply scans. The plan now keeps one sampler and still attributes peaks only to the invocation that spawned the process. Wrapper history does not copy child peaks.
2. **Suggestion: Child history must not copy root `--jobs`** — `specs/run-history/spec.md` "History links workflow-call invocations", task 4.4. `HistoryTarget.requested_jobs` is the CLI `--jobs` list (`src/main.rs`). A child always runs its complete graph, so copying that list would record a selection the child did not make. Child records omit `requested_jobs`.
3. **Suggestion: No-call event key lists are asserted exactly** — `design.md` risk, task 4.1. `tests/output.rs` compares `job_started`, `step_started`, `step_finished`, and `job_output` key vectors. Invocation fields are attached only when the selected reachable graph contains a call, so those assertions stay valid.

### Skipped

_None._

### User-approved integration corrections

After inspecting the returned review, the source thread proposed two corrections. The user approved importing the reviewed plan with both corrections; they are included in these local artifacts.

1. **Preserve the complete graph-hash definition** — `specs/run-history/spec.md` "Graph hash ignores environment values" retains the canonical field list and shell-condition normalization rather than referring to fields "as specified." A MODIFIED requirement replaces the original at archive time, so that reference would discard the actual definition. The separate call-object requirement and no-call byte compatibility remain intact.
2. **Measure wait metrics within each invocation** — `design.md` decision 4, both affected delta specs, and task 2.5 now start child dependency wait at the child's own execution start. Parent preparation and call-condition time cannot become child `blocked_ms` or `queued_ms`. An immediately ready child job has dependency wait near zero; its subsequent capacity wait is recorded separately. The parent call retains parent-relative wait metrics. This follows `runner::execute` and `waits` using a start instant for the owning execution, rather than extending the root's clock to children.

## Critical

_None._

## Warning

_None remaining. The seven warnings above were patched._

## Suggestion

_None remaining. The three suggestions above were patched._

## Recorded product constraints

These are constraints the reviewed artifacts state, not new features or outstanding implementation blockers. The user approved applying the reviewed plan with the two integration corrections above; no alternative model is introduced here.

1. **Profile partition maps stay global by job name.** A reusable workflow that partitions `test` shares a selected profile's `test` count with every other workflow that partitions `test`. A reachable workflow whose `test` is not partitioned fails that profile before any root job runs, even if the call condition is false. Per-workflow partition keys would be a new feature and were not added.
2. **A workflow cannot call itself.** That is a direct cycle. Bounded re-entry was not designed.
3. **Live text gains a short invocation distinguisher only when calls are present.** The exact width is an implementation detail (the first 8 hex characters of the invocation id is enough). No-call text is unchanged. A richer label syntax was not specified.
4. **Child history omits `requested_jobs`.** Parent/caller/child UUIDs are the linkage. Storing the root `--jobs` list on the child was rejected as misleading, not as a product expansion.

## Alignment notes

- **Canonical specs:** The run-history delta keeps schema version 1 and the no-call canonical bytes. The run-report delta keeps root-last text output, local configured-job counts, and the existing rerun quoting/comma fallback. Job-partition identity, slot accounting, and profile replacement stay in force; calls do not partition. Settings-environment per-key merge and reserved-variable scrubbing stay in force.
- **Code assumptions checked:** `Config::workflow` strips other workflows and applies one profile; `Config::select` is local; `runner::execute` admits work only up to `parallelism` and records skips before launch; `Output` clones share one writer; reporting sessions are per invocation and keyed by run id; log run directories are unique stems under the workflow log namespace; history writes are allowlisted. Those match the design's reuse plan.
- **Not expanded:** No workflow inputs/outputs, cross-workflow `needs`, call env, call partitions, call timeout, workflow-level prerequisites, deduplication, or imports.
- **Verification:** The Grok orb reported `openspec validate add-workflow-call-jobs --strict` passing and `git diff --check` clean after its review. Source-thread integration restores the complete MODIFIED hash definition, keeping the call object in a separate requirement, and adds invocation-relative wait scenarios. Strict validation and whitespace checks are rerun on the integrated artifacts. Application tests are not needed for these planning-only edits; implementation verification remains in the task list.

## What looks good

- Ordering is only `depends_on`. Definition order creates no edge, and repeated calls are separate executions.
- Waiting calls do not hold worker slots, which is the deadlock the current `JoinSet` limit would otherwise create.
- Root `--jobs` does not leak into the child, and child job names stay local.
- No-call event shapes, log layouts, report envelopes, and graph hashes are explicitly preserved.
- Fail-fast and signals have an ancestor rule instead of a new child process model.
