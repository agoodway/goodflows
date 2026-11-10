# Tasks

Prerequisite: the goodci change `validate-job-names` (another thread, branch `t3/validate-job-names`, not yet on `main`) applies the name rule to every job name. This change must not duplicate it. Implementation may start first, because task 2.2 reports bad job names as `invalid_record`. A release that enables uploads must include both changes (design D10).

## 1. Setting and record builder

- [x] 1.1 Add `history: bool` (serde default `false`) to `config::Reporting` (design D1). A non-boolean value must fail validation naming `reporting.history`. Verify with `config.rs` unit tests and `tests/reporting.rs` cases for `history = "yes"` (exit 1 before jobs) and for `history = true` with no `reports` jobs (valid, no `GOODFLOWS_RUN_ID` in commands). The existing `inspection_never_creates_spools_or_contacts_the_endpoint` test must still pass with `history = true` added.
- [x] 1.2 Refactor `history::graph_sha256` to return the hash together with an owned `GraphDocument`, and add a function that makes the truncated upload copy (job `command`, step `command`, condition `text` cut with `truncate`), as in design D2. Verify that `graph_hash_is_pinned`, `a_call_appends_its_target_hash_after_the_existing_fields` and the other existing graph tests pass unchanged. Add tests that a 2,000-byte command and a multibyte condition are cut to at most 1,024 bytes on a character boundary while the hash matches the uncut document, and that a call job keeps `call` and does not inline its target.
- [x] 1.3 Extract the `Record` construction from `history::write` into a builder that takes optional upload extras (`branch`, `ci`, `graph`), with `branch` after `dirty`, `ci` after `source` and `graph` last, all skipped when absent (design D2). Verify with a test that the local file bytes for a fixed run are identical before and after the refactor, and a test that an upload record minus `graph`, `ci` and `source.branch` equals the local record as a JSON value.

## 2. CI context and local validation

- [x] 2.1 Add `reporting::ci` (design D3), which reads the caller environment through an injectable lookup. Verify with a table test covering: the push example from the spec; `refs/pull/42/merge` → `pull_request` 42; `refs/pull/0/merge`, `refs/pull/x/merge`, `refs/heads/main` → no `pull_request`; `GITHUB_RUN_ATTEMPT` values `0`, `+1`, `1.0` and `9007199254740992` dropped; a 65-byte or whitespace-only `RUNNER_NAME` or `GITHUB_EVENT_NAME` dropped; an `http://`, path-carrying or userinfo `GITHUB_SERVER_URL` and a malformed `GITHUB_REPOSITORY` → no `url`; `GITHUB_ACTIONS` unset or `1` → no `ci`.
- [x] 2.2 Add `reporting::run_contract` (design D4). It validates a `serde_json::Value` and the encoded length against every rule in goodflows_web `add-workflow-run-storage` `specs/workflow-run-storage/spec.md`, from "The body is a run-history document" through "Outcome agrees with result statuses" and "Malformed or oversized uploads store nothing". It also covers the call fields in design "Server contract changes needed" items 1, 3 and 4. It returns the first `(path, code)`. Verify with unit tests for each server scenario the CLI can produce, including:
  - Required, unknown and null-optional keys, `schema_version` 2, uppercase `run_id`, fractional `started_at`.
  - The name rule at 65 bytes, whitespace-only and with a control character.
  - Empty and 1,025-entry `requested_jobs` and `commands`; 1,025 graph jobs, `depends_on` entries and steps.
  - A 1,025-byte `command`, `error` and graph text; `exit_code` 2^31; `peak_rss_bytes` 0; partition count 65, index 0 and index > count.
  - The four outcome cases, and `source.branch` at 257 bytes.
  - `ci` without `provider`, with `attempt` 0, an `http` URL, or an unknown key.
  - A graph with an unknown job key, `strategy` `hash`, a duplicate name, an unknown dependency, a two-job cycle, or a result outside the graph.
  - `call` with a bad hash; `parent_run_id` without `caller_job`; `caller_job` without `parent_run_id`; either of those keys set to null; `child_run_id` without `workflow`; `child_run_id` or `workflow` set to null. `workflow` without `child_run_id` is valid.
  - The captured documents in `fixtures/parent-default.json` and `fixtures/child-suite.json` pass once the call fields are accepted, including JSON `null` for `error` on every result and for `command` and `exit_code` on the call result.
  - U+0000 in a string, depth 9, a 4,194,305-byte body, and integers above 2^53-1.
  
  Unknown-key errors must name the enclosing path only.
