# Inspector Review-Update — generalize-test-report-filters

**Reviewed:** 2026-11-01
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement (release after the server change is deployed)

## Summary

The change replaces the CLI's five-name filter tag allowlist with the identifier grammar that the companion server change adopts. Its intent is sound. It placed `FILTER_TAGS` in `collect.rs`, but the constant lives in `src/reporting/contract.rs`. It missed the server change's new 256-entry cap on each filter list, so an over-cap report would be frozen and then permanently rejected with a 422. It also gave a `cargo test` command with two positional filters, which cargo rejects. All findings are patched.

**Original counts:** Critical: 1 · Warning: 4 · Suggestion: 2
**Patches:** Auto-patched: 5 · User-guided: 0 · Model-recommended: 1 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/generalize-test-report-filters/proposal.md`
- Design: `openspec/changes/generalize-test-report-filters/design.md`
- Tasks: `openspec/changes/generalize-test-report-filters/tasks.md`
- Deltas: `openspec/changes/generalize-test-report-filters/specs/test-timing-publishing/spec.md` (ADDED)
- Canonical specs consulted: `openspec/specs/test-timing-publishing/spec.md`
- Archived changes consulted: `2026-11-01-add-job-partitions`, `2026-11-01-partitioned-report-collection` (archived in commit `6e7137b` on this branch). Server: `../goodflows_web/openspec/changes/generalize-test-report-filters/`. GoodCases: `../goodcases/openspec/changes/publish-test-timings/`.
- Code verified: `src/reporting/contract.rs`, `src/reporting/collect.rs` (`validate_nested`, `valid_identifier`, `filters_use_the_server_tag_allowlist`), `README.md` "Test timing reports" collection paragraphs.

## Patches applied

5 findings were auto-patched and 1 was patched from a model recommendation. None were skipped.

### Auto-patched

1. **(Critical) Server's 256-entry filter-list cap was not mirrored.** `proposal.md`, `design.md` (new decision "Cap each filter list at 256 entries"), spec, tasks 1.1 to 1.3. The server change caps `filters.include` and `filters.exclude` at 256 entries (server `proposal.md:10`, `design.md:33-35`). The CLI has no cap, so a 257-entry report would be frozen and then rejected with a permanent 422. Added `MAX_FILTER_ENTRIES = 256` in `contract.rs` and local rejection as `unsupported_filter`, with a "Filter list over the cap" scenario and test cases for 256 and 257 entries.
2. **`FILTER_TAGS` location.** `design.md:5,27`, `tasks.md:6`. The constant is defined in `src/reporting/contract.rs:120` and glob-imported into `collect.rs`. The filter check is in `validate_nested`. Tasks and design now delete it from `contract.rs`.
3. **Contract test assertions.** `tasks.md` 1.2, `design.md`. `contract_is_versioned_and_matches_the_server_vocabulary` asserts the old `CONTRACT_VERSION` string as well as `FILTER_TAGS.len()`. The task now updates the version assertion and replaces the length assertion with `MAX_FILTER_ENTRIES == 256`. `filters_use_the_server_tag_allowlist` iterates `FILTER_TAGS`, so it must list the five names literally.
4. **Invalid cargo invocation.** `tasks.md` 1.2. `cargo test --locked a b` rejects the second positional filter. Changed to `cargo test --locked -- a b`.
5. **README and comment coverage.** `tasks.md` 1.3, `proposal.md` Impact, `design.md`. The README's contract paragraph also says "filter-tag allowlists". It is now covered, along with the `rg` check. The module comment's authoritative-change reference and the "Only allowlisted boolean tags" comment in `validate_nested` are listed for update. A `slow` + newline case was added to mirror the server scenario, since `valid_identifier` rejects it bytewise.

### Model-recommended patches

1. **Undated `CONTRACT_VERSION` value.** `design.md` "Set `CONTRACT_VERSION`".
   - **Chose:** keep `generalize-test-report-filters` (undated) and note that it can be renamed when the server archives the change.
   - **Rationale:** the existing value follows the server's dated archive name, but the server change is not archived yet and its date is unknown. Inventing a date would be wrong, and the proposal already names this value.

### Skipped

_None._

## Critical

_None._

## Warning

_None._

## Suggestion

1. **Non-array or missing `include`/`exclude`.** _Resolved._ `src/reporting/collect.rs` `validate_nested` used `filters[list].as_array().into_iter().flatten()`, which treated a non-array list as empty, so it passed locally while the server's `{:list, ..}` shape rejects it. It was first recorded as a non-goal, then brought into scope: see the `design.md` decision "Require both filter lists when `filters` is present". `validate_nested` now returns `unsupported_filter` unless `filters` is an object whose `include` and `exclude` are both arrays, and `filters_use_the_server_tag_grammar` covers each shape.
2. **Ordering note.** The undated contract-version string means a follow-up edit when the server archives its change. This is cosmetic.

## Alignment notes

- **Archived changes:** `partitioned-report-collection` is archived (commit `6e7137b` on this branch) and this change is built on it, so no rebase is needed. It edited `collect_dir`/`envelope`/`JobContext` in `collect.rs`, not `validate_nested`, so there is no rule overlap. Neither change touches the other's spec requirements. The ADDED requirement here is a new name in `test-timing-publishing`.
- **Canonical specs:** the canonical "Report processing is bounded and structural" requirement mentions a "timing-only field contract". This ADDED requirement narrows the filter part of it without contradicting it.
- **Server/consumer:** the grammar (1-128 bytes, `[A-Za-z0-9][A-Za-z0-9._-]*`, boolean value, order preserved, 256 cap) now matches `../goodflows_web/openspec/changes/generalize-test-report-filters/specs/test-report-storage/spec.md:27`. GoodCases `publish-test-timings` (task 1.2) already removes `@categories` and references this change.
- **Codebase assumptions verified:** `valid_identifier` matches the grammar. `unsupported_filter` is the existing code. `CONTRACT_VERSION` is `2026-11-01-store-test-timings`. The README phrases "server allowlist" and "new filter tag" existed at review time; the implementation removed both.

## What looks good

- It reuses `valid_identifier` and the existing `unsupported_filter` code.
- The release ordering against the server is stated clearly.
- Reports that use only the five names remain valid on both servers.
