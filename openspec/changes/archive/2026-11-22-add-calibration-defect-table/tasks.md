# Tasks

## 0. Order

- [x] 0.1 Archive `spec-change-request-commands` first, so `openspec/specs/review-calibration/spec.md` holds the requirement this change modifies

## 1. Renderer

- [x] 1.1 Add a `calibration` renderer in `src/output/change_requests.rs` for the window, lane lines (rate as a rounded percentage, `-` when null), policy versions, rules, gaps, globs and `unmatched`, and route the `calibration` event to it instead of `dump`; verify a unit test with rates `0.15`, `0.3333333333333333` and null, gaps `[46, 51]` and one rule and glob, asserting the exact lines
- [x] 1.2 Print `Defects:` lines in the server's order with `-` for null values and empty lists, `Defects: none` for an empty list, the truncation line when `defects_truncated` is true, and the unattributed line when `unattributed_defect_count` is present; verify unit tests for a full entry with two rules, a public-key entry with null ticket and severity, a truncated list and an empty list
- [x] 1.3 Verify a unit test that a report without `defects` or `unattributed_defect_count` prints the summary and no `Defects` or `Unattributed` line, and a test that a ticket with ESC prints escaped

## 2. Docs

- [x] 2.1 Describe the text output in the README's "Change requests" section
- [x] 2.2 Moved to #55: add a calibration step to `.agents/skills/verify-goodflows-cli/features/review-policy.md` once goodflows_web#31 is deployed

## 3. Finish

- [x] 3.1 Run `cargo test --locked`, `cargo clippy --locked --all-targets -- -D warnings` and `npx @fission-ai/openspec validate add-calibration-defect-table --strict`