- [x] 2.3 Prove the builder and the validator agree. Add a test that builds upload records for real configs and checks that each one validates: a minimal workflow; partitions with step and condition costs; workflow calls with the root and child records; `--env` with `--jobs`; and a signal-cancelled run.
- [x] 2.4 Add shared fixtures under `tests/fixtures/workflow-runs/`: `minimal.json`, `full.json` with `environment`, `requested_jobs`, `source.branch`, `ci`, and a `graph` with partitions, steps, conditions and a `call`, plus `child.json`. Verify that each passes `run_contract`, and note in the PR description that goodflows_web task 2.1 should reuse them.

## 3. Publisher

- [x] 3.1 Generalize `reporting::publish` (design D7): `Target` keeps the origin and project, and a `DocumentKind` selects `/test-reports` or `/workflow-runs`, the acknowledgement parser and the status mapping. The User-Agent becomes `goodflows/<v> test-reports/1 workflow-runs/1`. Verify that every existing `publish.rs` test passes unchanged for test reports. Add mock tests for `WorkflowRun`:
  - 201 and 200 `duplicate` acknowledgements, and a mismatched `run_id`, a non-UUID id and a missing `duplicate` (pending).
  - 408 then 201 (uploaded); 429 or 503 with `Retry-After`; 500 three times (pending `server_error`).
  - 409 (rejected `conflict`); 400 and 415 (rejected); 413 and 422 (pending, not retried); 401, 403, 404 and 302 (pending); a TLS failure (pending, one attempt).
- [x] 3.2 Make `Session` own one lazily created `Publisher` shared by test-report delivery and the run record (design D6), so the 30-second budget covers both. Verify with a unit test (fast limits) where slow report acks exhaust the budget and the run record is left pending with `budget_exhausted` without a request. The existing `shared_budget_is_invocation_wide` test must still pass.

## 4. Spool, manifest and invocation flow

- [x] 4.1 Add `run_record: Option<{schema_version}>` to `Manifest`, serialized only when present. Extend `manifest_version` (version 3 when a run record is listed, otherwise 1 or 2 as today) and replay's version check (design D5). Verify that the existing manifest tests pass and that new tests cover: version 1 and 2 manifests deserializing and serializing unchanged; a version 3 manifest with and without partitions; a version 3 manifest without `run_record`, and a version 2 manifest with it, both refused.
- [x] 4.2 In `Session::start`, when `history = true`: resolve credentials once. If they are not ready and the invocation is not remote (`Run::sprite_state` is `None`), record `run_upload_skipped` (`credentials_unavailable` or `invalid_url`) for emission at upload time. A remote invocation continues without credentials (design D11, task 5.2). If they are ready, capture `Source` and create the spool run directory even when no job declares `reports` (design D5). Make `main.rs` reuse that `Source` for local history. Verify with `tests/reporting.rs` cases: no key → the event, no spool directory, exit code unchanged; a valid key and no report jobs → `<run_id>/manifest.json` exists before jobs run and no command sees `GOODFLOWS_REPORT_DIR`.
- [x] 4.3 Implement freezing (design D5): build the upload record, validate it, write `run-record.json` create-new with mode 0600 relative to the run directory while refusing symlinks, then atomically rewrite the finalized manifest with `run_record` and version 3. Emit `run_upload_skipped` with `invalid_record` and `path`, or with `spool_unavailable`, on failure. Verify with tests for a symlinked run directory (refused), a job named `run-record.json` (no collision), an invalid record (no file, manifest unchanged, exit code unchanged), and file modes.
- [x] 4.4 Reorder `main.rs::invocation` (design D6) to: execute → `session.finish` → outcome snapshot → freeze and send the run record → local history → `summary`, with `summary.duration_ms` measured after the upload. Write `run-record.ack.json` or `run-record.rejected.json` from the outcome. Verify with `tests/reporting.rs` mock-server tests:
  - Upload with local history off; upload with `--no-history`; both on, with the history file lacking `branch`, `graph` and `ci` while the request has them.
  - A workflow call where the child and root records are both posted, the child first, linked by `parent_run_id`, `caller_job` and `child_run_id`.
  - `GITHUB_*` variables reaching `ci`, while a settings-env `GITHUB_ACTIONS` does not.
  - A signal during the upload: pending `cancelled`, the summary is the last event, the outcome is unchanged and the exit code is the signal's.
  - `duration_ms` ≥ `execute_ms` when the mock delays its ack.
  - Under `Mock::store()`, a second identical post returns 200 `duplicate`.
