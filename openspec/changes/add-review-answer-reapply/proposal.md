# Proposal

## Why

Review answers bind to the head. After a push, every item in `goodflows cr review` is unanswered again, even when its id and content did not change, and the reviewer types each `goodflows cr answer` again. That rule stays: every push is reviewed in full. The goodflows_web change `add-review-answer-reapply` makes the second answer cheap instead. Each packet item gains `earlier_answer`, the latest answer to the same item id at another head, with whether it can be re-applied and what changed since (`changed_since`), and a new route, `POST .../change-requests/<N>/review/reapply`, records the earlier answers of the named items at the current head in one all-or-nothing request. Answers gain `reapplied_from`.

The CLI prints the packet with a fixed renderer, so it shows none of this today, and it has no command for the new route. `--json` already passes the packet through as the server sends it.

## What Changes

- `cr review` Markdown shows, per item with an `earlier_answer`, who gave which answer at which head (with the option and note), what changed since that head (commits, files, and whether the item's own paths changed), and either a `goodflows cr reapply` command or why the answer cannot be re-applied. An answer that was re-applied says so on its `- answered:` line.
- Below `## Items to answer`, a summary line counts the items whose earlier answer can be re-applied and whose item paths did not change ("untouched"), with one command that re-applies them all.
- New command `goodflows cr reapply`:
  - `goodflows cr reapply <ITEM_ID>...` re-applies the earlier answers of the named items, 1 to 200 distinct ids. Naming an item is how a reviewer re-applies one whose paths changed, whose kind has no paths, or whose history was rewritten.
  - `goodflows cr reapply --untouched` reads the packet and re-applies every item with `reapplicable` true and `item_paths_changed` exactly `false`. It never picks an item with `item_paths_changed` true or null; it lists those as skipped with the reason.
  - `--head SHA` works as in `cr answer`. Without it, named items use the change request's head and `--untouched` uses the packet's head.
  - One request, one `review_reapply` event, all or nothing. A moved head is rewritten into a next step, as `cr answer` does.
- `cr review --json` keeps passing the packet through, now with `earlier_answer` and `reapplied_from`. A requirement and test lock that in.
- `cr answer` does not change; its text output already dumps every answer field, including `reapplied_from`.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `change-request-review`: the packet Markdown shows earlier answers, what changed since, and re-apply commands; the `- answered:` line names a re-applied answer's source head; new `cr reapply` command with its item selection, head, request, conflict message and output.

## Impact

- `src/change_requests/mod.rs`: a `CrCommand::Reapply` variant (clap) and handler, and its `"reapply"` command label.
- `src/output/change_requests.rs`: the `packet` renderer gains the earlier answer, changed-since and summary lines; a `review_reapply` renderer; `renders` lists `review_reapply`.
- Tests: rendering unit tests in `src/output/change_requests.rs`; mock server tests in `tests/change_requests.rs`.
- Docs: README "Change requests" section, `src/agents_guide.md` (usage errors and the conflict table), `cr reapply --help` examples, and the review-and-events recipe of the `verify-goodflows-cli` skill.
- Compatibility: against a server without `add-review-answer-reapply`, `cr review` prints what it prints today (no `earlier_answer`), `cr reapply <ids>` fails with the generic `404` message, and `cr reapply --untouched` finds nothing and posts nothing. So the CLI can ship before or after the server.
- Out of scope: changing the head-binding rule; re-applying `reject` or `request_change`; batching more than 200 items; re-applying from a head other than the one the server picked as `earlier_answer`.
