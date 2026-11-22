# Spec Delta

## ADDED Requirements

### Requirement: cr review reads the review packet
`goodflows cr review` SHALL send `GET change-requests/<N>/review` for the change request it picks, as the change-requests capability specifies, and print one `review_packet` event with `number` and `packet`, the response's `data`. With `--json` that event SHALL be printed as JSON; otherwise the packet SHALL be printed as Markdown. It SHALL exit 0 when the read succeeds.

#### Scenario: Packet as JSON
- **WHEN** a user runs `goodflows cr review -n 42 --json`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/change-requests/42/review`, prints one JSON event with `type` `review_packet`, `number` `42` and `packet` equal to the response's `data`, and exits 0

### Requirement: The packet Markdown starts with the verdict at the head
The Markdown SHALL start with `# Review: change request #<N>`, a blank line, then `Verdict **<verdict>** at head `<head>` against `<base.branch>` (merge base `<base.merge_base>`), policy v<policy_version>.`, with `head` and `merge_base` cut to their first 12 characters. A null value SHALL print as `-`.

#### Scenario: Header of a packet with Git facts
- **WHEN** the packet for change request 42 has verdict `needs_review`, head `aaaa…` (40 characters), base branch `main`, merge base `bbbb…` (40 characters) and `policy_version` 7
- **THEN** the output starts `# Review: change request #42` and its next line is ``Verdict **needs_review** at head `aaaaaaaaaaaa` against `main` (merge base `bbbbbbbbbbbb`), policy v7.``

#### Scenario: Packet without Git facts
- **WHEN** the packet has `head` null and `base` null
- **THEN** the verdict line reads ``at head `-` against `-` (merge base `-`)``

### Requirement: The packet Markdown summarises what passed
When the packet's `passed` is an object, the Markdown SHALL print `Passed: <must> Must and <should> Should scenarios; workflows: <workflows>.` with the workflow names joined by `, `, or `none` when the list is empty. When `passed` is not an object the line SHALL be left out.

#### Scenario: Passed scenarios and workflows
- **WHEN** the packet's `passed` is `{"must": 6, "should": 2, "workflows": ["ci"]}`
- **THEN** the Markdown has the line `Passed: 6 Must and 2 Should scenarios; workflows: ci.`

#### Scenario: No required workflows
- **WHEN** the packet's `passed` is `{"must": 1, "should": 0, "workflows": []}`
- **THEN** the line ends `workflows: none.`

### Requirement: Blocked and pending reasons are listed as context
For each nonempty list of `blocked`, then `pending`, the Markdown SHALL print a `## Blocked (no answer clears these)` or `## Pending (no answer clears these)` heading and one line per entry: `- `<rule>` <subjects>: <message>`, with the subjects joined by `, ` or `none`. An empty list SHALL print no heading.

#### Scenario: A pending workflow
- **WHEN** the packet's `pending` has rule `required_workflow_missing`, subject `["ci"]` and message `No ci run`, and `blocked` is empty
- **THEN** the Markdown has `## Pending (no answer clears these)` followed by ``- `required_workflow_missing` ci: No ci run`` and has no `## Blocked` heading

### Requirement: Each item to answer gets its own section
The Markdown SHALL print `## Items to answer (<count>)`, even when the count is 0, then for each item a `### <kind>: <message>` heading and `- id: `<id>``. When the item already has an answer at the head, it SHALL add `- answered: <answer>`, followed by ` (<note>)` when the answer has a note.

#### Scenario: An open question
- **WHEN** the packet has one item with id `question:9c1b`, kind `question`, message `Open question q1` and `answer` null
- **THEN** the Markdown has `## Items to answer (1)`, `### question: Open question q1` and ``- id: `question:9c1b` `` and no `- answered:` line

#### Scenario: An answered item
- **WHEN** an item's `answer` is `{"answer": "request_change", "note": "Add a test"}`
- **THEN** its section has `- answered: request_change (Add a test)`

### Requirement: Item data prints hunks, artifacts and fields
Each item's `data` fields SHALL print in key order. Each `decisions` entry SHALL print ``Decision `<id>` in `<path>`: <note>`` and its `hunk` in a fenced `diff` block. Each `artifacts` entry SHALL print `![<id>](<url>)` for an `image/` content type with a URL, `[<id>](<url>)` for another URL, and `<id>: expired` without one. Other fields SHALL print as `- <key>: <value>`, lists and objects indented below `- <key>:`.