- [x] 4.5 Add text and JSON rendering for `run_uploaded`, `run_upload_pending` and `run_upload_skipped` in `output.rs`, matching the existing report lines. Verify with `tests/output.rs` cases for each event and reason, including `terminal`, and a `--json` test that every line stays a valid versioned event.

## 5. Replay

- [x] 5.1 Extend `reporting::replay::upload_with` (design D8): after the reports, load a listed run record (bounded read, no symlinks), re-validate it, match `run_id`, `workflow` and `environment` against the manifest, skip it when acknowledged or rejected, and otherwise send it in the same budget and record the outcome. Add `run_record` to `reports_upload_summary`, and count a pending, rejected or invalid record toward a nonzero exit. Verify with `tests/reporting.rs` cases:
  - Replay after an offline run (sent unchanged, acked, exit 0); replay again (no request, exit 0).
  - Replay after a 409 (no request, `rejected_by_server`, exit 1); after a 422 (resent).
  - A tampered `run-record.json` or a mismatched `run_id` (invalid, exit 1); a missing or symlinked record file (invalid, exit 1).
  - A version 1 or 2 manifest giving `run_record` null.
- [x] 5.2 Sprite runs (design D11):
  - A remote invocation with `history = true` freezes `run-record.json` without a key, sends nothing, and emits `run_upload_pending` with `credentials_unavailable`.
  - `sprite::trusted_spool` refuses a manifest listing `run_record` unless the trusted `Reporting.history` is `true`.
  - The controller's existing post-collection loop needs no other change: `replay::upload` sends the record.
  
  Verify with a `trusted_spool` unit test next to `automatic_publication_requires_a_spool_bound_to_this_invocation`, and with `tests/sprite.rs` cases using the fake Sprite and a mock server:
  - An attached run with a workflow call posts the child's and root's records after collection, both with the same snapshot `source`, and the exit code is unchanged by a 503.
  - A spool listing a run record under a trusted config without `history` is refused and nothing is posted.
  - `pass_env` forwarding of the `GITHUB_*` variables yields `ci`, and without it there is no `ci`.
  - A detached run's collected spool replays the record through `reports upload`.
- [x] 5.3 Capture `commit`, `branch`, and archive-derived `dirty` inside `snapshot::take` (design D11). Send them on `Header.provenance`. `exec-workflow` reads that header once and writes the same `source` into the root and every child history file and test-report record. Do not put the values in `pass_env` or argv. Omit `source` when HEAD cannot be read. Verify a clean branch (`dirty` false, `branch` present), a tracked edit and a deleted tracked file (`dirty` true), a nonignored untracked file with an ignored file present (`dirty` true, ignored file absent from the archive), a detached HEAD (`branch` absent), and a job environment that does not contain the commit.

