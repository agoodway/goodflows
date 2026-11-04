# Inspector Review-Update — generalize-test-timings

**Reviewed:** 2026-11-04
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement

## Summary

The change replaces `exunit-timings` with a runner-neutral `test-timings/1` contract, an ExUnit reporter and a Jest reporter, and mirror releases. After this pass the delta removes the canonical filter requirement that would have rejected Jest's `filters: null`, and the tasks no longer contradict the code they have to change. Nothing is left unresolved.

**Original counts:** Critical: 1 · Warning: 9 · Suggestion: 1
**Patches:** Auto-patched: 8 · User-guided: 0 · Model-recommended: 3 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/generalize-test-timings/proposal.md`
- Design: `openspec/changes/generalize-test-timings/design.md`
- Tasks: `openspec/changes/generalize-test-timings/tasks.md`
- Deltas:
  - `openspec/changes/generalize-test-timings/specs/test-timing-contract/spec.md`
  - `openspec/changes/generalize-test-timings/specs/test-timing-publishing/spec.md`
  - `openspec/changes/generalize-test-timings/specs/test-reporters/spec.md`
- Canonical specs consulted:
  - `openspec/specs/test-timing-publishing/spec.md`
- Other active changes consulted: `record-run-history`, `run-workflows-on-pools` (neither touches these capabilities)

## Patches applied

8 findings were auto-patched. 0 were patched after user guidance. 3 were patched from model recommendations. 0 were skipped.

### Auto-patched

1. **Filter requirement was not removed** — `openspec/changes/generalize-test-timings/specs/test-timing-publishing/spec.md:90` → Added `REMOVED` for "Suite filter tags are boolean identifiers". Ported the canonical non-null scenarios into the contract spec (repeated tags, case, bad tag, non-boolean value, 257 entries, non-object). `filters: null` stays accepted. Omitting `filters` is a missing core key.
2. **Stable codes were not pinned** — `openspec/changes/generalize-test-timings/specs/test-timing-publishing/spec.md:87` → The string `duration_us` scenario now requires `invalid_field`. Added `missing_field` and duplicate-start `unpaired_event` scenarios. Filter shape failures are `unsupported_filter`, not `invalid_field`.
3. **Retired-format grep contradicted the rejection tests** — `openspec/changes/generalize-test-timings/tasks.md:18` → Tasks 2.4 and 3.2 no longer require `exunit-timings` to be absent. It may remain only in tests and messages that reject the retired format.
4. **Release tag was not part of the version check** — `openspec/changes/generalize-test-timings/specs/test-reporters/spec.md:98` → A tag, when the ref is a tag, must match the CLI and reporter versions. Task 7.1 compares the tag only when one is present.
5. **Envelope identity check still reads `run_id`** — `openspec/changes/generalize-test-timings/tasks.md:11` → Task 2.2 now changes `validate_envelope` so `report.suite_id` matches the first record's `suite_id`. `assign_partition` stays limited to partition fields.
6. **Closed code list dropped codes collection already returns** — `openspec/changes/generalize-test-timings/specs/test-timing-publishing/spec.md:93` → Added `duplicate_key`, `nul_character`, `non_integer_number`, `integer_out_of_range`, `nested_too_deeply`, `malformed_record`, `invalid_utf8`, `empty_report` and `missing_suite_started`. Limit codes stay `too_large`, `record_limit` and `request_too_large`.
7. **Config format constant is an alias** — `openspec/changes/generalize-test-timings/design.md:109` → D6 and task 3.1 now say to replace the alias with `"test-timings"` and leave `contract::REPORT_FORMAT` as `"test-timings/1"`.
8. **Interruption events had no core-key set** — `openspec/changes/generalize-test-timings/specs/test-timing-contract/spec.md:47` → `interrupted` and `max_failures_reached` carry only the common keys and optional `meta`. An extra `reason` key is rejected.

### Model-recommended patches

1. **Reporter behavior lived only in D7** — `openspec/changes/generalize-test-timings/specs/test-reporters/spec.md:75`
   - **Chose:** Copy D7 into the reporters spec, and make design outcome mapping match the ExUnit requirement.
   - **Rationale:** The tasks test the spec scenarios, so a rule that exists only in the design is not enforced. Jest group outcomes, a single `max_failures_reached`, core `line: null` with `meta.line`, the ExUnit and Jest `meta` keys, ignoring the fixture exit status, and the Jest min/latest matrix are now requirements. Design now says `excluded` becomes skipped and `invalid` becomes errored.
2. **ExUnit suite option name did not match the formatter being ported** — `openspec/changes/generalize-test-timings/design.md:115`
   - **Chose:** `:test_timing_suite_id` and `TEST_TIMING_SUITE_ID`, ported from goodcases `app/test/support/test_timing_formatter.ex`.
   - **Rationale:** That file uses `:test_timing_run_id` and `TEST_TIMING_RUN_ID` beside `:test_timing_directory`. Keeping the `test_timing_*` option namespace matches the source. The spec already required `TEST_TIMING_SUITE_ID`. The port must not copy `@identifier` as a module attribute, because OTP 28 rejects a compiled regex stored there. Jest's option is `suiteId`.
3. **Reporters CI job did not pin Elixir or OTP** — `openspec/changes/generalize-test-timings/tasks.md:36`
   - **Chose:** A separate `reporters` job on `ubuntu-24.04`, with `erlef/setup-beam` pinned to Elixir `1.18.6` and OTP `27`.
   - **Rationale:** `setup-beam` requires both versions, and Elixir 1.18 supports OTP 25–27. The review recommended `1.18.4`. That patch is inside the built-in JSON decoder advisory fixed in `1.18.6`, so the pin is the oldest patched 1.18 on a supported OTP. The job stays out of the `check` matrix and does not use goodcases' Elixir 1.20 or OTP 28.

## Critical

_None._

## Warning

_None._

## Suggestion

_None._

## Alignment notes

- **Other active changes:** No conflict. `record-run-history` modifies `run-report`, `run-logs`, `run-history` and `job-partitions`. `run-workflows-on-pools` adds `pool-workflow-execution` and does not change reporting.
- **Canonical specs:** The publishing delta's three `MODIFIED` requirements keep the canonical scenarios and update the format name. The filter requirement is now `REMOVED`, so archive will not keep a rule that rejects `filters: null`.
- **Codebase assumptions verified:** `config.rs` aliases `contract::REPORT_FORMAT`. `validate_envelope` compares `report.suite_id` to `first["run_id"]`. `assign_partition` rewrites only partition fields. The nine encoding codes added to the closed set are returned by `collect.rs` today. `ci.yml` has one `check` job and Node 24, and does not install Elixir. `release.yml` has `build` and tag-only `publish`. The goodcases formatter is 386 lines, uses Jason, and is not in this repo.

## What looks good

- The break from `exunit-timings` to `test-timings/1` is explicit, including replay of old snapshots as `invalid_snapshot`.
- The contract spec is written so a server change can port its scenarios, and goodci's collection codes stay in the publishing delta.
- `meta` is flat and bounded, and pairing ignores it, so runner-specific fields cannot fork the core record.
- Each task group carries its own check, and the drift guard runs the real CLI over the reporters instead of a golden file.
- Mirror pushes fail closed on version drift or a rewritten mirror history, and they do not force-push.
