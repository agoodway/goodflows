# Spec Delta

## ADDED Requirements

### Requirement: Runs record their links
When a run writes a history file or sends a run record, the record SHALL carry a `links` object with `threads`, a list of agent thread URLs, and `tickets`, a list of ticket IDs. The history file and the uploaded record SHALL hold the same `links` value, placed after `ci` and before `results`. `links` SHALL be omitted when both lists are empty, and each list SHALL be omitted when it is empty. Every invocation of a run SHALL record the same links. `schema_version` SHALL stay 1.

#### Scenario: File and upload agree
- **WHEN** local history and upload are both enabled and the run has a ticket and a thread
- **THEN** the history file and the uploaded record have the same `links` value, and removing `graph`, `ci` and `source.branch` from the uploaded record still leaves the same JSON value as the history file

#### Scenario: No links
- **WHEN** no flag, environment variable, agent session or commit supplies a link
- **THEN** neither the history file nor the uploaded record has a `links` key

#### Scenario: Child invocation
- **WHEN** a run with ticket `GOO-291` calls workflow `suite`
- **THEN** the history file for `suite` has `links.tickets` `["GOO-291"]`

#### Scenario: Only the child records
- **WHEN** the root workflow has history off, it calls workflow `suite` whose settings turn history on, and the run passes `--ticket GOO-291`
- **THEN** the history file for `suite` has `links.tickets` `["GOO-291"]`

### Requirement: Thread URLs are normalized and RFC 3986 safe
A thread URL MUST be `https`, have a host, have no userinfo, query or fragment, even an empty one, and be at most 2048 bytes. Every path byte MUST be an RFC 3986 `pchar` or `/`, and every `%` MUST start a `%` plus two hex digits. Normalized form SHALL be a lowercase host, no explicit `:443`, a nonempty path, no `.` or `..` segments, and percent triplets kept as written. Collection SHALL store that form, and upload validation SHALL reject, not rewrite, a URL not already in it.

#### Scenario: Characters outside RFC 3986
- **WHEN** a collected thread URL has `|`, `[` or `%zz` in its path, or ends with `%`
- **THEN** it is not recorded

### Requirement: Thread URL hosts are RFC 3986 safe
A thread URL's host MUST be either a bracketed IPv6 address or made only of RFC 3986 unreserved and sub-delims characters (`A-Z`, `a-z`, `0-9`, `-._~!$&'()*+,;=`). Collection SHALL drop, and upload validation SHALL reject, any other host, including one with `"`, `` ` ``, `{` or `}`, which WHATWG keeps as written and RFC 3986 parsers reject.

#### Scenario: Brace in the host
- **WHEN** upload validation (`run_contract::validate`) checks a record whose `links.threads[0]` is `https://a{b.com/x`
- **THEN** it rejects the record at path `links.threads[0]`, so the record is not sent

#### Scenario: Duplicate URL spellings
- **WHEN** the collected threads are `HTTPS://AmpCode.com/threads/T-<uuid>` and `https://ampcode.com/threads/T-<uuid>`
- **THEN** `links.threads` holds one entry, `https://ampcode.com/threads/T-<uuid>`

#### Scenario: Percent triplets are kept
- **WHEN** a collected thread URL is `https://h/a%7Eb`
- **THEN** it is recorded as `https://h/a%7Eb`, not `https://h/a~b`

#### Scenario: Upload validation
- **WHEN** upload validation (`run_contract::validate`) checks a record whose `links.threads[0]` has a query string, or is `https://AmpCode.com:443/threads/T-<uuid>` or `https://h/a/%2E%2e/b`
- **THEN** it rejects the record at path `links.threads[0]`, so the record is not sent

### Requirement: Tickets and link lists are bounded
A ticket MUST be 1 to 64 bytes of printable ASCII other than `,`, so it has no whitespace, control characters or commas. Each list SHALL hold at most 32 entries without duplicates. Tickets SHALL be compared ignoring ASCII case, and the first spelling SHALL be kept. Upload validation SHALL apply the same rules.

#### Scenario: Duplicate ticket in an upload
- **WHEN** upload validation (`run_contract::validate`) checks a record whose `links.tickets` is `["GOO-1", "goo-1"]`
- **THEN** it rejects the record at path `links.tickets[1]`, so the record is not sent

#### Scenario: Comma in a ticket flag
- **WHEN** a run passes `--ticket GOO-1,GOO-2`
- **THEN** the command exits 2 and no job runs

### Requirement: Links are collected once per run, only when needed
goodflows SHALL collect links at most once per run, when the first invocation that writes a history file or sends a run record needs them. An upload skipped for missing credentials MUST NOT count as sending. When no invocation records, goodflows MUST NOT run any git command to collect links. Collecting links MUST NOT change `source` and MUST NOT fail the run.

#### Scenario: History off
- **WHEN** neither the history file nor the upload is enabled, and a job declares `reports`
- **THEN** goodflows runs no git command to collect links

#### Scenario: Upload without credentials
- **WHEN** `[reporting] history = true`, the key variable is unset, and the history file is off
- **THEN** goodflows runs no git command to collect links

