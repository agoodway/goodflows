# Spec Delta

## MODIFIED Requirements

### Requirement: cr review reads the review packet
`goodflows cr review` SHALL send `GET change-requests/<N>/review` for the change request it picks, as the change-requests capability specifies, and print one `review_packet` event with `number` and `packet`, the response's `data`. With `--json` that event SHALL be printed as JSON, keeping every field the server returns, including each item's `earlier_answer` and each answer's `reapplied_from`; otherwise the packet SHALL be printed as Markdown. It SHALL exit 0 when the read succeeds.

#### Scenario: Packet as JSON
- **WHEN** a user runs `goodflows cr review -n 42 --json`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/change-requests/42/review`, prints one JSON event with `type` `review_packet`, `number` `42` and `packet` equal to the response's `data`, and exits 0

#### Scenario: JSON keeps earlier answers
- **WHEN** an item's `earlier_answer` is `{"answer": {"answer": "approve", "head": "dddd…", "reapplied_from": null}, "reapplicable": true, "not_reapplicable_reason": null, "changed_since": {"commits": 2, "files": ["lib/a.ex"], "item_paths": ["lib/b.ex"], "item_paths_changed": false}}` and the user passed `--json`
- **THEN** the event's `packet.items[0].earlier_answer` equals that object

### Requirement: Each item to answer gets its own section
The Markdown SHALL print `## Items to answer (<count>)`, even when the count is 0, then the re-apply summary lines, then for each item a `### <kind>: <message>` heading and `- id: `<id>``. When the item already has an answer at the head, it SHALL add `- answered: <answer>`, followed by ` <option>` when the answer has an option, ` (<note>)` when it has a note, and `, re-applied from `<head>`` when its `reapplied_from` is an object, with that head cut to 12 characters.

#### Scenario: An open question
- **WHEN** the packet has one item with id `question:9c1b`, kind `question`, message `Open question q1` and `answer` null
- **THEN** the Markdown has `## Items to answer (1)`, `### question: Open question q1` and ``- id: `question:9c1b` `` and no `- answered:` line

#### Scenario: An answered item
- **WHEN** an item's `answer` is `{"answer": "request_change", "note": "Add a test"}`
- **THEN** its section has `- answered: request_change (Add a test)`

#### Scenario: A re-applied choice
- **WHEN** an item's `answer` is `{"answer": "choose", "option": "b", "note": null, "reapplied_from": {"head": "dddd…"}}` (head 40 characters)
- **THEN** its section has ``- answered: choose b, re-applied from `dddddddddddd` ``

## ADDED Requirements

### Requirement: The packet shows each item's earlier answer
When an item's `earlier_answer` is an object, its section SHALL print, after the `- id:` line and any `- answered:` line, ``- earlier answer: <answer>[ <option>] by <user> at `<head>`[ (<note>)]``, where `<answer>`, `<option>`, `<note>` and `<head>` come from `earlier_answer.answer`, `<head>` is cut to 12 characters, and `<user>` is `answered_by.user_id`, or `-` when absent. When `earlier_answer` is null or missing, no earlier answer lines SHALL print.

#### Scenario: An approval two commits ago
- **WHEN** item `one_way_door:3f2a` has `earlier_answer.answer` `{"answer": "approve", "head": "dddd…", "note": "fine", "answered_by": {"user_id": "u_7", "key_id": "k_1"}}`
- **THEN** its section has ``- earlier answer: approve by u_7 at `dddddddddddd` (fine)``

#### Scenario: Server without re-apply
- **WHEN** no item has an `earlier_answer` key
- **THEN** the Markdown is the same as before this change

### Requirement: The packet summarises what changed since the earlier answer
Below the earlier answer line the section SHALL print `- changed since: <commits> commits, <files> files; <paths>`, where `<files>` is the length of `changed_since.files` and `<paths>` is `item paths unchanged` when `item_paths_changed` is `false`, `item paths changed: <item_paths>` (joined by `, `) when it is `true`, and `no item paths` when it is null. When `changed_since` is null it SHALL print `- changed since: unknown; the earlier head is not in this head's history`.

#### Scenario: Untouched paths
- **WHEN** `changed_since` is `{"commits": 2, "files": ["lib/a.ex", "README.md"], "item_paths": ["lib/b.ex"], "item_paths_changed": false}`
- **THEN** the section has `- changed since: 2 commits, 2 files; item paths unchanged`

#### Scenario: Touched paths
- **WHEN** `changed_since` is `{"commits": 1, "files": ["lib/b.ex"], "item_paths": ["lib/b.ex"], "item_paths_changed": true}`
- **THEN** the section has `- changed since: 1 commits, 1 files; item paths changed: lib/b.ex`

