# Design

## Context

See proposal.md for why the closed tag list has to go. `validate_nested` in `src/reporting/collect.rs` rejects a filter entry unless `tag` is in `FILTER_TAGS` and `value` is a boolean. `FILTER_TAGS` (`chrome`, `goodaudit_integration`, `athena_preview`, `chromic_teardown`, `qpdf`) is defined in `src/reporting/contract.rs` and imported through `use super::contract::{self, *}`. The failure code is `unsupported_filter`, and it happens before a snapshot is frozen. There is no limit on the number of filter entries, and a missing or non-array `include`/`exclude` is skipped. `filters_use_the_server_tag_allowlist` iterates `FILTER_TAGS` and currently asserts that `slow` is rejected. `contract_is_versioned_and_matches_the_server_vocabulary` asserts both `CONTRACT_VERSION` and `FILTER_TAGS.len() == 5`. `valid_identifier` in the same file already matches the server's suite-id rule: 1–128 bytes, ASCII alphanumeric, then `.`, `_` or `-`. `CONTRACT_VERSION` is `2026-11-01-store-test-timings`.

The server change is `../goodflows_web/openspec/changes/generalize-test-report-filters/`. Besides replacing the enum with the identifier grammar, it caps each of `filters.include` and `filters.exclude` at 256 entries (422 `must not have more than 256 entries`).

## Goals / Non-Goals

**Goals:**

- Check filter tags with `valid_filter_tag` (the identifier grammar, now in `contract.rs`) and keep `value` boolean.
- Leave include and exclude order, repeats, and the `unsupported_filter` code in place.
- Stamp the local contract copy with the server change that removed the enum.
- Mirror that change's 256-entry cap per filter list.
- Reject a present `filters` whose `include` or `exclude` is missing or not an array, so collection accepts and rejects the same filters the server does (accept/reject parity, not code parity; see "Accept/reject parity, one code per file").

**Non-Goals:**

- Editing the GoodFlows server or `GC.TestTimingFormatter`.
- Accepting non-boolean ExUnit filter terms.
- Changing any other field allowlist, limit, or event name.
- Requiring `filters` itself. The server requires it, but collection does not check required fields in general, and a missing `filters` stays the server's 422.

## Decisions

### Call `valid_filter_tag` instead of `FILTER_TAGS`

Move `valid_identifier` from `collect.rs` into `contract.rs`, with its 128-byte limit as `pub const MAX_IDENTIFIER_BYTES: usize = 128;`, so every server grammar the CLI enforces lives in the versioned contract module. Add `valid_filter_tag`, a thin wrapper over `valid_identifier`, so the filter rule has its own name if the server ever splits the two grammars. `collect.rs` picks both up through its glob import, and `replay.rs` calls `contract::valid_identifier` for the manifest's report ids. In the filter loop in `validate_nested` (`collect.rs`), accept a string tag when `valid_filter_tag` is true and `value` is a boolean, and update the "Only allowlisted boolean tags" comment. Delete `FILTER_TAGS` from `contract.rs` and the `assert_eq!(FILTER_TAGS.len(), 5)` check. The test can no longer iterate `FILTER_TAGS`, so it lists the five names literally, and it is renamed `filters_use_the_server_tag_grammar`. A missing tag, a non-string tag, a non-object entry (a string, `null` or an array has no tag to read), a tag that fails `valid_filter_tag`, and a non-boolean value all stay `unsupported_filter`. An unknown key in an entry or in `filters` stays `unsupported_field`. In `validate_nested` the two codes are bound as `unsupported_field` and `unsupported_filter`.

`slow`, `chrome`, a 128-byte tag, `1x.a-b` and `a_b` pass. `slow test`, `ok?`, `slow` followed by a newline, an empty tag, a 129-byte tag, `.slow`, `_slow`, `-slow`, `café` and a fullwidth `ｓlow` do not. `valid_identifier` checks every byte, so unlike a regex with `$` it cannot accept a trailing newline. The five current names pass `valid_identifier`, so JSONL that already collects still collects.

Alternative: return `invalid_identifier` for a bad tag. Rejected. That code already means the suite or report id. One filter code keeps the diagnostic the tests and README already use.

Alternative: keep a list and append tags as projects need them. Rejected. That is the GoodCases leak this change removes.

### Cap each filter list at 256 entries

Add `pub const MAX_FILTER_ENTRIES: usize = 256;` to `contract.rs`. In `validate_nested`, an `include` or `exclude` array longer than that returns `unsupported_filter` before its entries are checked. Without this check, a report with 257 entries is frozen and then permanently rejected (422) by the updated server, so it is never replayable. The cap is new on the server too: the current server has none. Real ExUnit filter lists are a handful of entries, so reports that work today are unaffected.

### Require both filter lists when `filters` is present

