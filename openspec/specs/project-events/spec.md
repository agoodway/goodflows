# project-events Specification

## Purpose

Defines how `goodflows events` reads the project's event feed after a cursor, keeps reading with `--follow`, and handles events deleted by retention.

## Requirements

### Requirement: goodflows events reads one page of the event feed
`goodflows events` SHALL send `GET events` for the project, as the change-requests capability specifies for reads, with the query parameters `after`, `limit` and `wait` each only when it has a value, in that order. Without `--follow` it SHALL send exactly one request, print the page and exit 0.

#### Scenario: Oldest kept events
- **WHEN** a user runs `goodflows events`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/events` with no query, prints the page and exits 0

#### Scenario: Limit and wait
- **WHEN** a user runs `goodflows events --limit 2 --wait 0`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/events?limit=2&wait=0`

### Requirement: --after takes an integer event id cursor
`--after <CURSOR>` SHALL take a non-negative integer, an event id such as the next cursor printed, and send it as `after`. Without `--after` the server's default, the oldest kept event, SHALL apply. A value that is not an integer SHALL be a usage error with exit 2.

#### Scenario: Resume after a cursor
- **WHEN** a user runs `goodflows events --after 1042 --wait 30`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/events?after=1042&wait=30`

#### Scenario: Cursor that is not a number
- **WHEN** a user runs `goodflows events --after abc`
- **THEN** goodflows sends no request and exits 2

### Requirement: --wait and --limit are bounded
`--wait <SECS>` SHALL accept 0 to 30 and `--limit <N>` 1 to 200; a value outside its range SHALL be a usage error with exit 2 and no request. Without `--wait`, a single read SHALL send no `wait`, so it returns at once, and `--follow` SHALL send `wait=30`. With both `--follow` and `--wait`, the given `--wait` SHALL be sent.

#### Scenario: Wait above 30
- **WHEN** a user runs `goodflows events --wait 31`
- **THEN** goodflows reports `invalid value '31' for '--wait <SECS>': 31 is not in 0..=30` and exits 2

#### Scenario: Limit of zero
- **WHEN** a user runs `goodflows events --limit 0`
- **THEN** goodflows sends no request and exits 2

### Requirement: Events print one line each, then the next cursor
The text output SHALL print one line per event, `<inserted_at>  <type>  <#number>  <data>`, separated by two spaces, with `-` when `number` is null and `data` as compact JSON. After the events it SHALL print `next cursor: <next_cursor>` when the response has one. Every value SHALL be escaped as the change-requests capability specifies.

#### Scenario: Two events
- **WHEN** the page holds event 5 (`change_request_opened`, number 7, data `{"title":"x"}`, inserted `2026-11-21T00:00:00Z`) and event 6 (`policy_updated`, number null, data `{}`, inserted `2026-11-21T00:00:01Z`), with `next_cursor` 6
- **THEN** goodflows prints `2026-11-21T00:00:00Z  change_request_opened  #7  {"title":"x"}`, then `2026-11-21T00:00:01Z  policy_updated  -  {}`, then `next cursor: 6`

#### Scenario: Empty page
- **WHEN** a single read with `--after 6 --wait 2` returns no events and `next_cursor` 6
- **THEN** goodflows prints only `next cursor: 6` and exits 0

### Requirement: Each page prints as one JSON event
With `--json`, each printed page SHALL be one JSON line with `type` `project_events`, `events`, the response's `data` as sent, and `next_cursor`, the response's `next_cursor`.

#### Scenario: Page as JSON
- **WHEN** a user runs `goodflows events --json` and the server returns events 5 and 6 with `next_cursor` 6
- **THEN** goodflows prints one JSON line with `type` `project_events`, `events` holding both events with their `id`, `type`, `number`, `data` and `inserted_at`, and `next_cursor` `6`

### Requirement: A response without an event list fails
When a successful response's `data` is not a list, goodflows SHALL fail with exit 1 and print `events failed: the server's response is not a list of events`, also under `--follow`.

#### Scenario: Object instead of list
- **WHEN** the server responds `200` with `{"data": {}, "next_cursor": null}`
- **THEN** goodflows prints `events failed: the server's response is not a list of events` and exits 1

### Requirement: --follow keeps reading from the next cursor
With `--follow`, goodflows SHALL read again after every page, sending as `after` the page's `next_cursor` when it is an integer and otherwise the cursor it last sent. It SHALL print each page that has events and SHALL NOT print an empty page. It SHALL stop only when interrupted or when a read fails, which exits 1 as the change-requests capability specifies.

#### Scenario: Cursor advances
- **WHEN** a user runs `goodflows events --after 3 --follow --json` and the first page holds event 5 with `next_cursor` 5
- **THEN** the second request is `GET /api/v1/projects/<project>/events?after=5&wait=30`

#### Scenario: Quiet long-poll
- **WHEN** under `--follow` a long-poll after cursor 6 returns no events and `next_cursor` 6
- **THEN** goodflows prints nothing for it and sends `GET .../events?after=6&wait=30` again

#### Scenario: Failure ends the follow
- **WHEN** under `--follow` a read responds `401`
- **THEN** goodflows prints the server error naming `events` and exits 1

### Requirement: --follow skips events deleted by retention
Events are kept 90 days. When a read under `--follow` responds `410` with an integer `latest_cursor`, goodflows SHALL print a page with no events, `next_cursor` `<latest_cursor>`, `skipped` true and `note` `events after cursor <after> were deleted by retention and skipped; resync from current state; continuing after <latest_cursor>`, then continue with `after` set to `latest_cursor`.

#### Scenario: Cursor fell out of retention
- **WHEN** a user runs `goodflows events --after 3 --follow --json`, the read after 5 responds `410` with `{"errors": {"detail": "..."}, "latest_cursor": 90}`
- **THEN** goodflows prints a JSON line with `events` `[]`, `next_cursor` `90`, `skipped` `true` and a `note` naming cursor 5 and `continuing after 90`, and its next request is `GET .../events?after=90&wait=30`

#### Scenario: Skip note as text
- **WHEN** the same `410` arrives for `goodflows events --after 3 --follow` without `--json`
- **THEN** goodflows prints `Note: events after cursor 3 were deleted by retention and skipped; resync from current state; continuing after 90` and then `next cursor: 90`

### Requirement: A single read of deleted events fails naming latest_cursor
Without `--follow`, a `410` with an integer `latest_cursor` SHALL fail with exit 1 after that one request and print `events failed: the events after cursor <after> were deleted by retention; resync from current state, then read with --after <latest_cursor> (latest_cursor)`, with `(none)` for `<after>` when no cursor was sent.

#### Scenario: Deleted cursor
- **WHEN** a user runs `goodflows events --after 3` and the server responds `410` with `latest_cursor` 90
- **THEN** goodflows sends one request, prints `events failed: the events after cursor 3 were deleted by retention; resync from current state, then read with --after 90 (latest_cursor)` and exits 1

### Requirement: A 410 without a latest cursor is a plain server error
When a `410` response has no integer `latest_cursor`, with or without `--follow`, goodflows SHALL fail with exit 1 and print the server error as the change-requests capability specifies, `events failed: gone (HTTP 410): <body>`.

#### Scenario: Null latest cursor
- **WHEN** a user runs `goodflows events --after 1` and the server responds `410` with `{"errors": {"detail": "gone"}, "latest_cursor": null}`
- **THEN** goodflows prints `events failed: gone (HTTP 410): {"errors":{"detail":"gone"},"latest_cursor":null}` and exits 1
