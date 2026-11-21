# Tasks

## 0. Server first

- [ ] 0.1 Land and deploy the goodflows_web change described under "Server contract" in design.md: accept and validate `links` with the same path-character, normalized-form and ticket rules, index tickets and thread URLs, add the `ticket` and `thread_url` list filters, and return `links.threads` as null for a `pk_` key. Do not release this change before that server change is live, because an older server answers every record with links with HTTP 422.

## 1. Test isolation

- [ ] 1.1 Remove `AMP_CURRENT_THREAD_ID`, `AMP_THREAD_ID`, `AMP_URL`, `GOODFLOWS_TICKETS`, `GOODFLOWS_THREAD_URLS`, `GITHUB_HEAD_REF` and `GITHUB_BASE_REF` from the environment in `tests/common::Project::command`, the `history_command` helper in `tests/reporting.rs`, and the Sprite test harness. Do this first, so the suite stays green inside an Amp thread and on PR CI while the feature lands. Verify with `cargo test --locked` run from an Amp thread.

## 2. Link values and rules

- [ ] 2.1 Add `src/links.rs` with `Links { threads: Vec<String>, tickets: Vec<String> }` (serialized with empty lists skipped), `valid_ticket` (1 to 64 bytes, `!` to `~`, no `,`), `thread_url` (parse and normalize with `reqwest::Url`; reject non-`https`, no host, userinfo, `query().is_some()`, `fragment().is_some()`, a host that is neither bracketed IPv6 nor only RFC 3986 unreserved and sub-delims characters, a path byte that is not an RFC 3986 `pchar` or `/`, a `%` not followed by two hex digits, or more than `MAX_URL_BYTES`), and a merge that deduplicates (tickets ignoring ASCII case, first spelling wins) and stops at 32 per list. Unit-test the boundaries: 64- and 65-byte tickets; a tab, a space, a comma, `é`; `http://`, `https://u@h/`, `?a=1`, a bare `?`, `#m`, a bare `#`; `|`, `^`, `[`, `]`, `%zz` and a trailing `%` in a path; `"`, `` ` ``, `{` and `}` in a host, and `https://[::1]/x` accepted; a 2049-byte URL; `https://h:443/a/../b` normalizing to `https://h/b`; `https://h` normalizing to `https://h/`; `%7E` and `%2f` kept as written; `HTTPS://AmpCode.com/threads/T-x` deduplicating with its lowercase form; a 33rd value dropped. Verify with `cargo test --lib links`.
- [ ] 2.2 Add ticket matching for configured prefixes. Test the spec's boundary subject (`MEGOO-12 GOO-7, goo-0042b and goo-15` gives `GOO-7`, `GOO-15`), a branch `tbrewer/goo-291-fix`, `GOO-` at the end of a string, and two prefixes in one subject. Verify with `cargo test --lib links`.
- [ ] 2.3 Add `Amp-Thread-ID:` line parsing: a full URL, a bare `T-<uuid>` expanded with a given origin, leading whitespace, a line in the middle of a body, an uppercase UUID (dropped), and a URL with a query (dropped). Verify with `cargo test --lib links`.

## 3. Config and flags

- [ ] 3.1 Add `ticket_prefixes` to the top-level `Settings` only, rejected in workflows and profiles as `sprite_prefix` is, and left out of `EffectiveSettings` as `sprite_prefix` is. Validate the prefix rule and case-insensitive duplicates. Test acceptance, each rejection, and a workflow and a profile setting it. Verify with `cargo test --lib config`.
- [ ] 3.2 Add global, repeatable `--ticket` (id `ticket`) and `--thread-url` (id `thread_url`) next to `--history` in `src/main.rs`, with value parsers from `links`, and reject more than 32 of either with `usage()`. Add both to `run_only_flag()` and extend `history_commands_name_a_rejected_run_only_flag`. Test an invalid value and 33 values of each on a run (exit 2, no job ran), `history show --ticket X` (exit 2), and that `validate` and `list` still accept run flags. Verify with `cargo test --locked --bin goodflows` and `cargo test --test runner`.

## 4. Collection

- [ ] 4.1 Add `links::scan(workdir, token_env, lookup)`: its own 2-second `tokio::time::timeout`, `GIT_OPTIONAL_LOCKS=0`, the token variable removed, shallow check, base resolution, `git log -z --max-count=200 --format=%B HEAD --not <base>`, and the `HEAD`-alone fallback, as in design.md. It returns commit messages and never touches `reporting::source` or `Source`. Unit-test against temporary git repos: a feature branch, a range that is empty, no `origin/HEAD`, `GITHUB_BASE_REF` choosing the base, a `git clone --depth 2` of a repo with older base history (only `HEAD` read), and not a git checkout. Verify with `cargo test --lib links`.
- [ ] 4.2 Add `links::collect(flags, prefixes, branch, workdir, token_env, lookup, warn)` that merges flags, `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS`, the agent session (`AMP_CURRENT_THREAD_ID`, then `AMP_THREAD_ID`, with `AMP_URL` checked and joined as `ci::run_url` does), git threads, then tickets from subjects, the branch and `GITHUB_HEAD_REF`. Unit-test: environment entries with empty pieces and one invalid entry warned by position without its value; `AMP_URL` with a trailing slash (accepted), a path, `http`, a query (each adding nothing); `AMP_CURRENT_THREAD_ID` winning over `AMP_THREAD_ID`; and the 32 cap across sources with flags kept. Verify with `cargo test --lib links`.
- [ ] 4.3 Add `links: tokio::sync::OnceCell<Option<Links>>` to `Run`. In `invocation()`, call `get_or_init` only when the invocation writes a history file or its session will send a run record (not for a session skipped for missing credentials), and pass the result into `HistoryTarget`. Verify with `cargo build`.