#### Scenario: Several child invocations
- **WHEN** a run with history on calls three workflows
- **THEN** links are collected once, and all four history files have the same `links`

### Requirement: Links are merged in a fixed order
goodflows SHALL merge, in this order: repeatable `--ticket` and `--thread-url` flags; `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS` from the caller's process environment, split on commas and whitespace with empty pieces ignored; the agent session; then git. Settings and job `env` MUST NOT supply these values. Once a list holds 32 entries, later values SHALL be dropped.

#### Scenario: Flags come first
- **WHEN** a run passes `--ticket goo-291` and `GOODFLOWS_TICKETS=GOO-291,GOO-300`
- **THEN** `links.tickets` is `["goo-291", "GOO-300"]`

#### Scenario: Settings env is ignored
- **WHEN** `[settings] env` sets `GOODFLOWS_TICKETS = "GOO-9"` and the caller's environment does not
- **THEN** the record has no `links.tickets`

#### Scenario: Cap across sources
- **WHEN** a run passes 30 `--ticket` flags and `GOODFLOWS_TICKETS` holds 5 other valid tickets
- **THEN** `links.tickets` holds the 30 flag tickets and the first 2 from the variable

### Requirement: Invalid link input is handled by source
An invalid `--ticket` or `--thread-url` value, more than 32 of either flag, or either flag given both before and after the subcommand, MUST fail as an argument error with exit code 2 before any job runs. An invalid environment entry SHALL be dropped with a stderr warning that names the variable and the entry's position among the non-empty entries, counting from 1, and MUST NOT print the entry. Invalid values from the agent session or git SHALL be dropped silently.

#### Scenario: Invalid flag
- **WHEN** a run passes `--thread-url 'https://ampcode.com/threads/T-1?token=x'`
- **THEN** the command exits 2 and no job runs

#### Scenario: Too many flags
- **WHEN** a run passes 33 `--ticket` flags
- **THEN** the command exits 2 and no job runs

#### Scenario: Flags on both sides of the subcommand
- **WHEN** a run passes `--ticket A run --ticket B`
- **THEN** the command exits 2 and no job runs

#### Scenario: Invalid environment entry
- **WHEN** `GOODFLOWS_TICKETS` is `GOO-1,,` followed by a 65-byte entry
- **THEN** `links.tickets` is `["GOO-1"]`, stderr warns about entry 2 of `GOODFLOWS_TICKETS` without printing it, and the exit code is unaffected

### Requirement: The agent session supplies its thread
The thread ID SHALL be `AMP_CURRENT_THREAD_ID`, or `AMP_THREAD_ID` when that is unset, and MUST be `T-` followed by a lowercase UUID. The origin SHALL be `AMP_URL`, defaulting to `https://ampcode.com`. It MUST be an `https` URL with a host, no userinfo, query or fragment, and a path of `/` or nothing. The session SHALL add the origin with the path `/threads/<id>`, and SHALL add nothing when a check fails.

#### Scenario: Agent session
- **WHEN** a run with history enabled starts with `AMP_URL=https://ampcode.com/` and `AMP_CURRENT_THREAD_ID=T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b`
- **THEN** `links.threads` is `["https://ampcode.com/threads/T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b"]`

#### Scenario: Origin with a path
- **WHEN** `AMP_URL` is `https://ampcode.com/app` or `http://ampcode.com`
- **THEN** the agent session adds no thread

#### Scenario: Not an agent session
- **WHEN** neither `AMP_CURRENT_THREAD_ID` nor `AMP_THREAD_ID` is set
- **THEN** the agent session adds no thread

### Requirement: Git scans only the branch's own commits
goodflows SHALL read the messages of at most 200 commits reachable from `HEAD` but not from the base, newest first. The base is `refs/remotes/origin/$GITHUB_BASE_REF` when `GITHUB_BASE_REF` is set, otherwise `refs/remotes/origin/HEAD`. goodflows SHALL read `HEAD` alone when the checkout is shallow, no base resolves, or the range is empty. The scan SHALL have its own 2-second timeout and SHALL remove the reporting key variable from git's environment.

#### Scenario: Run on main after a squash merge
- **WHEN** `HEAD` is reachable from the base, its subject is `GOO-291: Fix sprite test (#45)`, its body has an `Amp-Thread-ID:` line among the copied commit messages, and `ticket_prefixes = ["GOO"]`
- **THEN** `links.tickets` is `["GOO-291"]` and `links.threads` holds that line's URL

#### Scenario: Squash message trimmed to the title
- **WHEN** `HEAD` is reachable from the base and its message is `GOO-291: Fix sprite test (#45)` alone, and `ticket_prefixes = ["GOO"]`
- **THEN** `links.tickets` is `["GOO-291"]` and there is no `links.threads`

#### Scenario: Shallow checkout
- **WHEN** the checkout is shallow and `origin/HEAD` resolves
- **THEN** only `HEAD`'s message is read

#### Scenario: Not a git checkout
- **WHEN** the workdir is not inside a git checkout
- **THEN** the run succeeds and only flags, the environment and the agent session supply links

#### Scenario: Slow git
- **WHEN** the scan does not finish within 2 seconds
- **THEN** git supplies no links and the record's `source` is the same as without the scan

