# Inspector Review-Update — profile-partition-counts

**Reviewed:** 2026-11-02
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement

## Summary

The change adds per-job partition counts to environment profiles and resolves them before scheduling or inspection. Two parallel specialists reviewed structure/consistency and codebase alignment/gaps. The patches clarify the selected-profile API, correct assumptions about TOML diagnostics, preserve parallelism precedence, and complete acceptance coverage. No unresolved findings remain; implementation tasks remain unchecked.

**Original counts:** Critical: 0 · Warning: 5 · Suggestion: 1
**Patches:** Auto-patched: 4 · User-guided: 0 · Model-recommended: 2 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/profile-partition-counts/proposal.md`
- Design: `openspec/changes/profile-partition-counts/design.md`
- Tasks: `openspec/changes/profile-partition-counts/tasks.md`
- Delta: `openspec/changes/profile-partition-counts/specs/job-partitions/spec.md` — three ADDED requirements for the map, replacement semantics, and name validation; one MODIFIED requirement for effective-count inspection. No REMOVED requirements.
- Canonical spec: `openspec/specs/job-partitions/spec.md`
- Other active changes consulted: `add-settings-env`, `improve-run-report`.
- Code inspected: `src/config.rs`, `src/main.rs`, `src/output.rs`, runner and reporting resolution paths, and inspection/workflow/runner/reporting tests.
- Git: target directory is untracked and has no recorded commits. Existing unrelated source, README, test, and canonical-spec edits were present before this review.

## Patches applied

4 findings were auto-patched. 0 were patched after user guidance. 2 were patched from model recommendations. 0 were skipped. References below use final artifact line numbers.

### Auto-patched

1. **Warning: Preserve existing parallelism precedence** — `openspec/changes/profile-partition-counts/specs/job-partitions/spec.md:25` → Qualified “Parallelism MUST stay unchanged” so only count replacement is prohibited from changing parallelism. Existing shared/workflow/profile/CLI precedence remains valid; `design.md:52` and `tasks.md:10` now include this explicitly and require combined-profile/CLI coverage.
2. **Warning: Specify aggregate inspection failure** — `openspec/changes/profile-partition-counts/specs/job-partitions/spec.md:62` → Added acceptance coverage for aggregate validation and workflow discovery when a selected profile encounters an unpartitioned same-named job. `tasks.md:12` now tests both commands and contextual errors. Evidence: both commands resolve each workflow through the same loop in `src/main.rs:233` and `src/main.rs:248`.
3. **Warning: Verify effective counts beyond scheduling** — `openspec/changes/profile-partition-counts/tasks.md:15` → Added profile-count regressions for JSON job/step identities, results, run totals, separate logs, saved reporting entries, report envelopes, and replay retaining the original count. `proposal.md:32` now identifies reporting test impact. Existing declared-count tests alone do not exercise profile resolution; reporting expands instances in `src/reporting/mod.rs:640`.
4. **Suggestion: Complete existing validation, isolation, and rollback checks** — `openspec/changes/profile-partition-counts/tasks.md:5` → Added explicit unselected negative-count, unknown-name, and never-partitioned-name checks; `tasks.md:6` now tests repeated resolution without mutating the document or sibling profiles; `tasks.md:13` now includes the already-designed older-binary rollback guidance. These checks reinforce existing contracts without adding product scope.

### Model-recommended patches

1. **Warning: Carry the full selected profile into resolution** — `openspec/changes/profile-partition-counts/design.md:46` → Specified that `Config::environment` returns the full `Environment`, while `Config::workflow` receives the selected profile name and resolves that profile. Updated `tasks.md:6` and `proposal.md:30` to include normal and aggregate/discovery callers in `src/main.rs`.
   - **Chose:** Pass the selected profile name into workflow resolution, look up the full profile there, and preserve unknown-profile errors and settings precedence.
   - **Rationale:** The existing lookup returns only settings (`src/config.rs:530`) and workflow resolution accepts only that settings reference (`src/config.rs:569`). Carrying profile identity makes both the sibling map and required contextual errors available without putting per-job data in run-wide settings.
2. **Warning: Match native TOML type diagnostics** — `openspec/changes/profile-partition-counts/design.md:42` → Removed the incorrect claim that ordinary integer-map parse errors include the complete key path. `specs/job-partitions/spec.md:6` and `specs/job-partitions/spec.md:20` now permit native source-location/entry/type diagnostics; `tasks.md:5` requires those assertions and keeps named context mandatory for semantic errors.
   - **Chose:** Retain `BTreeMap<String, i64>` and native TOML diagnostics; require explicit profile/job context for range and name errors.
   - **Rationale:** A probe against the repository's compiled TOML dependency using derived profile structs showed line/column, the offending entry, and `expected i64`, without the profile name. This choice matches the existing “No custom visitor” design and existing parse-error conventions while avoiding additional parser machinery.

### Skipped

_None._

## Critical

_None._

## Warning

_None._

## Suggestion

_None._

## Alignment notes

- **Other active changes:** `improve-run-report/specs/job-partitions/spec.md:5` changes “Output identifies each instance”; this delta modifies only inspection and adds profile requirements. New output checks explicitly preserve that change's text-report counting contract. `add-settings-env/design.md:19` adds settings maps; this change's partition map remains a sibling of settings, with shared textual overlap in configuration and README requiring normal integration.
- **Canonical specs:** Existing inspection scenarios are retained at `specs/job-partitions/spec.md:71` and `specs/job-partitions/spec.md:83`. Partition strategies, output restrictions, and instance identity contracts stay intact.
- **Codebase assumptions verified:** Profile lookup and workflow resolution currently pass only settings; all run/inspection paths use workflow resolution; list serializes normalized partition configuration; reporting creates manifest entries by iterating `job.partition_ids()`.
- **Validation:** `openspec validate profile-partition-counts --strict` passes after patches. Whitespace checks pass. Application tests were not run because this pass modifies planning artifacts only; the implementation's test commands remain tasks.
- **Scope:** Review edits are confined to `openspec/changes/profile-partition-counts/`. No source changes, commits, branch changes, or issue creation were performed by this review.

## What looks good

- Effective counts resolve once on cloned workflow jobs, avoiding separate scheduler or reporting branches.
- Global validation catches invalid unselected profiles while workflow-specific checks preserve reusable profile maps.
- Bare-run behavior, strategy, configured-job inspection counts, and existing event shapes remain compatible.
- Rollback removes the new profile table; persisted report identities retain the count from their original run.