## 5. Record

- [ ] 5.1 Add `links: Option<&'a Links>` to `HistoryTarget` and a `links` field to `Record` between `ci` and `results`, skipped when empty. Extend the `record` unit tests in `src/history.rs`: the file and the upload hold the same `links`, and `links` is absent when empty. Update the `Record` doc comment. Verify with `cargo test --lib history`.
- [ ] 5.2 Add `links` to `run_contract::DOCUMENT` and a `LINKS` field set with the rules from design.md, reporting paths such as `links.tickets[2]`. Thread URLs must satisfy `Url::parse(s).as_str() == s` and pass the host- and path-character rules. Add `tests/fixtures/workflow-runs/links.json` (valid, both lists, including a `%7E` path) for goodflows_web to vendor. Add cases for an empty `links`, an unknown key in it, 33 tickets, a duplicate ticket differing only in case, a comma in a ticket, and thread URLs with a query, a bare `?`, an uppercase host, an explicit `:443`, `:0443`, `/a/%2E%2e/b`, `ex%61mple.com`, `127.1`, `[0:0:0:0:0:0:0:1]`, `|`, `[`, `%zz`, a trailing `%`, and `https://a{b.com/x`. Put the `%2E%2e` and `a{b.com` cases in the shared fixture set too. Verify with `cargo test --lib run_contract`.

## 6. Sprite

- [ ] 6.1 Add `links: Option<Links>` to `sprite::control::Provenance`, skipped when `None`. On the controller, collect links with `links::collect` when the run it sends can record (history on for the root or any callable workflow under the selected profile, or `[reporting] history = true`), and set them next to the snapshot provenance. On the remote, initialize `Run.links` from `Provenance.links` so it never scans git. Extend the provenance round-trip test, and a `tests/sprite.rs` history test so the remote history file carries the controller's links and a run with nothing recording sends none. Verify with `cargo test --test sprite`.

## 7. History read commands

- [ ] 7.1 In `history_client.rs`, add `#[arg(long, id = "ticket")] ticket: Option<String>` and `#[arg(long = "thread-url", id = "thread_url")] thread_url: Option<String>` to the `history list` filters, with the link value parsers, and send them as `ticket` and `thread_url`. Extend the query-building test. Add CLI tests: `history list --ticket GOO-291` sends `ticket=GOO-291`; a repeated `--ticket` and a bad value exit 2 without a request; `goodflows --ticket X history list` either filters by `X` or exits 2, and never drops the value. Verify with `cargo test --locked --bin goodflows` and `cargo test --test reporting`.
- [ ] 7.2 In `output/history.rs`, read `document.links` and print the `tickets` row and the `thread` rows after `ci`. Print a single `thread -` row only when `links.threads` is present and null, no `thread` row when the key is missing (tickets-only `pk_` response), and no rows without `links`. Add text-output tests for each case, one with an escape sequence in a ticket. Verify with `cargo test --test output`.

## 8. End-to-end tests

- [ ] 8.1 In `tests/history.rs`, build a temporary git repo: a base commit, `git update-ref refs/remotes/origin/HEAD <base>`, a branch `goo-291-fix` with two commits whose subjects carry no ticket and whose `Amp-Thread-ID` trailers differ, one body mentioning `GOO-200`, and `ticket_prefixes = ["GOO"]`. Run with history, `AMP_CURRENT_THREAD_ID` set, and `--ticket GOO-300`. Assert `links.tickets` is `["GOO-300", "GOO-291"]` and `links.threads` is the session URL followed by both trailer URLs, newest first.
- [ ] 8.2 In the same repo, make a squash commit on the base whose subject is `GOO-291: …` and whose body copies both branch messages, point `origin/HEAD` at it, unset the session variables, and assert `links.tickets` `["GOO-291"]` and both thread URLs. Then amend it to the subject alone and assert only `links.tickets`.
- [ ] 8.3 Assert that a called child workflow's file has the root's `links`; that a root with history off and a child with history on still records links in the child's file; that a run outside a git checkout still records flag and session links; and that `[settings] env` setting `GOODFLOWS_TICKETS` records nothing.
- [ ] 8.4 Put a fake `git` first on `PATH` that records each call to a file, and assert it is never called for links when history and the upload are off (with a job declaring `reports`), and when `[reporting] history = true` has no key. Assert it is called for links once in a run with history on that calls two workflows.
- [ ] 8.5 In `tests/reporting.rs`, assert that with history and upload both on, the uploaded record's `links` equals the file's, and that the existing "same run fields" comparison still passes. Verify sections 8.1 to 8.5 with `cargo test --test history --test reporting --test workflows`.

## 9. Documentation

- [ ] 9.1 In README "Run history", add a "Linking runs to threads and tickets" subsection: the `links` shape, the four sources and their order, `ticket_prefixes`, `fetch-depth: 0` for PR threads, that squash commits keep thread lines in their body unless the message is trimmed to the title, and that committed history files will contain thread URLs. In "Uploading run history", add `links` to the fields the record holds and note `history list --ticket` and `--thread-url`, and the `pk_` redaction of threads.

## 10. Integration check

- [ ] 10.1 Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked`, from an Amp thread so the isolation in 1.1 is exercised. Run this repo's own `goodflows.toml` with `--history` from a branch and confirm the file holds the thread URL and the expected tickets once `ticket_prefixes = ["GOO"]` is set.
