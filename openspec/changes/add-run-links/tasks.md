# Tasks

## 0. Server first

- [ ] 0.1 Land and deploy the goodflows_web change described under "Server contract" in design.md: accept and validate `links`, index tickets and thread URLs, add the `ticket` and `thread_url` list filters, and return `links.threads` as null for a `pk_` key. Do not release this change before that server change is live, because an older server answers every record with links with HTTP 422.

## 1. Link values and rules

- [ ] 1.1 Add `src/links.rs` with `Links { threads: Vec<String>, tickets: Vec<String> }` (serialized with empty lists skipped), `valid_ticket`, `thread_url` (parse and normalize with `reqwest::Url`, rejecting non-`https`, no host, userinfo, query or fragment, any character outside RFC 3986's unreserved, reserved and `%` set, or more than `MAX_URL_BYTES`), and a merge that deduplicates (tickets ignoring ASCII case, first spelling wins) and stops at 32 per list. Unit-test the boundaries: 64- and 65-byte tickets; a tab, a space, `é`; `http://`, `https://u@h/`, `?a=1`, `#m`, `|` and `^` in a path, a 2049-byte URL; `https://h:443/a/../b` normalizing to `https://h/b`; `HTTPS://AmpCode.com/threads/T-x` deduplicating with its lowercase form; a 33rd value dropped. Verify with `cargo test --lib links`.
- [ ] 1.2 Add ticket matching for configured prefixes. Test the spec's boundary subject (`MEGOO-12 GOO-7, goo-0042b and goo-15` gives `GOO-7`, `GOO-15`), a branch `tbrewer/goo-291-fix`, `GOO-` at the end of a string, and two prefixes in one subject. Verify with `cargo test --lib links`.
- [ ] 1.3 Add `Amp-Thread-ID:` line parsing: a full URL, a bare `T-<uuid>` expanded with a given origin, leading whitespace, an uppercase UUID (dropped), and a URL with a query (dropped). Verify with `cargo test --lib links`.

## 2. Config and flags

- [ ] 2.1 Add `ticket_prefixes` to the top-level `Settings` only, rejected in workflows and profiles as `sprite_prefix` is. Validate the prefix rule and case-insensitive duplicates. Show it in effective-settings inspection where `sprite_prefix` appears. Test acceptance, each rejection, and a profile setting it. Verify with `cargo test --lib config` and `cargo test --test inspection`.
- [ ] 2.2 Add global `--ticket` and `--thread-url` (repeatable) next to `--history` in `src/main.rs`. Validate them at parse time with exit 2. Add them to the run-flag rejection list for `validate`, `list`, `init`, `reports upload`, `history show` and the Sprite commands, but not `history list`. Extend the existing rejection tests. Verify with `cargo test --test runner --test reporting`.

## 3. Collection

- [ ] 3.1 Add the git scan to `reporting::source::capture`, in the same `join!`, timeout and token scrubbing: resolve the base (`origin/$GITHUB_BASE_REF`, then `origin/HEAD`), run `git log -z --max-count=200 --format=%B HEAD --not <base>`, and fall back to `HEAD` alone. Return the raw messages with `Source`, or a sibling value if `Source`'s `deny_unknown_fields` serde shape must not change. Verify with `cargo build`.
- [ ] 3.2 Collect links once per root invocation where `source` is captured, only when the history file or the upload is enabled: flags, then `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS` from `std::env`, then `AMP_THREAD_ID` and `AMP_URL`, then git threads, then tickets from subjects, `source.branch` and `GITHUB_HEAD_REF`. Pass the environment through a lookup closure as `ci::capture` does, so tests don't set process variables. Warn on stderr for an invalid environment entry by variable and position only.

## 4. Record

- [ ] 4.1 Add `links: Option<&'a Links>` to `HistoryTarget` and a `links` field to `Record` between `ci` and `results`, skipped when empty. Give child invocations the root's value. Extend the `record` unit tests in `src/history.rs`: the file and the upload hold the same `links`, and `links` is absent when empty. Verify with `cargo test --lib history`.
- [ ] 4.2 Add `links` to `run_contract::DOCUMENT` and a `LINKS` field set with the rules from design.md, reporting paths such as `links.tickets[2]`. Thread URLs must equal their normalized form and use only RFC 3986 characters. Add `tests/fixtures/workflow-runs/links.json` (valid, both lists) for goodflows_web to vendor. Add cases for an empty `links`, an unknown key in it, 33 tickets, a duplicate ticket differing only in case, a thread URL with a query, an uppercase host, an explicit `:443`, and `|` in the path. Verify with `cargo test --lib run_contract`.

## 5. Sprite

- [ ] 5.1 Add `links: Option<Links>` to `sprite::control::Provenance`, skipped when `None`, filled from the controller's collection. Have the remote runner pass it to `HistoryTarget` and never collect links itself. Extend the provenance round-trip test and a `tests/sprite.rs` history test so the remote history file carries the controller's links. Verify with `cargo test --test sprite`.

## 6. History read commands

- [ ] 6.1 In `history_client.rs`, send `--ticket` as `ticket` and `--thread-url` as `thread_url` on `history list`, and fail with exit 2 when either is given more than once. Extend the query-building test. Verify with `cargo test --lib history_client`.
- [ ] 6.2 In `output/history.rs`, read `document.links` and print the `tickets` row and the `thread` rows after `ci`. Print a single `thread -` row only when `links.threads` is present and null, no `thread` row when the key is missing (tickets-only `pk_` response), and no rows without `links`. Add text-output tests for each case, one with an escape sequence in a ticket. Verify with `cargo test --test output`.

## 7. End-to-end tests

- [ ] 7.1 In `tests/history.rs`, build a temporary git repo: a base commit, `git update-ref refs/remotes/origin/HEAD <base>`, a branch `goo-291-fix` with two commits carrying different `Amp-Thread-ID` trailers and one body mentioning `GOO-200`, and `ticket_prefixes = ["GOO"]`. Run with history, `AMP_THREAD_ID` set, and `--ticket GOO-300`. Assert `links.tickets` is `["GOO-300", "GOO-291"]` and `links.threads` is the session URL followed by both trailer URLs, newest first. Then check out the base with a `GOO-291: …` subject and no trailers, with the session variables unset, and assert only `links.tickets` `["GOO-291"]`.
- [ ] 7.2 In the same file, assert that a called child workflow's file has the root's `links`, that a run outside a git checkout still records flag and session links, and that `[settings] env` setting `GOODFLOWS_TICKETS` records nothing.
- [ ] 7.3 In `tests/reporting.rs`, assert that with history and upload both on, the uploaded record's `links` equals the file's, and that the existing "same run fields" comparison still passes. Verify sections 7.1 to 7.3 with `cargo test --test history --test reporting --test workflows`.

## 8. Documentation

- [ ] 8.1 In README "Run history", add a "Linking runs to threads and tickets" subsection: the `links` shape, the four sources and their order, `ticket_prefixes`, `fetch-depth: 0` for PR threads, the squash-merge limit on `main`, and that committed history files will contain thread URLs. In "Uploading run history", add `links` to the fields the record holds and note `history list --ticket` and `--thread-url`, and the `pk_` redaction of threads. Update the `Record` doc comment in `src/history.rs`.

## 9. Integration check

- [ ] 9.1 Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked`. Run this repo's own `goodflows.toml` with `--history` from this branch and confirm the file holds this thread's URL and the expected tickets once `ticket_prefixes = ["GOO"]` is set.