The server's `suite_started` shape (`validator.ex`, checked by `shape.ex`) makes `filters` a required, non-nullable object whose `include` and `exclude` are both required lists: a missing key is 422 `is required`, a non-list is `must be an array`, and a non-object `filters` is `must be an object`. Collection used to skip a missing or non-array list, freezing a report the server permanently rejects. When `filters` is present, `validate_nested` now returns `unsupported_filter` unless `filters` is an object and `filters.include` and `filters.exclude` are both arrays. That covers a `null` or non-object `filters` too, since neither list can then be read. An absent `filters` is not checked, like every other required field (see Non-Goals).

### Accept/reject parity, one code per file

Collection matches the server on which filters are accepted and which are rejected. It does not match the server's error output: `validate_nested` stops at the first failure and returns one structural code, while the server collects every error for the record. Where one entry has several faults the CLI's code depends on check order, so the order is pinned by `filters_use_the_server_tag_grammar`: an unknown key in `filters` or in an entry (`{"tag":"slow","value":true,"note":"x"}`, `{"include":[],"exclude":[],"x":1}`) is `unsupported_field` before any list or tag check, then a missing or non-array list or a list over the cap is `unsupported_filter`, then each entry's tag and value.

### Set `CONTRACT_VERSION` to `generalize-test-report-filters`

The constant names the server contract this module was copied from. Update the module comment (which now also says the identifier grammar lives there), both the authoritative-change reference (`2026-11-01-store-test-timings` (design D1), now plus `generalize-test-report-filters`) and its example, and the version assertion in `contract_is_versioned_and_matches_the_server_vocabulary`. The server change is not archived yet, so it has no date prefix. The undated change id is used, and the doc comment on `CONTRACT_VERSION` says so. Renaming it is a pre-release step in the Migration Plan, not a task: it depends on when the server archives its change, and an unchecked task would block archiving this one. A closed filter-tag list is no longer the thing a newer server might accept first. The coupling remains for a new event or field: collection still rejects unknown vocabulary before a snapshot exists. The README's collection paragraph needs the same correction: a filter tag must be an identifier with a boolean value, with at most 256 entries per list. "filter-tag allowlists" becomes "field and event allowlists and the filter-tag grammar", and the "new filter tag" example becomes a new event or field.

## Risks / Trade-offs

- [This CLI uploads `slow` and an old server answers 422, which is recorded as rejected and is not replayed] → Release this CLI after `generalize-test-report-filters` is deployed on GoodFlows. Reports that use only the five names work against either server.
- [GoodCases still emits only `@categories`] → Uploads from that formatter do not change until its own change records every boolean identifier filter. This CLI is ready for that formatter and for any other producer.
- [`partitioned-report-collection` also edited `collect.rs`] → That change is archived (`2026-11-01-partitioned-report-collection`, commit `6e7137b` on this branch), and this change is built on top of it, so no rebase is needed. The filter check is independent of per-instance directories, and neither change alters the other's rule.
- [Snapshots are re-validated before every send, so changing the CLI changes what frozen snapshots can replay] → See "Frozen snapshots" in the Migration Plan.

## Migration Plan

**Release order is required:** this CLI MUST NOT ship until the GoodFlows `generalize-test-report-filters` change is deployed. The deployed server still has the five-tag enum and no list cap, so a report with any other tag is a permanent 422 there.

1. Deploy the GoodFlows server change. Existing reports remain valid.
2. **Before releasing,** check whether the server change has been archived. If it was archived under a dated id (for example `2026-11-0X-generalize-test-report-filters`), rename `CONTRACT_VERSION` in `src/reporting/contract.rs`, its assertion in `contract_is_versioned_and_matches_the_server_vocabulary`, and the README's "currently the GoodFlows change" name to that id. Then ship this CLI. Producers may then emit other identifier tags.
3. GoodCases removes `@categories` in `publish-test-timings`, after this CLI is the one collecting its reports.

**Frozen snapshots.** `reports upload` and every send re-validate a saved snapshot with `validate_envelope` (`Item::load` in `src/reporting/mod.rs`) and refuse a failing one as `invalid_snapshot` without sending it. The outcome for an already frozen snapshot therefore depends on the CLI that replays it, not the one that froze it:

- After upgrading, a snapshot an older CLI froze with `filters.include` or `filters.exclude` missing or not an array, or with either list over 256 entries, now fails with `unsupported_filter` and is refused as `invalid_snapshot` on every replay. The server would have answered 422 for it anyway, so nothing that could be accepted is lost. Its raw JSONL stays in `reports/`. A snapshot using only the five names in arrays of at most 256 entries still replays.
- After rolling back, a snapshot this CLI froze with any tag other than the five names (for example `slow`) is refused locally as `invalid_snapshot` by the older CLI on every replay and is never sent, even if the server would accept it. Replaying it requires this CLI or a newer one.

The README's rule, replay a run with the goodflows that wrote it or a newer one, covers snapshot contents as well as the manifest version.

Rollback is restoring `FILTER_TAGS` and the old `CONTRACT_VERSION`, removing `MAX_FILTER_ENTRIES`, `MAX_IDENTIFIER_BYTES` and `valid_filter_tag`, and moving `valid_identifier` back to `collect.rs`. JSONL that used only the five names still collects. Snapshots frozen with other tags then follow the rollback rule above.
