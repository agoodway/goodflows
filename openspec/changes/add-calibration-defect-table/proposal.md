# Proposal

## Why

`goodflows policy calibration` prints the server's report through the generic `key: value` dump (`src/output/change_requests.rs`, the `calibration` arm of `render`). The report is mostly counts, so the dump is readable but long, and it shows nothing that ties a count to a defect. goodflows_web's `add-calibration-defect-list` (agoodway/goodflows_web#31) adds a `defects` list with each defect's lane, policy version, rules and globs, plus `defects_truncated` and `unattributed_defect_count`. Dumped as `key: value` lines, 500 defect entries would be unreadable.

## What Changes

- The text output of `policy calibration` becomes a summary followed by a defect table: the window, one line per lane with its defect rate as a percentage, the per-version, per-rule and per-glob counts, gaps, then one line per defect, newest first.
- The output says when the server cut the list (`defects_truncated`) and how many unattributed defects were recorded in the window.
- Against a server without the defect list, the same summary prints, with no defect section.
- `--json` is unchanged: it already passes the report through, so it carries the new fields as soon as the server sends them.
- No new flags.

## Capabilities

### Modified Capabilities

- `review-calibration`: `policy calibration` prints the report as a summary and defect table instead of `key: value` lines.

## Impact

- Code: a `calibration` renderer in `src/output/change_requests.rs`, replacing the `dump` call for that event type. `src/change_requests/mod.rs` does not change.
- Tests: rendering unit tests in `src/output/change_requests.rs`.
- Docs: the README's "Change requests" section and the review-policy recipe of the `verify-goodflows-cli` skill.
- Depends on `spec-change-request-commands` being archived first, because this change modifies a requirement that change adds to `review-calibration`.
- The server side is goodflows_web#31. This change ships safely before it, because a report without `defects` prints the summary alone.