#### Scenario: Rewritten history
- **WHEN** `changed_since` is null
- **THEN** the section has `- changed since: unknown; the earlier head is not in this head's history`

### Requirement: The packet prints a re-apply command for a re-applicable item
When `earlier_answer.reapplicable` is `true`, the section SHALL print ``- `goodflows cr reapply -n <N> <id> --head <head>` ``, with the packet's full head, before the item's answer commands. When the packet's head is null, ` --head <head>` SHALL be left out.

#### Scenario: A re-applicable item
- **WHEN** the packet for change request 42 has head `aaaa…` (40 characters) and item `one_way_door:3f2a` has `reapplicable` true
- **THEN** its section has ``- `goodflows cr reapply -n 42 one_way_door:3f2a --head aaaa…` `` with all 40 characters

### Requirement: The packet says why an earlier answer cannot be re-applied
When `earlier_answer.reapplicable` is not `true`, the section SHALL print `- not re-applicable: <why>` and no `cr reapply` command. `<why>` SHALL be `this kind needs a new answer at every head` for `kind`, `only approve and choose re-apply` for `answer`, `the chosen option is no longer offered` for `option_removed`, `already answered at this head` for `answered_at_head`, and the reason itself for any other value.

#### Scenario: A rejected earlier answer
- **WHEN** an item's `earlier_answer` has `reapplicable` false and `not_reapplicable_reason` `answer`
- **THEN** its section has `- not re-applicable: only approve and choose re-apply` and no `cr reapply` command

### Requirement: The packet counts untouched items
An item is untouched when its `earlier_answer.reapplicable` is `true` and its `earlier_answer.changed_since.item_paths_changed` is the JSON value `false`. After `## Items to answer (<count>)`, when at least one item is untouched and the packet's head is not null, the Markdown SHALL print ``<k> untouched items: `goodflows cr reapply -n <N> --untouched --head <head>` ``.

#### Scenario: Two untouched items
- **WHEN** the packet for change request 42 at head `aaaa…` has re-applicable items with `item_paths_changed` `false`, `false` and `true`
- **THEN** the Markdown has ``2 untouched items: `goodflows cr reapply -n 42 --untouched --head aaaa…` ``

### Requirement: The packet counts re-applicable items that need reading
When at least one item has `reapplicable` true and is not untouched, the Markdown SHALL print, after the untouched line, `<m> re-applicable items have changed or unknown item paths; re-apply each by id after reading it.` The line SHALL NOT print when that count is 0.

#### Scenario: A touched item and a question
- **WHEN** re-applicable items have `item_paths_changed` `true` and null
- **THEN** the Markdown has `2 re-applicable items have changed or unknown item paths; re-apply each by id after reading it.`

### Requirement: cr reapply takes item ids or --untouched
`goodflows cr reapply` SHALL take either 1 to 200 `ITEM_ID` arguments or `--untouched`, not both and not neither, plus `--head SHA`. These SHALL be usage errors with exit 2 and no request sent: no ids and no `--untouched`, ids with `--untouched`, more than 200 ids, and an id named twice (`item <id> is named twice`).

#### Scenario: Both forms
- **WHEN** a user runs `goodflows cr reapply -n 42 question:9c1b --untouched`
- **THEN** goodflows exits 2 and sends no request

#### Scenario: Duplicate id
- **WHEN** a user runs `goodflows cr reapply -n 42 q:1 q:1`
- **THEN** goodflows prints `item q:1 is named twice`, sends no request and exits 2

### Requirement: cr reapply with ids posts them as named
With item ids, `cr reapply` SHALL send `POST change-requests/<N>/review/reapply` with the JSON body `{"head": <head>, "item_ids": [<ids in the order given>]}` and no other keys. `head` SHALL be `--head SHA` as given; without it, goodflows SHALL first read `GET change-requests/<N>` and send its `head`, null when it has none. It SHALL NOT read the packet and SHALL NOT check reapplicability itself; the server's `422` decides.

#### Scenario: One item at the default head
- **WHEN** change request 42's head is `cccc…` (40 characters) and a user runs `goodflows cr reapply -n 42 path_match:77de`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/change-requests/42`, then `POST /api/v1/projects/<project>/change-requests/42/review/reapply` with body `{"head": "cccc…", "item_ids": ["path_match:77de"]}`

### Requirement: cr reapply --untouched picks only untouched items
With `--untouched`, `cr reapply` SHALL read `GET change-requests/<N>/review` and select, in packet order, the items the packet's untouched rule selects. It SHALL never select an item whose `item_paths_changed` is `true` or null, or whose `changed_since` is null. It SHALL report each re-applicable item it leaves out as skipped with reason `item_paths_changed` (`true`) or `item_paths_unknown` (null `item_paths_changed` or null `changed_since`).