#### Scenario: Screenshot and expired artifact
- **WHEN** an item's `data.artifacts` holds `{"id": "a1", "content_type": "image/png", "url": "https://s.example/a1"}` and `{"id": "a2", "content_type": "image/png", "url": null}`
- **THEN** the item prints `![a1](https://s.example/a1)` and `a2: expired`

#### Scenario: Question options
- **WHEN** a `question` item's `data` is `{"text": "Keep the old export?", "options": ["a", "b"]}`
- **THEN** the item prints `- options:`, `  - a` and `  - b`, then `- text: Keep the old export?`

### Requirement: The packet prints an answer command per allowed answer
For each answer in an item's `answers`, the Markdown SHALL print `- `goodflows cr answer -n <N> <id> <answer><extra> --head <head>``, with the packet's full head. `<extra>` SHALL be ` --option <option>` for `choose`, ` --note '<why>'` for `reject` and `request_change`, and empty for `approve`. When the packet's head is null, ` --head <head>` SHALL be left out.

#### Scenario: Commands for the head
- **WHEN** the packet for change request 42 has head `aaaa…` (40 characters) and item `question:9c1b` allows `["choose", "request_change"]`
- **THEN** the Markdown has ``- `goodflows cr answer -n 42 question:9c1b choose --option <option> --head aaaa…` `` with all 40 characters of the head, and ``- `goodflows cr answer -n 42 question:9c1b request_change --note '<why>' --head aaaa…` ``

#### Scenario: No head yet
- **WHEN** the packet's head is null and an item allows `approve`
- **THEN** its command is `goodflows cr answer -n 42 <id> approve` with no `--head`

### Requirement: Server text in the packet is escaped
Every string the packet Markdown prints from the server SHALL pass through the escaping of control characters that the change-requests capability specifies, except that a decision hunk SHALL keep its line breaks and escape the rest of each line.

#### Scenario: Escape in a message
- **WHEN** an item's kind is `question` and its message is `Open` then the ESC character then `[2J q1`
- **THEN** the heading prints `### question: Open\u{1b}[2J q1`, with no raw ESC character

#### Scenario: Multi-line hunk
- **WHEN** a decision's hunk is `-a\n+b`
- **THEN** the `diff` block holds the two lines `-a` and `+b`

### Requirement: cr answer takes an item, an answer and its arguments
`goodflows cr answer <ITEM_ID> <ANSWER>` SHALL accept `approve`, `reject`, `request_change` or `choose` as ANSWER and reject any other value as a usage error with exit 2. `choose` SHALL require `--option`, which no other answer accepts, and `reject` and `request_change` SHALL require `--note`. `--note` SHALL be optional for `approve` and `choose`.

#### Scenario: Unknown answer
- **WHEN** a user runs `goodflows cr answer question:9c1b accept`
- **THEN** goodflows exits 2 listing the possible values `approve, reject, request_change, choose`

### Requirement: cr answer refuses missing or misplaced arguments before sending
After picking the change request, `cr answer` SHALL fail with exit 1 and send no answer when `choose` has no `--option` (`choose needs --option`), another answer has `--option` (`--option is only for choose`), or `reject` or `request_change` has no `--note` (`<answer> needs --note`).

#### Scenario: Choose without an option
- **WHEN** a user runs `goodflows cr answer -n 42 question:9c1b choose`
- **THEN** goodflows prints `choose needs --option`, sends no request and exits 1

#### Scenario: Option with approve
- **WHEN** a user runs `goodflows cr answer -n 42 question:9c1b approve --option b`
- **THEN** goodflows prints `--option is only for choose`, sends no request and exits 1

#### Scenario: Request a change without a note
- **WHEN** a user runs `goodflows cr answer -n 42 path_match:77de request_change`
- **THEN** goodflows prints `request_change needs --note`, sends no request and exits 1

### Requirement: cr answer posts the answer for a head
`cr answer` SHALL send `POST change-requests/<N>/review/items/<ITEM_ID>/answers` with a JSON body of `head`, `answer`, and `option` and `note` only when given. `head` SHALL be `--head SHA` as given; without it, goodflows SHALL first read `GET change-requests/<N>` and send that change request's `head`, null when it has none.

