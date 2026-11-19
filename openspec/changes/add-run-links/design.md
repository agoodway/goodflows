# Design

## Context

See proposal.md for why. What exists today:

- `history::Record` (`src/history.rs`) is the whole contents of the history file, and, with the `Upload` keys `source.branch`, `ci` and `graph`, the uploaded record. Its doc comment calls the field list an allowlist, so nothing else can leak in.
- Every invocation, root or child, runs `invocation()` in `src/main.rs` and captures its own `source`. It reuses the reporting session's capture when there is one, and otherwise calls `reporting::source::capture` when that invocation's history is on. `source::capture` runs `git rev-parse`, `symbolic-ref` and `status` in one `tokio::join!` under a 2-second `GIT_TIMEOUT`, and returns `Source::default()` when the timeout fires. `Source` is also written into the spool manifest and each report header, both with `deny_unknown_fields`.
- The only value shared by every invocation of a run is on `Run`. `Run.provenance` carries a remote Sprite run's snapshot source, which `sprite::snapshot::provenance` builds on the controller, not `source::capture`.
- `history` can be set per workflow and per profile, so a root with history off can call a child with history on.
- `Session::uploads_run_record()` is true whenever `[reporting] history = true`, including when the upload is skipped for missing credentials.
- `reporting::ci::capture` reads only the caller's process environment, never settings `env`, and leaves out any value that fails its rule.
- `run_contract::validate` mirrors the goodflows_web contract and rejects unknown keys. The server does the same, so a new key needs the server change first. A run record that fails validation at run time prints `upload skipped: invalid record at <path>`. On replay it counts as `invalid_record` without a path.
- Run flags such as `--history` are global clap args. `Cli::run_only_flag()` lists them, and `last`, `status`, `wait`, `cancel`, `reports upload`, `history list`, `history show`, `skills install`, `agents`, `mcp` and the Sprite commands reject them through it. `validate` and `list` reject only `--log-dir` and accept the rest.
- For a `pk_` key the server returns redacted text as null, and `output::history` prints null as `-`.
- goodflows already detects an Amp session through `AMP_CURRENT_THREAD_ID` (`src/skills.rs`).

In this repo, `Amp-Thread-ID` lines hold full URLs (`https://ampcode.com/threads/T-<uuid>`). GitHub's default squash message copies every commit message into the squash commit's body, so those lines survive on `main` in the body, not in the trailer block. The squash commits for PRs #41, #42, #44 and #45 carry them; #43 has none.

## Goals / Non-Goals

**Goals:**
- A run records the agent threads and tickets behind the change it tests, in both the local file and the upload.
- Runs started by an agent, runs on a branch that Amp committed to, and runs on `main` after a squash merge link their threads with no setup.
- A server search by ticket or thread finds the PR runs and the `main` runs of a change.
- Nothing secret reaches the record: no query strings, no credentials in URLs, no environment values other than the link variables themselves.
- Collecting links never changes `source` and never fails a run.

**Non-Goals:**
- Recovering threads when a repo edits its squash messages down to the title. That needs server-side backfill, which can come later without changing this record shape.
- Following ticket relations, such as a bug filed as a new Linear issue that points back to the original ticket.
- Reading the PR title or body from `GITHUB_EVENT_PATH`.
- Agent-specific variables for agents other than Amp. Those agents can set `GOODFLOWS_THREAD_URLS`.
- Adding links to the `summary` event or the last-run record.

## Decisions

### Record shape

```json
"links": {
  "threads": ["https://ampcode.com/threads/T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b"],
  "tickets": ["GOO-291"]
}
```

`links` comes after `ci` and before `results` in both the file and the upload. It is omitted when both lists are empty, and each list is omitted when it is empty. `schema_version` stays 1, because older readers ignore the optional key.

Rules, which `run_contract` and the server share:

| Field | Rule |
|---|---|
| `links` | object with only `threads` and `tickets`, at least one present |
| `threads` | 1 to 32 entries, unique |
| thread URL | `https`, has a host, no userinfo, no query, no fragment (an empty `?` or `#` counts), path characters as below, already in normalized form, at most `MAX_URL_BYTES` (2048) |
| `tickets` | 1 to 32 entries, unique ignoring ASCII case |
| ticket | 1 to 64 bytes of printable ASCII (`!` to `~`) other than `,`, so no whitespace, control characters or commas |

