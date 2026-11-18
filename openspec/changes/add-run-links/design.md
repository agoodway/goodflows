# Design

## Context

See proposal.md for why. What exists today:

- `history::Record` (`src/history.rs`) is the whole contents of the history file, and, with the `Upload` keys `source.branch`, `ci` and `graph`, the uploaded record. Its doc comment calls the field list an allowlist, so nothing else can leak in.
- `reporting::source::capture` runs `git rev-parse`, `symbolic-ref` and `status` under a 2-second `GIT_TIMEOUT`, with the reporting key's variable removed from git's environment. Its result reaches child invocations through `HistoryTarget.source`, and reaches a Sprite run through `sprite::control::Provenance`.
- `reporting::ci::capture` reads only the caller's process environment, never settings `env`, and leaves out any value that fails its rule.
- `run_contract::validate` mirrors the goodflows_web contract and rejects unknown keys. The server does the same, so a new key needs the server change first.
- Run flags such as `--history` are global clap args, and the non-run commands reject them by name.
- For a `pk_` key the server returns redacted text as null, and `output::history` prints null as `-`.

In this repo, `Amp-Thread-ID` trailers hold full URLs (`https://ampcode.com/threads/T-<uuid>`). Squash merges onto `main` keep the PR title with its `GOO-###` prefix and drop the trailers.

## Goals / Non-Goals

**Goals:**
- A run records the agent threads and tickets behind the change it tests, in both the local file and the upload.
- Runs started by an agent, and runs on a branch that Amp committed to, link their threads with no setup.
- A server search by ticket finds both the PR runs, which have threads, and the `main` runs, which have the ticket.
- Nothing secret reaches the record: no query strings, no credentials in URLs, no environment values other than the link variables themselves.

**Non-Goals:**
- Recovering threads for squash-merged `main` runs. That needs a GitHub setting change or server-side backfill. Both can come later without changing this record shape.
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
| thread URL | `https`, has a host, no userinfo, no query, no fragment, only RFC 3986 characters, already in normalized form, at most `MAX_URL_BYTES` (2048) |
| `tickets` | 1 to 32 entries, unique ignoring ASCII case |
| ticket | 1 to 64 bytes of printable ASCII (`!` to `~`), so no whitespace or control characters |

Collection stores thread URLs in `reqwest::Url`'s normalized form, so `HTTPS://AmpCode.com/...` and `https://ampcode.com/...` deduplicate. Normalized form means:

- a lowercase host
- no explicit `:443`
- a nonempty path
- no `.` or `..` segments

The server never rewrites a submitted value, so `run_contract` requires a URL to equal its own normalized form instead of normalizing it.

goodflows_web parses URLs as RFC 3986 (`URI.new/1`), while `reqwest::Url` follows WHATWG and leaves `|` and `^` in a path unencoded. Collection therefore also rejects any character outside RFC 3986's unreserved, reserved and `%` set, so an accepted URL never gets a 422 on upload.

Why lists: one change often spans several threads, such as a plan thread, an implementation thread and a review thread, and it can close more than one ticket.

Why reject a query or fragment instead of stripping it: a query can carry a token. An explicit value with one is a mistake worth reporting. A collected value with one is dropped.

### Where links come from

goodflows collects links once per root invocation, only when the history file or the upload is enabled, at the same point it captures `source`. Sources are merged in this order, and the first spelling of a duplicate wins:

1. **Flags**: repeatable `--ticket ID` and `--thread-url URL`. An invalid value, or more than 32 of either, fails as an argument error with exit 2 before any job runs.
2. **Caller environment**: `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS`, split on commas and whitespace. They are read from the process environment only, never settings or job `env`, as `ci` is. An invalid entry is dropped with a warning on stderr that names the variable and the entry's position, never its value. A bad CI variable shouldn't fail the build.
3. **Agent session**: when `AMP_THREAD_ID` is `T-` followed by a lowercase UUID, `<AMP_URL>/threads/<AMP_THREAD_ID>`. `AMP_URL` defaults to `https://ampcode.com` and must be an `https` origin. Otherwise the session adds nothing.
4. **Git**: see below.

Values from steps 2 to 4 that pass the rules are added until a list holds 32. Anything past that is dropped silently. Flag values come first, so they are never dropped.

### Git scan

`source::capture` gains one more git call in the same `tokio::join!` and under the same timeout and environment scrubbing:

```
git log -z --max-count=200 --format=%B HEAD --not <base>
```

`<base>` is the first of these that resolves: `refs/remotes/origin/$GITHUB_BASE_REF` when `GITHUB_BASE_REF` is set, then `refs/remotes/origin/HEAD`. When neither resolves, or HEAD is already reachable from `<base>` (a run on `main`), goodflows scans `HEAD` alone. Messages are read newest first.

- **Threads**: every line that starts, after optional whitespace, with `Amp-Thread-ID:`. Matching any line instead of only git trailers costs nothing, and it also catches trailers copied into a squash commit body if the GitHub setting changes later. A value is either a thread URL, which must pass the rule, or a bare `T-<uuid>`, which is expanded with `AMP_URL` as above.
- **Tickets**: only in commit subjects (first lines), in `source.branch`, and in `GITHUB_HEAD_REF` from the caller's environment. Bodies are skipped because they often mention other tickets ("follow-up to GOO-200"), which would link unrelated runs.