#### Scenario: Touched and unknown items are skipped
- **WHEN** the packet at head `aaaa…` has re-applicable items `a:1` (`item_paths_changed` false), `b:2` (true) and `q:3` (null) and a user runs `goodflows cr reapply -n 42 --untouched`
- **THEN** goodflows posts `{"head": "aaaa…", "item_ids": ["a:1"]}` and reports `b:2` skipped as `item_paths_changed` and `q:3` as `item_paths_unknown`

### Requirement: cr reapply --untouched posts at the packet's head
With `--untouched`, the posted head SHALL be the packet's head. When `--head` is given and differs from it, goodflows SHALL post nothing and fail with exit 1: `change request #<N> is at head <packet head>, not <head>; read goodflows cr review again`.

#### Scenario: Head mismatch
- **WHEN** the packet's head is `aaaa…` and a user runs `goodflows cr reapply -n 42 --untouched --head bbbb…`
- **THEN** goodflows posts nothing and exits 1 with `change request #42 is at head aaaa…, not bbbb…; read goodflows cr review again`

### Requirement: cr reapply --untouched with none or too many
With no item selected, `--untouched` SHALL post nothing, print `No untouched items to re-apply.` followed by any skipped items, and exit 0. With more than 200 selected it SHALL post nothing and fail with exit 1: `<k> untouched items exceed the 200 a request takes; re-apply them by id in batches`.

#### Scenario: Nothing untouched
- **WHEN** no item in the packet is untouched
- **THEN** goodflows sends only `GET` requests, prints `No untouched items to re-apply.` and exits 0

### Requirement: cr reapply names the new head when the head moved
A `409` whose `errors` has `head` and no `error` SHALL fail with exit 1 and `cr reapply failed: change request #<N> moved to head <errors.head> since the answers' head; read goodflows cr review again and re-apply at the new head`. Any other `409`, including the one for a merged or closed change request, SHALL print as the change-requests capability specifies.

#### Scenario: Head moved
- **WHEN** the server responds `409` with `{"errors": {"detail": "stale", "head": "cccc…"}}`
- **THEN** goodflows prints `cr reapply failed: change request #42 moved to head cccc… since the answers' head; read goodflows cr review again and re-apply at the new head` and exits 1

### Requirement: cr reapply names the refused items
A `422` whose `errors` keys are all `item_ids[<i>]` SHALL fail with exit 1 and `cr reapply failed: nothing was re-applied; <id>: <messages>[; <id>: <messages>…]`, naming the request's id at each index and joining each key's messages with `, `. Any other `422` SHALL print as the change-requests capability specifies.

#### Scenario: One item cannot be re-applied
- **WHEN** a user re-applies `a:1` and `b:2` and the server responds `422` with `{"errors": {"item_ids[1]": ["is not reapplicable"]}}`
- **THEN** goodflows prints `cr reapply failed: nothing was re-applied; b:2: is not reapplicable` and exits 1

### Requirement: cr reapply prints one review_reapply event
A stored batch SHALL print one `review_reapply` event with `number`, `head` (the head sent), `answers` (one `{"item_id", "answer"}` per entry of the response's `data`, paired with `item_ids` by index) and `skipped` (`{"item_id", "reason"}`, empty without `--untouched`), and exit 0.

#### Scenario: As JSON
- **WHEN** `goodflows cr reapply -n 42 a:1 q:9 --json` stores two answers at head `cccc…`
- **THEN** goodflows prints one event with `type` `review_reapply`, `number` `42`, `head` `cccc…`, `answers[1].item_id` `q:9`, `answers[1].answer` equal to the second entry of `data`, and `skipped` `[]`

### Requirement: The re-apply text lists answers and skipped items
The text output SHALL print `Re-applied <n> answers at head <head>:`, then one line per answer, ``- <item_id>: <answer>[ <option>] from `<reapplied_from.head>` ``, with heads cut to 12 characters. When `skipped` is not empty it SHALL then print `Skipped; name them to re-apply:` and `- <item_id>: item paths changed` or `- <item_id>: item paths unknown` per item.

#### Scenario: Two answers as text
- **WHEN** `cr reapply -n 42 a:1 q:9` stores `approve` and `choose` with option `b`, both re-applied from head `dddd…`, at head `cccc…`
- **THEN** goodflows prints `Re-applied 2 answers at head cccccccccccc:`, ``- a:1: approve from `dddddddddddd` `` and ``- q:9: choose b from `dddddddddddd` `` and exits 0
