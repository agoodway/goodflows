# Proposal

## Why

goodflows records a run-history document for each workflow run, but only on the machine that ran it, and only when local history is on. On CI that machine is thrown away, so the runs most worth comparing across commits are lost. The GoodFlows server change `add-workflow-run-storage` (goodflows_web) adds `POST/GET /api/v1/projects/:project/workflow-runs` to store and read these documents. Its design D8 lists the CLI work. This change is that CLI side: upload each run's record, keep it for replay when delivery fails, and read stored runs back.

## What Changes

- New `[reporting]` setting `history` (boolean, default `false`). When it is `true` and the reporting key is available, every invocation (root and each called workflow) builds its run record in memory and uploads it with `POST /api/v1/projects/:project/workflow-runs`. This happens whether or not local history is enabled. `--history`, `--no-history` and `--history-dir` still control only the local file. Without a `[reporting]` block nothing is uploaded.
- The uploaded record is the local history record plus three optional keys the server accepts for analytics:
  - `graph`: the canonical job graph that `graph_sha256` hashes, with each command and condition text cut to 1,024 bytes. The hash is still computed over the full text.
  - `source.branch`: the branch that reporting already captures.
  - `ci`: GitHub Actions context (`provider`, `event`, `run_id`, `attempt`, `pull_request`, `runner`, `url`). A value that fails its rule is left out.
  
  The local history file is unchanged: it still has no branch, no graph and no CI context.
- Before uploading, goodflows validates the record locally against every rule in the server contract: field allowlist, name and text rules, integer bounds, the 1,024-entry list limits, graph consistency, nesting depth 8 and the 4 MiB body limit. A record that fails is never frozen or sent. It is reported with the JSON path of the first violation, and the exit code is unchanged.
- The validated record is frozen as an immutable snapshot (`run-record.json`) in the invocation's reporting spool directory, next to the test-report snapshots. It is sent within the same 30-second publishing budget, after that invocation's test reports. The spool manifest goes to version 3 when it lists a run record.
- Responses: `201` and `200` with a matching acknowledgement mean done. `409` is a permanent conflict: it is recorded and never resent. `408`, `429`, `5xx` and transport errors are retried within the budget. `422` and `413` keep the snapshot pending for a later replay. New events: `run_uploaded`, `run_upload_pending` and `run_upload_skipped`.
- `goodflows reports upload <run-directory>` also replays a pending run record with its original `run_id` and bytes. It counts the record in its summary and exit code.
- Sprite runs (`goodflows run --sprite`): the remote runner has no publisher key, so it freezes each invocation's run record without one and leaves it pending. After collection, the controller publishes it with the test reports through the existing trusted-spool replay. The controller captures `commit`, `branch`, and archive-derived `dirty` while it builds the source tarball, sends them on the submission header, and the remote runner writes that `source` into the root and child history and test-report records. Jobs do not see it. `source` is omitted when HEAD cannot be read. `branch` is omitted when git reports none. Records carry `ci` only when `[sprite] pass_env` forwards the `GITHUB_*` variables.
- New read commands that use the `[reporting]` endpoint and key:
  - `goodflows history list`, with the filters `--workflow`, `--environment`, `--outcome`, `--commit`, `--branch`, `--event`, `--graph-sha256`, `--limit` and `--cursor`.
  - `goodflows history show <run_id>`.
  
  Both print a human table by default. With `--json` they print one versioned event that carries the server's data.
- Not included: `goodflows history compare`, uploading local history files written earlier, CI providers other than GitHub Actions, and server-side changes (those are listed in design.md under "Server contract changes needed").

## Capabilities

### New Capabilities

None. Uploading and reading runs extends `run-history`, and delivery reuses `test-timing-publishing`.

### Modified Capabilities

- `run-history`: History commands write no history files. The uploaded record (graph, branch, CI context), its local validation, delivery outcomes, and the `history list`/`show` commands are new requirements.
- `test-timing-publishing`: The `[reporting]` block gains `history`. The invocation-wide publishing budget and failure handling now cover run records. The manifest version 3, run-record replay, and new reporting events are added.
- `settings-environment`: CI context, like the publisher endpoint, is read only from the caller environment and never from settings `env`.
- `sprite-execution`: Remote reporting collection also covers run records. The remote runner freezes them without a key, and the controller publishes them after collection only when its trusted configuration enables upload.

## Impact

- Code: `src/history.rs` (shared record builder with the optional upload keys, the graph document returned alongside its hash), `src/config.rs` (`Reporting.history`), `src/reporting/{mod,publish,replay,source}.rs` (endpoint per document kind, run-record ack and status mapping, spool snapshot, manifest v3, replay), a new run-record validator and CI-context module under `src/reporting/`, `src/main.rs` (new `history` subcommand, ordering of the outcome snapshot and run upload before the summary), `src/sprite/mod.rs` (`trusted_spool` accepts a run record only when the trusted config enables upload), and `src/output.rs` (text rendering of the new events and the history output).
- Wire: new requests to `/api/v1/projects/:project/workflow-runs` (POST, GET list, GET show). The test-report wire format is unchanged.
- Prerequisites: the goodci change `validate-job-names`, being written in another thread, must land first, so every job name passes the name rule. Until then, local validation reports affected runs as `invalid_record`. The server change `add-workflow-run-storage` must be deployed, with the contract changes listed in design.md, before uploads are enabled in a release.
- Builds on `add-sprite-execution` (merged in `efa2b1e`): its remote runner, spool collection and controller publication are reused unchanged except for `trusted_spool`.
- Docs: README "Run history" and "Test timing reports" sections, and `docs/sprites.md` (run records on Sprite runs, forwarding `GITHUB_*` with `pass_env`).
