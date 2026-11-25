# Design

## Context

`CrCommand::Review` (`src/change_requests/mod.rs`) reads the packet and emits it; `packet` in `src/output/change_requests.rs` renders it, with one section per item, its `- answered:` line, `item_data`, and a `goodflows cr answer` command per allowed answer. `CrCommand::Answer` posts one answer, defaults its head from `ctx.show`, and rewrites a stale-head `409` with `explain_conflict`. `--json` prints the packet as the server sent it.

The server contract is goodflows_web `add-review-answer-reapply`: `earlier_answer` on packet items, `reapplied_from` on answers, and `POST .../review/reapply` with `{head, item_ids}`, all or nothing, `201` with the answers in `item_ids` order, `409` for a stale head or a merged or closed change request, and `422` keyed `item_ids[i]`.

## Goals / Non-Goals

**Goals:**
- A reviewer reading `cr review` sees, per item, the earlier answer, who gave it at which head, and whether the item's own paths changed since.
- One command re-applies one item, a named set, or every untouched item.
- The CLI never re-applies an item whose paths changed or are unknown unless the user names it.

**Non-Goals:**
- Changing the head-binding rule or re-applying `reject` or `request_change`.
- Splitting more than 200 items over several requests.
- Showing the full list of changed files in Markdown; `--json` has it.

## Decisions

### D1. A new `cr reapply` command, not a flag on `cr answer`

`cr answer` takes one item and an `ANSWER` positional and posts to the item's answers route. Re-applying takes no answer (the server copies it), takes many items, and posts to a different route with all-or-nothing semantics. A flag such as `cr answer <id> --reapply` would make `ANSWER` optional for one mode and still not cover batches.

### D2. Untouched means `item_paths_changed` is exactly `false`

The predicate is `reapplicable == true && changed_since.item_paths_changed == false`, matched against the JSON value, so a missing key, null `changed_since` (rewritten history) and null `item_paths_changed` (kinds without paths) all fall out. The packet renderer and `--untouched` share one function so the count `cr review` prints is the set `--untouched` posts. Consequence: items without paths, such as `question` or `one_way_door`, are never untouched and always need naming; the summary line counts them so the reviewer knows to look.

### D3. Named ids go straight to the server

`cr reapply <ids>` does not read the packet. The server already checks every id and answers `422` keyed by index, which the CLI maps back to ids. Reading the packet first would add a request and a second copy of the server's rules. Naming an item is the explicit choice the goal requires for touched or unknown items.

### D4. Heads follow `cr answer`, and `--untouched` uses the packet's head

Named ids default to `GET change-requests/<N>`'s head, as `cr answer` does. `--untouched` already read the packet, so it posts the packet's head: the selection was made at that head, and if the change request moved since, the server's `409` refuses the whole batch. A `--head` that differs from the packet's head fails locally, because the selection would describe a different head than the one posted.

### D5. Skipped items are part of the result

`--untouched` reports the re-applicable items it left out and why, in the event and the text. An agent can then show the user exactly which items need a decision, without reading the packet again. Items that are not re-applicable at all are not listed; `cr review` already says why.

### D6. Usage errors exit 2 before any request

Count, duplicate and either/or checks run in clap or right after parsing, before picking the change request, so they exit 2 and send nothing. This avoids repeating the `cr answer` behavior the spec-change-request-commands design listed as a bug (argument checks exit 1 after the branch lookup).

### D7. Markdown shows counts, not file lists

`changed since` prints commit and file counts and the item-path verdict. A long file list per item would bury the one fact the reviewer needs: whether this item's own paths moved. When they did, the line names the item's paths.

## Risks / Trade-offs

- [Two `409`s on one route] → the server's stale-head body is `{"errors": {"detail", "head"}}` and its merged or closed body is `{"errors": {"detail"}}`; the server spec pins that the latter has no `head`. The rewrite fires on `errors.head` without `errors.error`, the same check as `cr answer`, so both commands share one rule.
- [`answered_by` holds ids, not names] → the answer object carries `answered_by: {user_id, key_id}`, so the earlier answer line shows `user_id`; a readable name needs a server field.
- [A packet with more than 200 untouched items] → `--untouched` refuses and asks for batches by id; splitting would break the all-or-nothing guarantee the reviewer relies on.

## Migration Plan

Additive. Against an older server `cr review` prints what it prints today, `cr reapply --untouched` posts nothing, and `cr reapply <ids>` fails with `404`. The CLI can ship before or after goodflows_web `add-review-answer-reapply`.