What this gives on GitHub Actions:

| Run | Default checkout (`fetch-depth: 1`) | `fetch-depth: 0` |
|---|---|---|
| Pull request | tickets from `GITHUB_HEAD_REF` only; HEAD is GitHub's merge commit | plus tickets and threads from every PR commit |
| Push to `main` after a squash | the squash subject's ticket | the same |

A git failure, a timeout or a shallow history means fewer links, never an error.

### Ticket prefixes

`[settings] ticket_prefixes = ["GOO"]` is valid only in the top-level `[settings]`, like `sprite_prefix`, because ticket IDs belong to the project, not to a workflow or profile. Each prefix is 1 to 16 ASCII alphanumerics starting with a letter, and duplicates ignoring case are rejected at load.

A match is the prefix, ignoring ASCII case, then `-`, then one or more ASCII digits. It must not be preceded or followed by an ASCII alphanumeric. The result is stored as the configured prefix, `-`, and the digits as written. So `tbrewer/goo-291-fix` gives `GOO-291`, and `MEGOO-12`, `GOO-`, and `GOO-12b` give nothing.

Without `ticket_prefixes`, git supplies no tickets. A general pattern such as `[A-Z]+-\d+` matches `UTF-8` and `SHA-256`, both of which appear in this repo's README. Using fixed prefixes also avoids adding a regex dependency.

### One flag pair for recording and filtering

`--ticket` and `--thread-url` are global args, like `--history`, because clap doesn't allow a subcommand to define a long name that a global arg already uses. On a run, they record links. On `history list`, the same args filter, take at most one value each (more fails with exit 2), and are checked with the same rules. `validate`, `list`, `init`, `reports upload`, `history show` and the Sprite commands reject them, as they reject other run flags.

Alternative: separate names such as `--filter-ticket`. Rejected because it gives two names for the same value, and `--ticket GOO-291` reads correctly in both places.

### Child invocations and Sprite runs

`HistoryTarget` gains `links: Option<&'a Links>`. A child invocation gets the root's value, the same way it gets `source`, so a ticket search also finds child runs.

`sprite::control::Provenance` gains `links: Option<Links>`, skipped when `None`. The remote runner is always the controller's version (the header carries `runner_version`, and the fallback installs the same published version), so `deny_unknown_fields` is not a compatibility problem. The remote runner writes the links it receives and never scans git. Its snapshot has no history to scan.

### Server contract

The companion goodflows_web change is `openspec/changes/accept-run-links/` in that repo:

- `POST /workflow-runs` accepts `links` with the rules above, and rejects an empty `links` or unknown keys inside it with 422 and the path, such as `links.tickets[2]`. A thread URL that is not already normalized gets "must be normalized", and a repeated entry gets "must be unique" on the repeat's path.
- The server stores both lists as written and indexes them for exact lookup. Tickets match ignoring ASCII case. The `thread_url` filter normalizes its value before matching.
- Because the server stores tickets as written, a retry that differs only in a ticket's case is a 409, not an idempotent 200.
- `GET /workflow-runs` accepts `ticket` and `thread_url`. Both filter for every key type.
- `GET /workflow-runs/:id` returns `links` under `data.document.links`, as stored. For a `pk_` key, `links.threads` is null only when the run has threads, and `links.tickets` is unchanged. A tickets-only `links` gets no `threads` key. The list response needs no new fields.
- Stored runs and older clients without `links` behave as before. The new columns default to empty, so nothing is backfilled.

A `pk_` key can still filter by `thread_url`. That only confirms the existence of a run for a URL the caller already has, and thread IDs are random UUIDs.

### `history show`

After the `ci` lines, `show` prints a `tickets` row with the tickets joined by `, `, and one `thread` row per URL. When `links.threads` is present and null, it prints one `thread` row with `-`. A missing `threads` key prints no `thread` row. When the run has no `links`, it prints neither row. All values pass through `visible`, as every other server string does. `--json` output is unchanged: the server's `data` as returned.

## Risks / Trade-offs

- [An older server rejects every record that has links, with 422] → Ship goodflows_web first. Records already in a spool are replayed by `reports upload` once the server accepts them. A CI spool that is thrown away loses them.
- [Wrong links: a long-lived branch accumulates unrelated threads, or a subject names the wrong ticket] → Commits are limited to the branch's own commits since `<base>`, at most 200, and lists to 32 entries. Flags let a caller state links explicitly when it matters.
- [Thread URLs in committed history files] → The same URLs already appear in those commits' trailers. A run started in an agent session that never committed adds its own URL. Projects that commit history files should know that.
- [An extra git call on every history-enabled run] → It runs in parallel with the existing calls, under the same 2-second budget.
- [`main` runs lack threads] → Accepted for now (option 1). The ticket search connects them to the PR runs that have threads.

## Migration Plan

1. Land the goodflows_web change and deploy it.
2. Land this change. No config change is required. Projects add `ticket_prefixes` to get tickets from git, and set `fetch-depth: 0` in CI to get threads from PR commits.
3. Existing history files and spools are unchanged. Records written before the upgrade simply have no `links`.