**Path characters.** Every path byte is an RFC 3986 `pchar` (unreserved, sub-delims, `:` or `@`) or `/`, and every `%` starts a `%` plus two hex digits. That excludes `[`, `]`, `|`, `^`, `` ` ``, `{`, `}`, `\` and a bare or malformed `%` (`%zz`, a trailing `%`). `reqwest::Url` follows WHATWG and leaves several of these as written, and goodflows_web parses URLs as RFC 3986 (`URI.new/1`). A URL that passes here therefore can never get a 422 on upload. A 422 leaves the record pending, and `reports upload` retries it forever.

**Normalized form** is the form `reqwest::Url` produces:

- a lowercase host
- no explicit `:443`
- a nonempty path (`https://h` becomes `https://h/`)
- no `.` or `..` segments
- percent-encoded triplets kept exactly as written: no decoding (`%7E` stays `%7E`, not `~`) and no change of hex case

Collection stores thread URLs in this form, so `HTTPS://AmpCode.com/...` and `https://ampcode.com/...` deduplicate. The server never rewrites a submitted value, so `run_contract` requires a URL to equal its own normalized form instead of normalizing it. Precisely, a URL is normalized when `Url::parse(s)` succeeds and `as_str()` equals `s`. Besides the cases above, that rejects every other input WHATWG rewrites:

- `%2e` dot segments (`/a/%2E%2e/b`)
- a percent-encoded host (`ex%61mple.com`)
- non-canonical IPv4 (`127.1`, `0x7f.0.0.1`, `127.0.0.1.`)
- non-canonical IPv6 (`[0:0:0:0:0:0:0:1]`, `[::FFFF:1.2.3.4]`)
- ports with leading zeros (`:080`, `:0443`)

goodflows_web's "must be normalized" check rejects the same set. The shared fixture pins the definition with `%7E`, `%2f` and `%2E%2e` cases.

Why lists: one change often spans several threads, such as a plan thread, an implementation thread and a review thread, and it can close more than one ticket.

Why reject a query or fragment instead of stripping it: a query can carry a token. An explicit value with one is a mistake worth reporting. A collected value with one is dropped.

Why no comma in a ticket: the environment variables split on commas, so `--ticket GOO-1,GOO-2` would otherwise record one ticket from a flag and two from the variable.

Tickets keep their digits as written. `GOO-0291` and `GOO-291` are different tickets; the server matches only ASCII case.

### When and where links are collected

Links belong to the run, not to an invocation. `Run` gains `links: tokio::sync::OnceCell<Option<Links>>`. An invocation reads it with `get_or_init` only when it is about to build a record: when it writes a history file, or when its reporting session will send a run record. A session whose upload is skipped for missing credentials does not count. So:

- Collection runs at most once per run, however many child invocations there are, and in parallel children the first one to need it collects.
- A run whose root has history off but whose called child has it on still gets links, from the child.
- A run where no invocation records runs no git command for links, even when test reports are configured.

`HistoryTarget` gains `links: Option<&'a Links>`, filled from `Run.links`. Both the history file and the uploaded record are built from the same `HistoryTarget`, so they hold the same `links`.

`reporting::source::capture` and `Source` are unchanged. Adding commit messages to `Source` would write them into the spool manifest and report headers, and adding a git call to its `join!` would let a slow scan wipe `commit`, `branch` and `dirty` through the shared timeout.

### Sources and their order

Sources are merged in this order, and the first spelling of a duplicate wins:

1. **Flags**: repeatable `--ticket ID` and `--thread-url URL`. clap's value parser checks each value, so an invalid one fails with exit 2 before any job runs. More than 32 of either fails with exit 2 through `usage()`.
2. **Caller environment**: `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS`, split on commas and whitespace. Empty pieces are ignored and do not count as entries. They are read from the process environment only, never settings or job `env`, as `ci` is. An invalid entry is dropped with a warning on stderr that names the variable and the entry's position, counting from 1, never its value. A bad CI variable shouldn't fail the build.
3. **Agent session**: the thread ID is `AMP_CURRENT_THREAD_ID`, or `AMP_THREAD_ID` when that is unset, and must be `T-` followed by a lowercase UUID. The origin is `AMP_URL`, defaulting to `https://ampcode.com`. It must parse as an `https` URL with a host, no userinfo, query or fragment, and a path of `/` or nothing; a trailing slash is allowed. The thread URL is built by setting the parsed origin's path to `/threads/<id>`, as `ci::run_url` builds its URL, then checked against the thread URL rule. When any check fails, the session adds nothing.
4. **Git**: see below.

Collection takes the environment through a lookup closure, as `ci::capture` does, so tests never set process variables. Values from steps 2 to 4 that pass the rules are added until a list holds 32. Anything past that is dropped silently. Flag values come first, so they are never dropped.

