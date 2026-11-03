# Inspector Review-Update — add-settings-env

**Reviewed:** 2026-11-02
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement

## Summary

The change adds settings environment defaults with per-key inheritance, full source-map validation, inspection support, and shared Elixir initializer defaults. Two specialists reviewed structure/consistency and codebase alignment/gaps. All three findings were patched within this change directory; no unresolved findings remain. This verdict concerns the planning artifacts, not implementation completion.

**Original counts:** Critical: 0 · Warning: 2 · Suggestion: 1
**Patches:** Auto-patched: 2 · User-guided: 0 · Model-recommended: 1 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/add-settings-env/proposal.md`
- Design: `openspec/changes/add-settings-env/design.md`
- Tasks: `openspec/changes/add-settings-env/tasks.md`
- Delta: `openspec/changes/add-settings-env/specs/settings-environment/spec.md` — six ADDED requirements covering accepted maps, inheritance, execution, validation, inspection, and initialization; no MODIFIED or REMOVED requirements.
- Canonical specs consulted: `openspec/specs/job-partitions/spec.md`, `openspec/specs/test-timing-publishing/spec.md`. No canonical `settings-environment` spec exists yet.
- Other active changes consulted: `profile-partition-counts`, `run-workflows-on-pools`, `improve-run-report` (proposals and relevant deltas).
- Codebase: `src/config.rs`, `src/main.rs`, `src/runner.rs`, `src/init.rs`, `src/reporting/mod.rs`, `README.md`, and relevant integration tests.
- Git: target directory is untracked and has no history. Pre-existing source, README, and canonical-spec edits were left untouched.

## Patches applied

2 findings were auto-patched. 0 were patched after user guidance. 1 was patched from a model recommendation. 0 were skipped. Artifact citations below refer to the revised files.

### Auto-patched

1. **Warning: Explicit profile identity protections** — `openspec/changes/add-settings-env/tasks.md:6`, `openspec/changes/add-settings-env/tasks.md:14`, `openspec/changes/add-settings-env/design.md:31`, `openspec/changes/add-settings-env/design.md:35`, `openspec/changes/add-settings-env/specs/settings-environment/spec.md:66`, and `openspec/changes/add-settings-env/specs/settings-environment/spec.md:85` → explicitly require settings-scope rejection of `GOODFLOWS_ENVIRONMENT` and regression coverage for selected-profile injection and inherited identity removal without profile selection. Verified against existing rejection in `src/config.rs:738` and profile application in `src/runner.rs:417`.
2. **Suggestion: Publisher endpoint-source regression** — `openspec/changes/add-settings-env/tasks.md:14` and `openspec/changes/add-settings-env/specs/settings-environment/spec.md:70` → add coverage proving commands receive settings-defined endpoint values while the publisher reads only the caller environment, including an absent caller endpoint. This makes the existing design promise at `openspec/changes/add-settings-env/design.md:31` directly testable; current credential sourcing is in `src/reporting/mod.rs:371`.

### Model-recommended patches

1. **Warning: Diagnostic privacy scope** — `openspec/changes/add-settings-env/specs/settings-environment/spec.md:75`, `openspec/changes/add-settings-env/design.md:35`, and `openspec/changes/add-settings-env/tasks.md:6` → distinguish semantic env validation errors from TOML syntax/type errors.
   - **Chose:** Require semantic validation errors to identify the settings scope without configured values; retain existing TOML parser diagnostics, which may include source excerpts and values.
   - **Rationale:** `Config::load` forwards TOML deserialization errors (`src/config.rs:460`), so reusing env validation cannot provide a parser-wide privacy guarantee. Preserving existing syntax/type diagnostics matches job/step behavior and keeps this change within its stated validation scope.

### Skipped

_None._

## Critical

_None._

## Warning

_None._

## Suggestion

_None._

## Alignment notes

- **Other active changes:** No conflicting requirements found. `profile-partition-counts` adds a sibling profile partition map (`openspec/changes/profile-partition-counts/proposal.md:9`); it does not replace settings resolution. Remote execution preserves settings env and delegates resolution to the member (`openspec/changes/run-workflows-on-pools/specs/pool-workflow-execution/spec.md`, requirements “The member resolves configuration and tools” and “Remote environment and reporting remain explicit”). Run-report changes concern summary rendering rather than settings inheritance (`openspec/changes/improve-run-report/proposal.md:9`). These changes share implementation files and must retain each other's additions when implemented.
- **Canonical specs:** The new capability preserves partition identity authority and publisher credential isolation (`openspec/specs/job-partitions/spec.md:43`, `openspec/specs/test-timing-publishing/spec.md:157`). A new capability is appropriate; existing requirements need no replacement.
- **Codebase assumptions verified:** Settings override application, workflow-before-profile ordering, original-config validation, effective-settings serialization, runner env assembly/scrubbing, command-owned output paths, publisher process-env sourcing, and per-job Elixir `MIX_ENV` insertion all match current source.
- **Verification:** `openspec validate add-settings-env --strict` passes after patches. Application tests were not run because this review changes planning artifacts only; implementation verification remains in tasks 4.1–4.2.

## What looks good

- Precedence and empty-string behavior are explicit and consistent across artifacts.
- Validating authored maps prevents invalid unselected or overridden entries from escaping checks.
- Configured defaults stay separate from explicit job overrides and the ambient environment in inspection.
- Migration and binary rollback implications are documented.
