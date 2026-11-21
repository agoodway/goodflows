# Proposal

## Why

When a production bug is filed against a ticket, there is no way to get from that ticket to the goodflows runs that tested the change or to the agent threads that wrote it. A run record holds the commit and, when uploaded, the branch and CI run. It never holds the ticket or the thread.

The information already exists, scattered:

- Amp sets `AMP_CURRENT_THREAD_ID` (and `AMP_THREAD_ID`) and `AMP_URL` in agent sessions, so a run started by an agent knows its own thread. goodflows already detects Amp through `AMP_CURRENT_THREAD_ID`.
- Commits that Amp makes end with an `Amp-Thread-ID: https://ampcode.com/threads/T-…` trailer. This repo has 29 such commits.
- Every recent commit subject on `main` starts with a ticket ID (`GOO-291: …`), including squash merges.

GitHub's default squash message copies each commit's message into the squash commit's body, so the `Amp-Thread-ID` lines survive on `main`, though no longer as git trailers. The squash commits for PRs #41, #42, #44 and #45 all carry them; #43 has none. A scan that reads every line of a message, not only the trailer block, therefore finds threads on `main` runs as well as on PR runs.

## What Changes

- The history file and the uploaded run record get an optional `links` object with `threads` (thread URLs) and `tickets` (ticket IDs). Both the file and the upload carry it. A called child workflow records its root invocation's links.
- goodflows collects links once per run, before the root invocation starts, from four sources, merged and deduplicated in this order:
  1. Repeatable `--ticket ID` and `--thread-url URL` run flags.
  2. `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS` in the caller's environment.
  3. The agent session: `AMP_CURRENT_THREAD_ID` (or `AMP_THREAD_ID`) with `AMP_URL`.
  4. Git: `Amp-Thread-ID:` lines in the messages of the branch's own commits, or of `HEAD` alone on `main`, and ticket IDs in those commit subjects and in the branch name.
- Tickets are found in git only for prefixes listed in a new top-level `[settings] ticket_prefixes`, such as `["GOO"]`. Without it, tickets come only from flags and the environment. A built-in pattern would match `UTF-8` and `SHA-256`.
- `goodflows history list` gains `--ticket` and `--thread-url` filters. `goodflows history show` prints the links.
- For a `pk_` key the server returns `links.threads` as null, so `history show` prints `-`. `links.tickets` stays visible, and a `pk_` key can still filter by either.
- Sprite runs collect links on the controller and pass them to the remote runner with the existing provenance.

No change to GitHub's squash settings is needed. A repo that edits squash messages down to the title loses the thread lines on `main`; its `main` runs then record tickets only, and a ticket search still reaches the PR runs that have threads.

## Capabilities

### New Capabilities

- `run-history`: records `links` in the history file and the uploaded record, says where links come from, and adds link filters to `history list` and the link rows to `history show`. The existing run-history requirements stay as archived in GOO-245. GOO-246 removed `openspec/specs/`, so this delta only adds requirements.

## Impact

- Code: `src/history.rs` (`Record`, `HistoryTarget`), a new `src/links.rs` for the rules, the git scan and collection, `src/config.rs` (`ticket_prefixes`), `src/main.rs` (flags, `Run.links`), `src/reporting/run_contract.rs` (validation), `src/reporting/history_client.rs` (filters), `src/output/history.rs` (`show`), `src/sprite/control.rs` and `src/sprite/mod.rs` (`Provenance`). `reporting::source::capture` and `Source` are unchanged.
- Tests: the test harnesses remove the Amp, link and GitHub base and head variables, so running the suite inside an Amp thread or on PR CI doesn't add `links` to records that tests compare exactly.
- Server: goodflows_web must accept `links` before this ships. Its validation rejects unknown keys, so an older server answers every record that has links with HTTP 422. The record stays pending, and a CI spool that is thrown away loses it. A companion goodflows_web change adds the field, a ticket and thread index, the `ticket` and `thread_url` list filters, and `pk_` redaction of `links.threads`.
- Files: the history file's `schema_version` stays 1, and `links` is optional, so older readers keep working. History files committed to a repo will contain thread URLs. The same URLs are already in that repo's commit trailers.
- Users: nothing to change. Projects that want tickets from git add `ticket_prefixes`. CI that wants threads from PR commits checks out with `fetch-depth: 0`.
- Sprite: a development build that falls back to the published runner of the same version can't send `links` to it, as docs/sprites.md already warns for any unreleased change. Released versions match.