### Git scan

`links::scan` runs in its own `tokio::time::timeout` of 2 seconds, in the invocation's workdir, with `GIT_OPTIONAL_LOCKS=0` and the reporting key's variable removed from git's environment, as `source::capture` does. A timeout or any git failure returns no commits and has no effect on `source`.

1. If `git rev-parse --is-shallow-repository` prints `true`, scan `HEAD` alone. With a partial depth, `HEAD --not <base>` can list old base commits below the shallow boundary and link unrelated runs.
2. Otherwise resolve `<base>`: `refs/remotes/origin/$GITHUB_BASE_REF` when `GITHUB_BASE_REF` is set, then `refs/remotes/origin/HEAD`. When neither resolves, scan `HEAD` alone.
3. Run `git log -z --max-count=200 --format=%B HEAD --not <base>`. When it lists nothing, because `HEAD` is reachable from `<base>` as on `main`, scan `HEAD` alone.

Scanning `HEAD` alone is `git log -z -1 --format=%B HEAD`. Messages are read newest first.

- **Threads**: every message line that starts, after optional whitespace, with `Amp-Thread-ID:`. Matching any line, not only the trailer block, is what finds the thread lines in a squash commit's body. A value is either a thread URL, which must pass the rule, or a bare `T-<uuid>`, expanded with the agent session's origin.
- **Tickets**: only in commit subjects (first lines), in `source.branch`, and in `GITHUB_HEAD_REF` from the caller's environment. Bodies are skipped because they often mention other tickets ("follow-up to GOO-200"), which would link unrelated runs. A squash body's copied subjects usually repeat the PR's own ticket, which the squash subject already supplies.

What this gives on GitHub Actions:

| Run | Default checkout (`fetch-depth: 1`, shallow) | `fetch-depth: 0` |
|---|---|---|
| Pull request | tickets from `GITHUB_HEAD_REF`; `HEAD` alone is GitHub's merge commit, which has neither | plus threads and tickets from every PR commit |
| Push to `main` after a squash | the squash subject's ticket and the thread lines in its body | the same |

### Ticket prefixes

`[settings] ticket_prefixes = ["GOO"]` is valid only in the top-level `[settings]`, like `sprite_prefix`, because ticket IDs belong to the project, not to a workflow or profile. Like `sprite_prefix`, it is not part of the effective settings that inspection shows. Each prefix is 1 to 16 ASCII alphanumerics starting with a letter, and duplicates ignoring case are rejected at load.

A match is the prefix, ignoring ASCII case, then `-`, then one or more ASCII digits. It must not be preceded or followed by an ASCII alphanumeric. The result is stored as the configured prefix, `-`, and the digits as written. So `tbrewer/goo-291-fix` gives `GOO-291`, and `MEGOO-12`, `GOO-`, and `GOO-12b` give nothing.

Without `ticket_prefixes`, git supplies no tickets. A general pattern such as `[A-Z]+-\d+` matches `UTF-8` and `SHA-256`, both of which appear in this repo's README. Using fixed prefixes also avoids adding a regex dependency.

### Flags for recording and filtering

`--ticket` and `--thread-url` are global args with ids `ticket` and `thread_url`, repeatable, next to `--history`. On a run, they record links. They join `run_only_flag()`, so every command that rejects run flags today rejects them too. `validate` and `list` accept and ignore them, as they do `--history`.

`history list` declares its own `#[arg(long, id = "ticket")] ticket: Option<String>` and `#[arg(long = "thread-url", id = "thread_url")] thread_url: Option<String>` in its filters. clap does not propagate a global arg into a subcommand that defines the same id, so:

- `history list --ticket GOO-291` fills the filter. A second `--ticket` fails with clap's own "cannot be used multiple times" error and exit 2.
- The value parser applies the link rules, so a bad value exits 2 without a request.
- `history list --help` documents the filters.

`history list` therefore keeps using `run_only_flag()` unchanged: the global fields stay empty when clap gave the values to the filter. A test pins `goodflows --ticket X history list`, whichever way clap resolves it, to either filtering by `X` or exiting 2, never to a silently dropped value.

Alternative: separate names such as `--filter-ticket`. Rejected because it gives two names for the same value, and `--ticket GOO-291` reads correctly in both places.

### Sprite runs

The controller collects links itself with the same collection function, on its own checkout and environment, when the run it sends can record: when history is on for the selected root or any workflow it can call under the selected profile, by setting or flag, or when `[reporting] history = true`. Otherwise it sends none.