### Requirement: Git supplies threads from Amp-Thread-ID lines
Every scanned message line that starts, after optional whitespace, with `Amp-Thread-ID:` SHALL supply a thread, including lines in the body that are not git trailers. Its value SHALL be either a valid thread URL as given, or a bare `T-<uuid>` expanded with the agent session's origin.

#### Scenario: Feature branch with Amp commits
- **WHEN** `origin/HEAD` points at the base commit and the branch has two commits with different `Amp-Thread-ID` trailers
- **THEN** `links.threads` holds both URLs, the newer commit's first

### Requirement: Git supplies tickets from subjects and branch names
Tickets SHALL come only from scanned commit subjects, the checked-out branch name, and `GITHUB_HEAD_REF` from the caller's environment, and only for prefixes in `ticket_prefixes`. Commit bodies MUST NOT supply tickets.

#### Scenario: Branch name
- **WHEN** `ticket_prefixes = ["GOO"]` and the checked-out branch is `tbrewer/goo-291-fix`
- **THEN** `links.tickets` contains `GOO-291`

#### Scenario: Ticket in a commit body
- **WHEN** a branch commit's body, but not its subject, says `follow-up to GOO-200`
- **THEN** `GOO-200` is not recorded

### Requirement: Ticket prefixes are configured per project
`ticket_prefixes` SHALL be valid only in the top-level `[settings]`. Each prefix MUST be 1 to 16 ASCII alphanumerics starting with a letter, and the list MUST NOT contain duplicates ignoring case. A match SHALL be the prefix ignoring ASCII case, `-`, then one or more ASCII digits, not preceded or followed by an ASCII alphanumeric. A match SHALL be recorded as the configured prefix, `-`, and the digits as written. Without `ticket_prefixes`, git SHALL supply no tickets.

#### Scenario: Prefix boundaries
- **WHEN** `ticket_prefixes = ["GOO"]` and a subject reads `MEGOO-12 GOO-7, goo-0042b and goo-15`
- **THEN** the tickets recorded from it are `GOO-7` and `GOO-15`

#### Scenario: Misplaced setting
- **WHEN** a workflow or profile sets `ticket_prefixes`
- **THEN** validation fails before any job runs

#### Scenario: No prefixes
- **WHEN** `ticket_prefixes` is unset and a subject reads `GOO-291: Fix UTF-8 handling`
- **THEN** no ticket is recorded

### Requirement: Sprite runs carry the controller's links
The Sprite controller SHALL collect links itself when the run it sends can record, and send them in the provenance. The remote runner's history file and uploaded record SHALL use those links unchanged, and the remote runner MUST NOT collect links itself.

#### Scenario: Remote history file
- **WHEN** a `--sprite` run with history enabled starts in an agent session on a branch with ticket `GOO-291`
- **THEN** the remote invocation's history file has the controller's `links`

#### Scenario: Nothing records
- **WHEN** a `--sprite` run has history and the upload off
- **THEN** the controller runs no git command to collect links and the provenance has no `links`

### Requirement: Stored runs can be filtered by link
`goodflows history list` SHALL send `--ticket` as the `ticket` query parameter and `--thread-url` as `thread_url`, alongside the existing filters. Each SHALL take at most one value and follow the link rules, and SHALL be the filter only when typed exactly once on the command line. A bad value or a filter typed more than once, on either side of the subcommand, MUST exit 2 without a request. `history show` SHALL reject both flags as run flags.

#### Scenario: Filter by ticket
- **WHEN** the user runs `goodflows history list --ticket GOO-291`
- **THEN** one request is sent with `ticket=GOO-291`

#### Scenario: Repeated link filter
- **WHEN** the user runs `goodflows history list --ticket GOO-1 --ticket GOO-2`
- **THEN** no request is sent and the command exits 2

#### Scenario: Link filter on both sides of the subcommand
- **WHEN** the user runs `goodflows --ticket GOO-1 history list --ticket GOO-2`
- **THEN** no request is sent and the command exits 2

#### Scenario: Show rejects link flags
- **WHEN** the user runs `goodflows history show <run_id> --ticket GOO-1`
- **THEN** no request is sent and the command exits 2

### Requirement: A stored run shows its links
In text mode, `goodflows history show` SHALL read `links` from the run's `document`. After the `ci` rows, it SHALL print a `tickets` row when `links.tickets` is present, and one `thread` row per thread URL. When `links.threads` is present and null, as the server returns it for a `pk_` key, it SHALL print one `thread` row as absent. A missing `threads` key SHALL print no `thread` row. `--json` output SHALL stay the server's `data` unchanged.

#### Scenario: Public key
- **WHEN** the configured key is a `pk_` key and the run has tickets and threads
- **THEN** the tickets are printed in full and the thread row prints `-`

#### Scenario: Public key, tickets only
- **WHEN** the configured key is a `pk_` key and the run's `links` has tickets and no `threads` key
- **THEN** the tickets are printed and no `thread` row is printed

#### Scenario: Run without links
- **WHEN** the stored run has no `links`
- **THEN** no `tickets` or `thread` row is printed
