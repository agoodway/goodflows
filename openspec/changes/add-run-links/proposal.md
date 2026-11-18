# Proposal

## Why

When a production bug is filed against a ticket, there is no way to get from that ticket to the goodflows runs that tested the change or to the agent threads that wrote it. A run record holds the commit and, when uploaded, the branch and CI run. It never holds the ticket or the thread.

The information already exists, scattered:

- Amp sets `AMP_URL` and `AMP_THREAD_ID` in every agent session, so a run started by an agent knows its own thread.
- Commits that Amp makes end with an `Amp-Thread-ID: https://ampcode.com/threads/T-…` trailer. This repo has 30 of them.
- Every recent commit subject on `main` starts with a ticket ID (`GOO-291: …`), including squash merges.

Squash merges drop the `Amp-Thread-ID` trailers (PRs #41 to #45 have none on `main`), so `main` runs can't recover threads from git. They still carry the ticket, and so do the PR runs, which do have the threads. A search by ticket therefore reaches both.

## What Changes

- The history file and the uploaded run record get an optional `links` object with `threads` (thread URLs) and `tickets` (ticket IDs). Both the file and the upload carry it. A called child workflow records its root invocation's links.
- goodflows collects links once per root invocation from four sources, merged and deduplicated in this order:
  1. Repeatable `--ticket ID` and `--thread-url URL` run flags.
  2. `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS` in the caller's environment.
  3. The agent session: `AMP_THREAD_ID` with `AMP_URL`.
  4. Git: `Amp-Thread-ID:` lines in commit messages on the current branch, and ticket IDs in those commit subjects and in the branch name.
- Tickets are found in git only for prefixes listed in a new top-level `[settings] ticket_prefixes`, such as `["GOO"]`. Without it, tickets come only from flags and the environment. A built-in pattern would match `UTF-8` and `SHA-256`.
- `goodflows history list` gains `--ticket` and `--thread-url` filters. `goodflows history show` prints the links.
- For a `pk_` key the server returns `links.threads` as null, so `history show` prints `-`. `links.tickets` stays visible, and a `pk_` key can still filter by either.
- Sprite runs collect links on the controller and pass them to the remote runner with the existing provenance.

No change to GitHub's squash settings. `main` runs record tickets only, unless an agent or a flag supplies threads.

## Capabilities

### New Capabilities

- `run-history`: records `links` in the history file and the uploaded record, says where links come from, and adds link filters to `history list` and the link rows to `history show`. The existing run-history requirements stay as archived in GOO-245. GOO-246 removed `openspec/specs/`, so this delta only adds requirements.

## Impact

- Code: `src/history.rs` (`Record`, `HistoryTarget`), a new `src/links.rs` for collection, `src/reporting/source.rs` (git scan), `src/config.rs` (`ticket_prefixes`), `src/main.rs` (flags), `src/reporting/run_contract.rs` (validation), `src/reporting/history_client.rs` (filters), `src/output/history.rs` (`show`), `src/sprite/control.rs` (`Provenance`).
- Server: goodflows_web must accept `links` before this ships. Its validation rejects unknown keys, so an older server answers every record that has links with HTTP 422. The record stays pending, and a CI spool that is thrown away loses it. A companion goodflows_web change adds the field, a ticket and thread index, the `ticket` and `thread_url` list filters, and `pk_` redaction of `links.threads`.
- Files: the history file's `schema_version` stays 1, and `links` is optional, so older readers keep working. History files committed to a repo will contain thread URLs. The same URLs are already in that repo's commit trailers.
- Users: nothing to change. Projects that want tickets from git add `ticket_prefixes`. CI that wants threads from PR commits checks out with `fetch-depth: 0`.