`sprite::control::Provenance` gains `links: Option<Links>`, skipped when `None`, set next to the snapshot's commit, branch and dirty. On the remote runner, `execute` initializes `Run.links` from `Provenance.links` (`None` means no links), so the remote never scans git. Its snapshot has no history to scan.

`Provenance` has `deny_unknown_fields`, and the remote runner is the same `CARGO_PKG_VERSION`, not necessarily the same code. Released versions always match. A development build that falls back to the published runner of its version, as non-Linux controllers do, would send `links` to a runner that rejects the field when it reads the header. That is the case docs/sprites.md already warns about for any unreleased change, and `links` is omitted when empty, so runs without links still work. No `PROTOCOL` bump: it would make every fallback for that version fail.

### Server contract

The companion goodflows_web change is `openspec/changes/accept-run-links/` in that repo:

- `POST /workflow-runs` accepts `links` with the rules above, and rejects an empty `links` or unknown keys inside it with 422 and the path, such as `links.tickets[2]`. A thread URL that is not already normalized gets "must be normalized", and a repeated entry gets "must be unique" on the repeat's path.
- The server stores both lists as written and indexes them for exact lookup. Tickets match ignoring ASCII case. The `thread_url` filter fixes only scheme and host case, an empty or `443` port, and an empty path, then matches exactly; any other non-normalized value gets 422. `%7E` and `%2f` match only as written. The CLI's `--thread-url` filter normalizes with `reqwest::Url` before sending, so it never hits that 422.
- Because the server stores tickets as written, a retry that differs only in a ticket's case is a 409, not an idempotent 200.
- `GET /workflow-runs` accepts `ticket` and `thread_url`. Both filter for every key type.
- `GET /workflow-runs/:id` returns `links` under `data.document.links`, as stored. For a `pk_` key, `links.threads` is null only when the run has threads, and `links.tickets` is unchanged. A tickets-only `links` gets no `threads` key. The list response needs no new fields.
- Stored runs and older clients without `links` behave as before. The new columns default to empty, so nothing is backfilled.

A `pk_` key can still filter by `thread_url`. That only confirms the existence of a run for a URL the caller already has, and thread IDs are random UUIDs.

### `history show`

After the `ci` lines, `show` prints a `tickets` row with the tickets joined by `, `, and one `thread` row per URL. When `links.threads` is present and null, it prints one `thread` row with `-`. A missing `threads` key prints no `thread` row. When the run has no `links`, it prints neither row. All values pass through `visible`, as every other server string does. `--json` output is unchanged: the server's `data` as returned.

### Test isolation

`AMP_CURRENT_THREAD_ID`, `AMP_THREAD_ID` and `AMP_URL` are set in every Amp session, and `GITHUB_HEAD_REF` and `GITHUB_BASE_REF` on PR CI. Integration tests compare history files and uploaded records key by key, so these must not reach a goodflows under test. `tests/common::Project::command`, the reporting `history_command` helper and the Sprite test harness remove them along with `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS`. In-process tests pass collection a lookup closure instead of reading `std::env`.

## Risks / Trade-offs

- [An older server rejects every record that has links, with 422] → Ship goodflows_web first. Records already in a spool are replayed by `reports upload` once the server accepts them. A CI spool that is thrown away loses them.
- [The server and CLI disagree on normalized form or path characters, so valid-looking records get 422] → One definition, written above, and a shared fixture that both repos test against, with `[`, `%zz`, a trailing `%`, `%7E`, `%2f`, an uppercase host and `:443`.
- [Wrong links: a long-lived branch accumulates unrelated threads, or a subject names the wrong ticket] → Commits are limited to the branch's own commits since `<base>`, at most 200, shallow checkouts scan only `HEAD`, and lists hold at most 32 entries. Flags let a caller state links explicitly when it matters.
- [A `main` run links every thread copied into its squash body] → Those are the threads that wrote the change, which is the point.
- [Thread URLs in committed history files] → The same URLs already appear in those commits' messages. A run started in an agent session that never committed adds its own URL. Projects that commit history files should know that.
- [Extra git calls on runs that record] → At most four, once per run, under their own 2-second budget, and never when nothing records.
- [Repos that trim squash messages lose threads on `main`] → Their `main` runs keep the ticket, and the ticket search reaches the PR runs that have threads.

## Migration Plan

1. Land the goodflows_web change and deploy it.
2. Land this change. No config change is required. Projects add `ticket_prefixes` to get tickets from git, and set `fetch-depth: 0` in PR CI to get threads from PR commits.
3. Existing history files and spools are unchanged. Records written before the upgrade simply have no `links`.