## 6. History read commands

- [x] 6.1 Add the `history` subcommand with `list` and `show <run_id>` to `main.rs` (design D9). It rejects every flag in `RUN_ONLY_FLAGS` with the same error form as `reports upload` and the `sprite` commands, extends `Cli::parser` to hide those flags from `history` help, adds `history` to `Cli::workflow()` and the `cancellation()` command set, and uses clap value parsers for the filter rules, `limit` 1-200, the `cursor` grammar and a lowercase canonical `run_id`. Verify with `main.rs` parse tests: exit 2 for `--outcome green`, an uppercase `run_id`, `--limit 0` and `--commit HEAD`; rejected flags named; no request sent (mock request count 0).
- [x] 6.2 Implement the GET client in `reporting/` (or a new `history_client` module) that reuses `Target` origin validation and credential resolution. Responses are bounded to 16 MiB with a structural shape check, have no retries, and report errors by HTTP status only. Verify with mock tests: filters encoded as query parameters; a 404 on show ("run not found", exit 1); a 503 with a body that never appears in output; an oversized or malformed body (exit 1); a missing `[reporting]` block, key or invalid URL (exit 1, no request).
- [x] 6.3 Implement text and `--json` output (design D9). Verify with `tests/output.rs` and integration tests:
  - The list table, with a dirty commit marker and a next-cursor line, and its absence on the last page.
  - Show, with results, partitions, call results and linked test reports.
  - A `pk_` show with null `command` and graph text printed as absent.
  - An escape sequence in a server-provided workflow name printed escaped.
  - `--json` producing exactly one `history_runs` or `history_run` event.
  
  Use `tests/fixtures/workflow-runs/full.json` inside a mocked show response.

## 7. Documentation

- [ ] 7.1 Update the README:
  - "Run history": `[reporting] history = true`, what is uploaded (including graph command text for unrun jobs), that `--no-history` controls only the file, that an unset key skips uploads, and `history list`/`show` with examples.
  - "Test timing reports": the `run-record.json` spool file, manifest version 3 and replay.
  - The `--no-history` help text: the file only.
  - `docs/sprites.md`: remote run records are published by the controller after collection; their `source` is the snapshot provenance (`commit`, archive-derived `dirty`, and `branch` only when git reports a branch); listing the `GITHUB_*` variables in `pass_env` adds `ci` and also exposes those variables to hooks and jobs, and does not supply `source`.
  
  Verify that the documented `goodflows history list --workflow ci --branch main` and `reports upload` examples run against the mock server in an integration test.

## 8. Integration

- [ ] 8.1 Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked` and verify they pass.
- [ ] 8.2 Smoke test against a local goodflows_web running `add-workflow-run-storage` with the server contract changes. Run an upload-enabled workflow with a call and `GITHUB_*` variables set, check that `history list --graph-sha256 <hash>` and `history show` return both records, kill the server mid-run, and replay with `reports upload` (200 `duplicate` or 201). If a Sprite is available, repeat with `run --sprite` and check that the controller publishes both records after collection. Record the commands and the minimum goodflows_web version in the PR description.

## Workflow follow-up

- Send the "Server contract changes needed" list in design.md to the goodflows_web `add-workflow-run-storage` thread before that change is archived.
- Archive this change after `validate-job-names` has landed and the server accepts the call fields.

## Size

8 task groups, about 27–45 agent-hours. The risky parts are the local validator (null `command`, `exit_code`, and `error`, the four call fields, and the pairing rules), manifest version 3 next to version 1 and 2 spools, Sprite publication that freezes a record without a key, and setting `dirty` from the shipped archive rather than from `git status --untracked-files=no`. The smoke test stays red until the history-file allowlist accepts the call fields. The upload envelope (`graph`, `ci`, `source.branch`) waits on a separate goodflows_web change.
