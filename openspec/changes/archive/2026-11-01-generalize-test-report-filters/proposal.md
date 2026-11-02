# Proposal

## Why

Collection rejects any suite filter tag outside five GoodCases categories before a snapshot is frozen. A report whose include or exclude uses another project's boolean tag never becomes replayable, even after the GoodFlows server accepts that tag.

## What Changes

- Accept a `suite_started` filter tag that is an identifier (1–128 bytes, `[A-Za-z0-9][A-Za-z0-9._-]*`) with a boolean value.
- Keep rejecting a non-identifier tag and a non-boolean value locally, with no snapshot.
- Mirror the server change's new cap of 256 entries on each of `filters.include` and `filters.exclude`, rejecting a longer list locally as `unsupported_filter` instead of freezing a report the updated server would permanently reject (422).
- Reject a present `filters` whose `include` or `exclude` is missing or not an array as `unsupported_filter`, so collection accepts and rejects the same `filters` shapes as the server's required lists (accept/reject parity; the CLI reports only the first failing code while the server collects every error), instead of freezing a report the server permanently rejects (422).
- Point the versioned contract copy (`src/reporting/contract.rs`) at the server change `generalize-test-report-filters`, remove the five-name `FILTER_TAGS` list and add the filter-list cap there. Move `valid_identifier` and its 128-byte limit (`MAX_IDENTIFIER_BYTES`) into that module and add `valid_filter_tag`, so the identifier grammar lives with the rest of the contract.
- Update the README so a new identifier tag is no longer the example of vocabulary this CLI refuses.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `test-timing-publishing`: local collection accepts boolean identifier filter tags and rejects the rest before freezing a snapshot.

## Impact

- `src/reporting/contract.rs` (`FILTER_TAGS`, `CONTRACT_VERSION`, new `MAX_FILTER_ENTRIES`, `MAX_IDENTIFIER_BYTES`, `valid_identifier` (moved from `collect.rs`) and `valid_filter_tag`, the module comment and `contract_is_versioned_and_matches_the_server_vocabulary`), `src/reporting/collect.rs` (the filter check in `validate_nested`, its comment, `filters_use_the_server_tag_allowlist`, renamed `filters_use_the_server_tag_grammar`, and `frozen_envelopes_are_revalidated`), `src/reporting/replay.rs` (calls `contract::valid_identifier`) and `tests/reporting.rs` (an end-to-end rejection test).
- `README.md` "Test timing reports" collection paragraphs and the replay rule (an older goodflows refuses a snapshot whose contents it does not accept), which currently say that a filter tag outside the server allowlist is rejected locally, call the tags a "filter-tag allowlist", and use "a new filter tag" as the example of vocabulary this CLI can lag.
- Depends on the server change `../goodflows_web/openspec/changes/generalize-test-report-filters/`. Reports that use only the five existing names, in lists of at most 256 entries, stay valid on both the current and the updated server. A tag such as `slow` is accepted here and is a permanent 422 on a server that still has the enum, so release this CLI after that server is deployed.
- GoodCases `GC.TestTimingFormatter` still drops tags outside its own `@categories`. This change does not edit that formatter. Until it records every boolean identifier filter, uploaded GoodCases reports stay on the five names.