#### Scenario: Default head
- **WHEN** change request 42's head is `cccc…` (40 characters) and a user runs `goodflows cr answer -n 42 one_way_door:3f2a approve`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/change-requests/42`, then `POST /api/v1/projects/<project>/change-requests/42/review/items/one_way_door:3f2a/answers` with body `{"answer": "approve", "head": "cccc…"}` and no `option` or `note`

#### Scenario: Explicit head
- **WHEN** a user runs `goodflows cr answer -n 42 question:9c1b choose --option b --note why --head 0123…` (40 characters)
- **THEN** goodflows reads no change request and sends one `POST` whose body is `{"answer": "choose", "head": "0123…", "note": "why", "option": "b"}`

### Requirement: An answer at an old head names the new head
When the answer request fails with `409` and the response's `errors` has `head` but no `error`, `cr answer` SHALL fail with exit 1 and print `cr answer failed: change request #<N> moved to head <errors.head> since the answer's head; read goodflows cr review again and answer at the new head`. Any other failure SHALL print as the change-requests capability specifies.

#### Scenario: Head moved
- **WHEN** a user answers change request 42 with `--head 0000000000000000000000000000000000000000` and the server responds `409` with `{"errors": {"detail": "stale", "head": "cccc…"}}`
- **THEN** goodflows prints `cr answer failed: change request #42 moved to head cccc… since the answer's head; read goodflows cr review again and answer at the new head` and exits 1

### Requirement: cr answer prints the stored answer
A stored answer SHALL print one `review_answer` event with `number`, `item_id` and `answer`, the response's `data`, and exit 0. The text output SHALL print the answer's fields as `key: value` lines, nested values indented.

#### Scenario: Approve as JSON
- **WHEN** a user runs `goodflows cr answer -n 42 q:1 approve --json` and the server stores `{"answer": "approve", "head": "cccc…"}`
- **THEN** goodflows prints a JSON event with `type` `review_answer`, `number` `42`, `item_id` `q:1` and that `answer`, and exits 0

#### Scenario: Approve as text
- **WHEN** the same answer is stored without `--json`
- **THEN** goodflows prints `answer: approve` and `head: cccc…`

### Requirement: cr tasks lists the change request's tasks
`goodflows cr tasks` SHALL send `GET change-requests/<N>/tasks`, with `state=<state>` only when `--state open|stale|done` is given, so the server's default of open and stale tasks applies. It SHALL print one `change_request_tasks` event with `number` and `tasks`. The text output SHALL print `- [<state>] <id>` per task with its fields indented two spaces below, or `No tasks.` when the list is empty.

#### Scenario: Default listing
- **WHEN** change request 42 has open task `5b1f` and a user runs `goodflows cr tasks -n 42`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/change-requests/42/tasks` with no query and prints `- [open] 5b1f` followed by lines such as `  state: open`

#### Scenario: No finished tasks
- **WHEN** a user runs `goodflows cr tasks -n 42 --state done` and the response's `data` is `[]`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/change-requests/42/tasks?state=done`, prints `No tasks.` and exits 0

### Requirement: cr tasks done marks one task done
`goodflows cr tasks done <TASK_ID>` SHALL send `POST change-requests/<N>/tasks/<TASK_ID>/done` with no body and print one `change_request_task` event with `task`, the response's `data`; text output SHALL print the task's fields as `key: value` lines. `-n` SHALL be accepted before or after `done`.

#### Scenario: Number after done
- **WHEN** a user runs `goodflows cr tasks done 5b1f -n 42` or `goodflows cr tasks -n 42 done 5b1f`
- **THEN** goodflows sends exactly one request, `POST /api/v1/projects/<project>/change-requests/42/tasks/5b1f/done`, with no branch lookup

#### Scenario: Printed task
- **WHEN** the server responds with `{"data": {"id": "5b1f", "state": "done"}}`
- **THEN** goodflows prints `id: 5b1f` and `state: done` and exits 0

### Requirement: cr tasks done refuses --state
`cr tasks` with both `--state` and `done` SHALL fail as a usage error, `--state cannot be used with cr tasks done`, with exit 2, before checking the `[reporting]` block or sending any request.

#### Scenario: State with done
- **WHEN** a user runs `goodflows cr tasks -n 42 --state open done 5b1f`
- **THEN** goodflows prints `--state cannot be used with cr tasks done`, sends no request and exits 2
