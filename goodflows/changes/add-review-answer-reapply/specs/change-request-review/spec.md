## MODIFIED Requirements

### Requirement: cr review reads the review packet
Refs: U3, G3
`goodflows cr review` SHALL send `GET change-requests/<N>/review` for the change request it picks, as the change-requests capability specifies, and print one `review_packet` event with `number` and `packet`, the response's `data`. With `--json` that event SHALL be printed as JSON, keeping every field the server returns, including each item's `earlier_answer` and each answer's `reapplied_from`; otherwise the packet SHALL be printed as Markdown. It SHALL exit 0 when the read succeeds.

#### Scenario: Packet as JSON
- **WHEN** a user runs `goodflows cr review -n 42 --json`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/change-requests/42/review`, prints one JSON event with `type` `review_packet`, `number` `42` and `packet` equal to the response's `data`, and exits 0

#### Scenario: JSON keeps earlier answers
- **WHEN** an item has an `earlier_answer` object whose answer has `reapplied_from` and the user passed `--json`
- **THEN** the event's `packet.items[0].earlier_answer` equals the server's object, unchanged

### Requirement: Each item to answer gets its own section
Refs: G1
The Markdown SHALL print `## Items to answer (<count>)`, even when the count is 0, then for each item a `### <kind>: <message>` heading and `- id: `<id>``. When the item already has an answer at the head, it SHALL add `- answered: <answer>`, followed by the option when the answer has one, ` (<note>)` when it has a note, and the head it was re-applied from when its `reapplied_from` is an object.

#### Scenario: An open question
- **WHEN** the packet has one item with id `question:9c1b`, kind `question`, message `Open question q1` and `answer` null
- **THEN** the Markdown has `## Items to answer (1)`, `### question: Open question q1` and ``- id: `question:9c1b` `` and no `- answered:` line

#### Scenario: An answered item
- **WHEN** an item's `answer` is `{"answer": "request_change", "note": "Add a test"}`
- **THEN** its section has `- answered: request_change (Add a test)`

## ADDED Requirements

### Requirement: The packet shows earlier answers and counts untouched items
Refs: G1, G3, U1, U2, D2, D7
An item is untouched when its `earlier_answer.reapplicable` is `true` and its `earlier_answer.changed_since.item_paths_changed` is the JSON value `false`. When an item's `earlier_answer` is an object, its section SHALL show the earlier answer, who gave it, at which head, the commit and file counts since that head, and whether the item's own paths changed, are unknown, or do not exist; then, when `reapplicable` is `true`, the command `goodflows cr reapply -n <N> <id>` with `--head` and the packet's full head (left out when the packet's head is null), and otherwise why the answer cannot be re-applied, with no `cr reapply` command. Below `## Items to answer`, when the packet's head is not null and at least one item is untouched, the Markdown SHALL print the number of untouched items and the command `goodflows cr reapply -n <N> --untouched --head <head>`; when at least one re-applicable item is not untouched, it SHALL print their number and that each must be re-applied by id. The untouched number SHALL equal the number of items `cr reapply --untouched` selects for the same packet. A packet in which no item has `earlier_answer` SHALL render exactly as it did before this change.

#### Scenario: Server without re-apply
- **WHEN** no item has an `earlier_answer` key
- **THEN** the Markdown is byte for byte the same as before this change

#### Scenario: Untouched, touched and unknown items
- **WHEN** the packet for change request 42 at head `aaaa…` has re-applicable items whose `item_paths_changed` are `false`, `false`, `true` and null
- **THEN** the Markdown counts 2 untouched items with ``goodflows cr reapply -n 42 --untouched --head aaaa…`` (all 40 characters), and 2 items to re-apply by id

### Requirement: cr reapply takes item ids or --untouched
Refs: U1, D1, D6
`goodflows cr reapply` SHALL take either 1 to 200 distinct `ITEM_ID` arguments or `--untouched`, plus an optional `--head SHA`. No ids and no `--untouched`, ids with `--untouched`, more than 200 ids, and an id named twice SHALL each be a usage error that exits 2 before goodflows picks the change request or sends any request; the duplicate error SHALL name the id.

#### Scenario: Both forms
- **WHEN** a user runs `goodflows cr reapply -n 42 question:9c1b --untouched`
- **THEN** goodflows exits 2 and sends no request

#### Scenario: Duplicate id
- **WHEN** a user runs `goodflows cr reapply q:1 q:1` on a branch with a change request
- **THEN** goodflows names `q:1`, sends no request, not even the branch lookup, and exits 2

