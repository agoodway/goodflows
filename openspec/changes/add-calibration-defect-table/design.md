# Design

## Context

See proposal.md. `policy()` in `src/change_requests/mod.rs` emits `{"type": "calibration", "report": <data>}`, and `render` in `src/output/change_requests.rs` sends that event to `dump`. Other renderers in that file print one line per item with fields separated by two spaces (`review_policy_versions`, `project_events`) and use `s()` (null as `-`, escaped) and `list()` (`none` when empty).

## Goals / Non-Goals

**Goals:**
- A reader can go from a lane, rule or glob count to the defects behind it in the same output.
- The output works against servers with and without goodflows_web#31.

**Non-Goals:**
- Client-side filtering or sorting of defects (`--severity`, `--lane`). `--json` and `jq` cover that.
- Paging. The server caps the list at 500.
- Changing the JSON event.

## Decisions

### D1. Summary first, then the table

The counts answer "is the policy letting defects through?" and the table answers "which ones?". Printing the counts first keeps them visible when a long table scrolls.

### D2. Two-space separated lines, not padded columns

The other list renderers in `src/output/change_requests.rs` separate fields with two spaces and do not pad. Padding would need every row before printing and would break on wide globs. Every defect line has the same eight fields in the same order, with `-` for missing values, so `awk` and `cut` still work.

### D3. Rates as whole percentages

`defect_rate` is a fraction such as `0.15`. A whole percentage (`15%`) is easier to compare across lanes; the exact value stays in `--json`.

### D4. Feature-detect the new fields

The renderer prints the defect section only when `defects` is present and the unattributed line only when `unattributed_defect_count` is present. No server version check is needed, and the CLI can ship before goodflows_web#31.

## Risks / Trade-offs

- Text output scripts that parsed the old `key: value` dump break. The help already points scripts at `--json`, and the dump was never specified as stable before `spec-change-request-commands`.
