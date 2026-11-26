---
format: goodflows-change/1
id: add-review-answer-reapply
status: proposed
size: small
door: two_way
capabilities: [change-request-review]
depends_on: []
goals:
  - {id: G1, text: "A reviewer reading `cr review` after a push sees, per item, the earlier answer, who gave it at which head, and whether the item's own paths changed since."}
  - {id: G2, text: "Answering again after a push costs one command for the items whose own paths did not change."}
  - {id: G3, text: "The CLI ships before or after the goodflows_web change without breaking against either server."}
non_goals:
  - {id: N1, text: "Changing the rule that answers bind to the head; every push is still reviewed in full."}
  - {id: N2, text: "Re-applying `reject` or `request_change`, or re-applying from a head other than the one the server picked as `earlier_answer`."}
  - {id: N3, text: "Splitting more than 200 items over several requests."}
  - {id: N4, text: "Listing the changed files in Markdown; `--json` has them."}
  - {id: N5, text: "Changing `cr answer`; its text output already prints every answer field, `reapplied_from` included."}
anti_goals:
  - {id: A1, text: "A batch that stores some answers and not others."}
  - {id: A2, text: "An untouched count in `cr review` that differs from what `cr reapply --untouched` posts for the same packet."}
decisions:
  - id: U1
    question: What can a reviewer re-apply?
    answer: "One item's earlier answer, named by id, and, in one command, every re-applicable item whose `item_paths_changed` is false."
    by: user
    why: Re-answering unchanged items after every push is the cost being removed.
  - id: U2
    question: May the CLI pick an item whose own paths changed, or whose path change is unknown?
    answer: "No. Items whose `item_paths_changed` is true or null are re-applied only when the user names them."
    by: user
    why: Those items need a human to read them again.
  - id: U3
    question: What does `cr review --json` do with the new fields?
    answer: "Passes `earlier_answer` and `reapplied_from` through as the server sends them."
    by: user
    why: Agents read the JSON packet, not the Markdown.
  - id: D1
    question: Is re-applying a new command or a flag on `cr answer`?
    answer: "A new command, `goodflows cr reapply`."
    by: agent
    why: "Re-applying takes no answer (the server copies it), takes many items and posts to another route with all-or-nothing semantics; `cr answer` takes one item and a required answer."
    rejected:
      - "`cr answer <id> --reapply`: makes `ANSWER` optional for one mode and still does not cover batches."
  - id: D2
    question: Exactly which items are untouched?
    answer: "Those with `reapplicable` true and `changed_since.item_paths_changed` equal to the JSON value false. A missing key, a null `changed_since` (rewritten history) and a null `item_paths_changed` (kinds without paths, such as `question` or `one_way_door`) are not untouched, and the summary counts them so the reviewer knows to read them. `cr review` and `--untouched` apply the same rule."
    by: agent
    why: "U2 forbids picking unknowns; matching the exact value keeps every unknown shape out."
    rejected:
      - "Treat a null `item_paths_changed` as untouched because the kind has no paths: re-applies answers nobody reread."
  - id: D3
    question: Does `cr reapply <ids>` check the ids against the packet first?
    answer: "No. Named ids go straight to the server, whose `422` is keyed by index and mapped back to ids."
    by: agent
    why: "Naming an item is the explicit choice U2 asks for; reading the packet first adds a request and a second copy of the server's rules."
    rejected:
      - "Read the packet and refuse ids that are not re-applicable before posting."
  - id: D4
    question: Which head does `cr reapply` post?
    answer: "Named ids use `--head`, else the change request's head, as `cr answer` does. `--untouched` posts the packet's head, and a `--head` that differs from it fails before posting."
    by: agent
    why: "The `--untouched` selection was made at the packet's head; if the change request moved since, the server's `409` refuses the whole batch."
    rejected:
      - "`--untouched` posts the change request's current head: the selection would describe a different head than the one posted."
  - id: D5
    question: Does the result list the items `--untouched` left out?
    answer: "Yes, each re-applicable item left out, with its reason, in the event and the text. Items that are not re-applicable at all are not listed; `cr review` already says why."
    by: agent
    why: An agent can show the user which items need a decision without reading the packet again.
    rejected:
      - Report only the answers stored.
  - id: D6
    question: When are argument errors caught?
    answer: "In clap or right after parsing, before picking the change request: exit 2, nothing sent."
    by: agent
    why: "Avoids repeating the `cr answer` behavior the spec-change-request-commands design listed as a bug (argument checks exit 1 after the branch lookup)."
    rejected:
      - "Check after the branch lookup, as `cr answer` does today."
  - id: D7
    question: How much of `changed_since` does the Markdown show?
    answer: "Commit and file counts and the item-path verdict; when the item's paths changed, those paths."
    by: agent
    why: "A file list per item would bury the one fact the reviewer needs: whether this item's own paths moved."
    rejected:
      - Print every changed file under each item.
