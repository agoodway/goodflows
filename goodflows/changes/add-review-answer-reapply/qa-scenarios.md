---
format: qa-scenarios/2
source: intent
change: add-review-answer-reapply
specs: uncommitted
scenarios:
  - id: 1
    name: JSON packet keeps earlier answers
    priority: must
    covers:
      - {capability: change-request-review, requirement: cr review reads the review packet, scenario: JSON keeps earlier answers}
  - id: 2
    name: Answered line shows a re-applied choice
    priority: must
    covers:
      - {capability: change-request-review, requirement: Each item to answer gets its own section, implied: true}
  - id: 3
    name: Packet shows earlier answers and the untouched count
    priority: must
    covers:
      - {capability: change-request-review, requirement: The packet shows earlier answers and counts untouched items, scenario: "Untouched, touched and unknown items"}
  - id: 4
    name: Older server packet renders as before
    priority: must
    covers:
      - {capability: change-request-review, requirement: The packet shows earlier answers and counts untouched items, scenario: Server without re-apply}
  - id: 5
    name: Usage errors send nothing
    priority: must
    covers:
      - {capability: change-request-review, requirement: cr reapply takes item ids or --untouched, implied: true}
  - id: 6
    name: Named ids post at the change request's head
    priority: must
    covers:
      - {capability: change-request-review, requirement: cr reapply with ids posts them in one request, scenario: One item at the default head}
      - {capability: change-request-review, requirement: cr reapply prints one review_reapply event, scenario: As JSON}
  - id: 7
    name: Untouched re-applies only items whose paths did not change
    priority: must
    covers:
      - {capability: change-request-review, requirement: cr reapply --untouched picks only untouched items, implied: true}
  - id: 8
    name: Refused batch stores nothing
    priority: must
    covers:
      - {capability: change-request-review, requirement: cr reapply names the new head or the refused items, implied: true}
---

# QA: add-review-answer-reapply

Run these against a goodflows_web build with `add-review-answer-reapply` (scenario 4 against one without), on a project with change request 42 whose review packet has at least two items you can approve.

## Scenarios

### 1. JSON packet keeps earlier answers (Must)

1. Approve two items with `goodflows cr answer -n 42 <id> approve`.
2. Push a commit to the change request's branch.
3. Run `goodflows cr review -n 42 --json`.

Expected: each answered item has an `earlier_answer` object with `answer`, `reapplicable`, `not_reapplicable_reason` and `changed_since`, identical to what `GET .../change-requests/42/review` returns; the command exits 0.

### 2. Answered line shows a re-applied choice (Must)

1. Answer a `question` item with `choose --option b`, push, then run `goodflows cr reapply -n 42 <question id>`.
2. Run `goodflows cr review -n 42`.

Expected: the item's `- answered:` line shows `choose`, the option `b` and the head the answer was re-applied from; an item answered directly with `request_change --note 'Add a test'` still shows `- answered: request_change (Add a test)`.

### 3. Packet shows earlier answers and the untouched count (Must)

1. Approve one item whose path is `lib/a.ex`, one whose path is `lib/b.ex`, and answer a `question` item with `choose`.
2. Push a commit that changes only `lib/b.ex`.
3. Run `goodflows cr review -n 42`.

Expected: each of the three items shows who answered, at which earlier head, the commit and file counts since, and whether its own paths changed (unchanged, changed with `lib/b.ex` named, no item paths). Each has a `goodflows cr reapply -n 42 <id> --head <full head>` command. Below `## Items to answer`, the summary counts 1 untouched item with `goodflows cr reapply -n 42 --untouched --head <full head>` and 2 items to re-apply by id. An item whose earlier answer was `reject` says why it cannot be re-applied and has no `cr reapply` command.

### 4. Older server packet renders as before (Must)

1. Against a goodflows_web build without `add-review-answer-reapply`, run `goodflows cr review -n 42` with this build and with the previous release.
2. Run `goodflows cr reapply -n 42 --untouched`, then `goodflows cr reapply -n 42 <id>`.

Expected: the two `cr review` outputs are byte for byte equal. `--untouched` sends only `GET` requests and exits 0. The named id fails with the generic `404` message and exits 1.

### 5. Usage errors send nothing (Must)

Run each with a request log on the server:

1. `goodflows cr reapply -n 42`
2. `goodflows cr reapply -n 42 q:1 --untouched`
3. `goodflows cr reapply -n 42 q:1 q:1`
4. `goodflows cr reapply -n 42` with 201 distinct ids

Expected: each exits 2; the third names `q:1`; the server logs no request, not even a change request lookup.

### 6. Named ids post at the change request's head (Must)

1. After the push in scenario 3, run `goodflows cr reapply -n 42 <lib/b.ex item id> <question id> --json`.

Expected: goodflows sends `GET .../change-requests/42`, then one `POST .../change-requests/42/review/reapply` with body `{"head": "<current head>", "item_ids": ["<lib/b.ex item id>", "<question id>"]}`. It prints one `review_reapply` event with `number` 42, that `head`, two `answers` in that order each with `item_id` and the stored answer, and `skipped` `[]`; it exits 0. Running it again with `--head <current head>` sends only the `POST`.

### 7. Untouched re-applies only items whose paths did not change (Must)

1. Repeat scenario 3's setup and run `goodflows cr reapply -n 42 --untouched --json`.
2. Run `goodflows cr review -n 42`.
3. Run `goodflows cr reapply -n 42 --untouched --head 0000000000000000000000000000000000000000`.
4. Run `goodflows cr reapply -n 42 --untouched` again.

Expected: step 1 posts only the `lib/a.ex` item at the packet's head and reports the `lib/b.ex` item skipped as `item_paths_changed` and the question as `item_paths_unknown`; the number posted equals the untouched count `cr review` showed. Step 2 shows that item answered and re-applied. Step 3 posts nothing, names both heads and exits 1. Step 4 finds nothing untouched, posts nothing and exits 0.

### 8. Refused batch stores nothing (Must)

1. Run `goodflows cr reapply -n 42 <re-applicable id> <id of an item whose earlier answer was reject>`.
2. Run `goodflows cr reapply -n 42 <re-applicable id> --head <the head before the last push>`.
3. Merge change request 42 and run `goodflows cr reapply -n 42 <re-applicable id>`.

Expected: step 1 exits 1, says nothing was re-applied and names the second id with the server's message, and `cr review` shows neither item answered at the head. Step 2 exits 1 naming the current head and telling the user to read `goodflows cr review` again. Step 3 exits 1 with the generic conflict message and no head.