### Requirement: cr reapply with ids posts them in one request
Refs: U1, U2, D3, D4, A1
With item ids, `cr reapply` SHALL send one `POST change-requests/<N>/review/reapply` with the JSON body `{"head": <head>, "item_ids": [<ids in the order given>]}` and no other keys. `head` SHALL be `--head SHA` as given, with no other request; without it, goodflows SHALL first read `GET change-requests/<N>` and send its `head`, null when it has none. It SHALL NOT read the packet or check reapplicability itself.

#### Scenario: One item at the default head
- **WHEN** change request 42's head is `cccc…` (40 characters) and a user runs `goodflows cr reapply -n 42 path_match:77de`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/change-requests/42`, then `POST /api/v1/projects/<project>/change-requests/42/review/reapply` with body `{"head": "cccc…", "item_ids": ["path_match:77de"]}`, and nothing else

### Requirement: cr reapply --untouched picks only untouched items
Refs: G2, G3, U1, U2, D2, D4, D5, A2
With `--untouched`, `cr reapply` SHALL read `GET change-requests/<N>/review` and select, in packet order, the untouched items. It SHALL never select an item whose `item_paths_changed` is `true` or null or missing, or whose `changed_since` is null. It SHALL post them in one request as `cr reapply` with ids does, with `head` set to the packet's head. Each re-applicable item it leaves out SHALL be reported as skipped with reason `item_paths_changed` (`true`) or `item_paths_unknown` (any other value); items that are not re-applicable SHALL NOT be reported. It SHALL post nothing and:
- exit 1 when `--head` is given and differs from the packet's head, naming both heads;
- exit 1 when more than 200 items are selected, naming the count and that the items must be re-applied by id in batches;
- exit 0 when no item is selected, saying so and listing the skipped items.

#### Scenario: Touched and unknown items are skipped
- **WHEN** the packet at head `aaaa…` has re-applicable items `a:1` (`item_paths_changed` false), `b:2` (true) and `q:3` (null) and a user runs `goodflows cr reapply -n 42 --untouched`
- **THEN** goodflows posts `{"head": "aaaa…", "item_ids": ["a:1"]}` and reports `b:2` skipped as `item_paths_changed` and `q:3` as `item_paths_unknown`

#### Scenario: Head mismatch
- **WHEN** the packet's head is `aaaa…` and a user runs `goodflows cr reapply -n 42 --untouched --head bbbb…`
- **THEN** goodflows posts nothing and exits 1 with a message naming both heads

#### Scenario: Nothing untouched
- **WHEN** no item in the packet is untouched, as with a server without re-apply
- **THEN** goodflows sends only `GET` requests and exits 0

### Requirement: cr reapply names the new head or the refused items
Refs: A1, D3, D4
A `409` whose `errors` has `head` and no `error` SHALL fail with exit 1 and a message naming the change request, the new head `errors.head`, and that `goodflows cr review` must be read again, the same rule as `cr answer`. A `422` whose `errors` keys are all `item_ids[<i>]` SHALL fail with exit 1, saying nothing was re-applied and naming, for each key, the id sent at index `<i>` with its messages. Any other failure, including the `409` for a merged or closed change request, SHALL print as the change-requests capability specifies.

#### Scenario: One item cannot be re-applied
- **WHEN** a user re-applies `a:1` and `b:2` and the server responds `422` with `{"errors": {"item_ids[1]": ["is not reapplicable"]}}`
- **THEN** goodflows names `b:2` with `is not reapplicable`, not `a:1`, and exits 1

#### Scenario: Merged change request
- **WHEN** the server responds `409` with `{"errors": {"detail": "merged"}}`
- **THEN** goodflows prints the generic failure, names no head, and exits 1

### Requirement: cr reapply prints one review_reapply event
Refs: U1, D5
A stored batch SHALL print one `review_reapply` event with `number`, `head` (the head sent), `answers` (one `{"item_id", "answer"}` per entry of the response's `data`, paired with the sent `item_ids` by index) and `skipped` (`{"item_id", "reason"}` per skipped item, empty without `--untouched`), and exit 0. The text output SHALL list each re-applied item with its answer and the head it came from, then the skipped items with their reasons.

#### Scenario: As JSON
- **WHEN** `goodflows cr reapply -n 42 a:1 q:9 --json` stores two answers at head `cccc…`
- **THEN** goodflows prints one event with `type` `review_reapply`, `number` `42`, `head` `cccc…`, `answers[1].item_id` `q:9`, `answers[1].answer` equal to the second entry of `data`, and `skipped` `[]`