left_to_builder:
  - "The exact Markdown of the packet's new lines: the earlier answer, what changed since, the per-item `cr reapply` command, the not-re-applicable reason, the two count lines below `## Items to answer`, and the option and re-applied parts of `- answered:`. Constraints: each item with an earlier answer shows who answered, at which head, and whether its own paths changed; each `not_reapplicable_reason` (`kind`, `answer`, `option_removed`, `answered_at_head`) reads as a plain explanation and an unknown reason prints as sent; heads in prose may be shortened as the verdict line does, heads in commands are full."
  - "The wording of `cr reapply`'s text output and error messages, within what the spec requires each to say."
  - "What `cr reapply --untouched --json` prints when nothing is selected."
  - "Type, function and clap layout, and how `cr review` and `--untouched` share the untouched rule."
  - "Tests, and where `cr reapply` is documented: README's change request section, `src/agents_guide.md` (usage errors and the conflict table), `cr reapply --help` examples and the review-and-events recipe of the `verify-goodflows-cli` skill."
---

## Why

Review answers bind to the head. After a push every item in `goodflows cr review` is unanswered again, even when its id and content did not change, and the reviewer types each `goodflows cr answer` again. That rule stays. The goodflows_web change `add-review-answer-reapply` makes the second answer cheap instead, but the CLI renders the packet with a fixed renderer, so it shows none of the new data, and it has no command for the new route.

## Context

The server contract is goodflows_web `add-review-answer-reapply`:

- Each packet item gains `earlier_answer`: null, or `{answer, reapplicable, not_reapplicable_reason, changed_since}`. `answer` is the latest answer to the same item id at another head, with `answer`, `option`, `note`, `head`, `reapplied_from` and `answered_by: {user_id, key_id}`; the CLI can show only the `user_id`, since a readable name needs a server field. `not_reapplicable_reason` is `kind`, `answer`, `option_removed` or `answered_at_head` when `reapplicable` is false.
- `changed_since` is `{commits, files, item_paths, item_paths_changed}`, or null when the earlier head is not in the current head's history. `item_paths_changed` is null for kinds without paths.
- Answers gain `reapplied_from`: null, or an object with the `head` the answer was copied from.
- `POST /api/v1/projects/<project>/change-requests/<N>/review/reapply` takes `{head, item_ids}` with 1 to 200 ids and stores the earlier answers of all of them at `head`, or none. `201` returns the answers in `data`, in `item_ids` order. `409` is `{"errors": {"detail", "head"}}` for a stale head and `{"errors": {"detail"}}`, with no `head`, for a merged or closed change request. `422` has `errors` keyed `item_ids[<i>]`.

`cr review --json` already passes the packet through. Against a server without the change, the packet has no `earlier_answer`, so `cr review` prints what it prints today, `--untouched` finds nothing to post, and `cr reapply <ids>` gets the generic `404`.

## Approaches

The CLI renders the new packet fields and adds one command for the new route, in two forms: named ids, and `--untouched` for the items the packet shows as untouched. The alternatives were a mode of `cr answer` (D1) and a client-side check of named ids against the packet (D3). Re-applying automatically on push was not considered, since it would undo the head-binding rule (N1) and U2.
