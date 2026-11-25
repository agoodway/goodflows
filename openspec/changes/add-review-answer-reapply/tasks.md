# Tasks

## 1. Packet rendering

- [ ] 1.1 Add an `untouched(item)` predicate in `src/output/change_requests.rs` per D2, exported for the command handler. Unit-test it with `item_paths_changed` `false`, `true`, null, a missing key, and `changed_since` null, and with `reapplicable` false. Verify with `cargo test output::change_requests`.
- [ ] 1.2 Extend `packet`: the summary lines after `## Items to answer`, the option and re-applied suffixes on `- answered:`, and the `- earlier answer:`, `- changed since:` and re-apply command or `- not re-applicable:` lines, with server text escaped. Add rendering tests for each scenario in the spec delta, including a packet without `earlier_answer` that renders byte for byte as before. Verify with `cargo test output::change_requests`.

## 2. cr reapply

- [ ] 2.1 Add `CrCommand::Reapply { pick, item_ids, untouched, head }` with clap rules for 1-200 ids or `--untouched`, the duplicate check (exit 2) and `after_help` examples; add `"reapply"` to the command label match. Verify `cargo build` and `goodflows cr reapply --help`.
- [ ] 2.2 Implement the named-ids path: default head from `ctx.show`, post `{head, item_ids}`, rewrite the stale-head `409` and the `item_ids[i]` `422`, emit `review_reapply`. Add mock server tests in `tests/change_requests.rs`: default head, explicit head (no `GET`), stale head, `422` naming the second id, merged `409` keeps the generic message, usage errors send nothing. Verify with `cargo test change_requests`.
- [ ] 2.3 Implement `--untouched`: read the packet, select with `untouched`, collect skipped, check `--head` and the 200 limit, post or exit 0 with nothing selected. Add mock server tests: mixed packet posts only `false` items and reports the rest skipped; nothing untouched sends only `GET`; head mismatch posts nothing; older server packet posts nothing. Verify with `cargo test change_requests`.
- [ ] 2.4 Render `review_reapply` (answers and skipped) and add it to `renders`. Add a rendering test for the text scenario. Verify with `cargo test`.
- [ ] 2.5 Add a `cr review --json` mock server test asserting `earlier_answer` and `reapplied_from` pass through unchanged.

## 3. Documentation

- [ ] 3.1 Document `cr reapply` and the new packet lines in README's change request section, add its usage errors and the moved-head conflict to `src/agents_guide.md`, and add re-apply steps (push, `cr review` shows the earlier answer, `cr reapply --untouched`, a touched item named by id) to the review-and-events recipe of the `verify-goodflows-cli` skill. Verify `cargo test` (guide and skills tests).

## 4. Integration

- [ ] 4.1 Run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`, and verify all pass.
- [ ] 4.2 Against a goodflows_web build with `add-review-answer-reapply`, answer two items, push a commit that touches one item's path, and verify `cr review` shows both earlier answers, `cr reapply --untouched` re-applies only the untouched one and skips the other, and `cr reapply <id>` re-applies the touched one.
