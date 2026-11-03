# Inspector Review-Update — improve-run-report

**Reviewed:** 2026-11-02
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement

## Summary

The change replaces the closing text summary with a configured-job report while preserving JSON instance counts. Two specialists reviewed structure/consistency and codebase alignment/gaps; their duplicate findings were merged, and a final consistency pass clarified partition breakdown semantics. All six findings have been patched in the change artifacts, with no application source edits or unresolved decisions.

**Original counts:** Critical: 1 · Warning: 3 · Suggestion: 2
**Patches:** Auto-patched: 3 · User-guided: 0 · Model-recommended: 3 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/improve-run-report/proposal.md`
- Design: `openspec/changes/improve-run-report/design.md`
- Tasks: `openspec/changes/improve-run-report/tasks.md`
- Deltas: `openspec/changes/improve-run-report/specs/run-report/spec.md` (ADDED: closing report, rows/counts, failure details, rerun hint, live failure trimming and summary metadata); `openspec/changes/improve-run-report/specs/job-partitions/spec.md` (MODIFIED: Output identifies each instance).
- Canonical specs consulted: `openspec/specs/job-partitions/spec.md`; `run-report` is new and has no canonical spec.
- Other active changes consulted: none; this is the only unarchived change.
- Code/test context: `src/main.rs`, `src/output.rs`, `src/runner.rs`, `src/config.rs`, README and relevant output/reporting/scheduling tests. The target directory was untracked before review, with no path-specific commit history.

## Patches applied

3 findings were auto-patched. 0 were patched after user guidance. 3 were patched from model recommendations. 0 were skipped. References below identify current lines; original locations are included where useful.

### Auto-patched

1. **Warning — Failure context can be absent** — `openspec/changes/improve-run-report/specs/run-report/spec.md:73` (original line 57), `openspec/changes/improve-run-report/design.md:36`, `openspec/changes/improve-run-report/tasks.md:14` → Require an actual failed/timed-out step before naming one, show the failure status, and omit absent command/error/exit-code fields. Added pre-step/post-step failure and timeout coverage; clarified job-level tails and bounded live-block assertions in task 3.1. Verified preparation and output-evaluation failures at `src/runner.rs:444` and `src/runner.rs:585`, and flattened step status at `src/runner.rs:113`.
2. **Suggestion — Example contradicts its own report rules** — `openspec/changes/improve-run-report/design.md:49` (original lines 43–57) → Changed subsecond wall time to `710ms`, added the missing passing `lint` row, and included both failed partition entries with explicit statuses. Counts now agree with the displayed rows.
3. **Suggestion — Compatibility note has no implementation task** — `openspec/changes/improve-run-report/tasks.md:23`, `openspec/changes/improve-run-report/design.md:83` → Added task 3.3 for the text-output compatibility note and machine-output guidance. The repository has no existing changelog file, so the task uses the README and next release description.

### Model-recommended patches

1. **Critical — Start state cannot be inferred from duration** — `openspec/changes/improve-run-report/design.md:31` (original line 29), `openspec/changes/improve-run-report/specs/run-report/spec.md:32`, `openspec/changes/improve-run-report/specs/run-report/spec.md:118`, `openspec/changes/improve-run-report/tasks.md:7` → Specify an internal `started` marker projected only into summary results; propagate the additive field and runner impact into the proposal.
   - **Chose:** Define started as entry into `run_job`; omit row duration only when no instance started, retaining valid `0ms` and condition-skip durations. Keep live event shapes and reporting's separate `executed` semantics.
   - **Rationale:** Millisecond durations can be zero for started jobs, and cancellation/skipped status cannot identify launch state. `executed` tracks main/step command spawning and is skipped by serialization (`src/runner.rs:59`, `src/runner.rs:434`, `src/runner.rs:600`), so a distinct summary-only marker supplies the exact information the stateless renderer requires.
2. **Warning — Rerun hint does not preserve all valid names** — `openspec/changes/improve-run-report/design.md:43` (original line 39), `openspec/changes/improve-run-report/specs/run-report/spec.md:92`, `openspec/changes/improve-run-report/tasks.md:15` → Specify embedded-apostrophe escaping, equals-form option values, leading-hyphen workflow placement after `--`, and a comma-name fallback. Updated the proposal and integration assertion syntax.
   - **Chose:** Generate a shell-safe and parser-safe selective hint where possible; explain comma-name limitations and offer a full-workflow rerun preserving workflow/environment when selection cannot round-trip.
   - **Rationale:** Shell quoting alone cannot prevent Clap's comma splitting (`src/main.rs:45`), and valid names can contain apostrophes, leading hyphens and commas (`src/config.rs:403`, `src/config.rs:652`). A clearly explained full-workflow command remains usable without expanding scope into name validation or CLI grammar changes; it can rerun passing jobs as well.
3. **Warning — Partition breakdown and skip-reason rules are underspecified** — `openspec/changes/improve-run-report/specs/run-report/spec.md:47` (original line 39), `openspec/changes/improve-run-report/design.md:29`, `openspec/changes/improve-run-report/tasks.md:11` → Define which counts appear, prohibit unpartitioned breakdowns, and add passed+skipped and differing-skip-reason scenarios.
   - **Chose:** Show all nonzero non-passed partition statuses in severity order, with passed counts implicit; list distinct nonempty skip reasons in instance order for skipped rows.
   - **Rationale:** This matches the proposal's rollup intent and existing `2/3 failed` example while making all-skipped and passed+skipped output deterministic. Preserving distinct reasons avoids silently discarding existing result context.

### Skipped

_None._

## Critical

_None._

## Warning

_None._

## Suggestion

_None._

## Alignment notes

- **Other active changes:** None; no competing capability edits.
- **Canonical specs:** `openspec/specs/job-partitions/spec.md:123` requires instance-identifying output and instance counts. The replacement requirement at `openspec/changes/improve-run-report/specs/job-partitions/spec.md:5` preserves live instance identity, JSON counts and `run_started` counts while explicitly assigning configured-job counts to the text report.
- **Codebase assumptions verified:** The current writer is stateless (`src/output.rs:126`), workflow/environment are attached before rendering (`src/output.rs:78`), reporting finishes before summary (`src/main.rs:330`), scheduler results preserve unit order (`src/runner.rs:258`), and CLI selections include prerequisite jobs (`src/config.rs:597`). Effective log-directory rules distinguish legacy and named workflows (`src/main.rs:304`); tasks now test both.
- **Accepted limits retained:** The advisory hint omits `--config`, parallelism and fail-fast flags, as explicitly scoped in `openspec/changes/improve-run-report/design.md:19` and `openspec/changes/improve-run-report/design.md:78`. Runner errors or output suppression can prevent a final report, as recorded at `openspec/changes/improve-run-report/design.md:76`.
- **Verification:** `openspec validate improve-run-report --strict --no-interactive` passed after the artifact updates. Application tests were not run because this pass changes planning artifacts only; implementation and its checks remain unchecked tasks.

## What looks good

- Text counts and JSON instance counts are explicitly distinguished without removing existing JSON fields.
- The report reuses ordered summary results, keeping rendering stateless.
- Outcome precedence, individual partition failures and reporting-before-summary ordering are covered by requirements and implementation tasks.
- No migrations, dependencies, commits or branch changes are introduced by this review.
