# Design

## Context

See proposal.md for motivation. The wire contract belongs to goodflows_web `add-workflow-run-storage` (`specs/workflow-run-storage/spec.md`, design D1 and D8). This change follows that contract except where "Server contract changes needed" below says the contract must change.

Current goodci state that shapes the approach:

- `history::write` builds a private `Record` (`history.rs`) and writes it only when `settings.history` is on. `RunFields` (run id, start, outcome, parallelism, `graph_sha256`, `max_in_flight`, host) are computed in `main.rs::invocation` after reporting finishes, from one signal snapshot.
- `history::graph_sha256` builds a borrowed `GraphDocument` (jobs with `name`, `command`, `depends_on`, `partitions`, `timeout_secs`, `shell`, `condition`, `steps`, plus a trailing `call` `{workflow, graph_sha256}` for workflow-call jobs since `add-workflow-call-jobs`/`0ccc2d4`), hashes it, and drops it.
- Each invocation, the root and every called workflow, has its own run id, its own `reporting::Session`, and its own spool run directory `<spool>/<run_id>/`. A spool directory is created today only when a job declares `reports`.
- `reporting::publish::Publisher` sends test-report snapshots with a shared 30-second budget, at most 3 attempts, and a fixed status mapping (`400/409/413/415/422` are rejections, `408` is pending and not retried). The endpoint path `/test-reports` and the `{id, report_id, status, duplicate}` acknowledgement are hard-coded. `deliver()` creates a new `Publisher` on each call.
- `reporting::source::capture` records `commit`, `branch` and `dirty`. History keeps only `commit` and `dirty`. Source is captured only when reporting jobs exist or local history is on.
- Job names are checked only for being non-blank and having no control characters. The 64-byte rule applies only to reporting jobs. `validate-job-names` (another thread, still unmerged) fixes this.
- `add-sprite-execution` (merged in `efa2b1e`) runs whole workflows on a Sprite. The remote runner (`__sprite-agent exec-workflow`) calls the same `main::execute`/`invocation` path with `Run::sprite_state` set. Its config is rewritten so `[reporting] spool_dir` points into the run's results, and its environment is `base_env()` plus the `[sprite] pass_env` values. The reporting `token_env` is denied there, so remote sessions never have a key: test reports are frozen, delivery is skipped with `credentials_unavailable`, and after collection the controller checks each spool with `sprite::trusted_spool` and publishes it with `reporting::replay::upload`. The source archive excludes `.git`, and the sprite-execution spec forbids injecting controller provenance into native history or report `source` fields.

## Goals / Non-Goals

**Goals:**
- One builder for the run record, so the uploaded and local documents differ only in the three optional upload keys.
- A local validator that mirrors the server rules exactly, so any `422` points at a contract change rather than a goodci bug.
- Reuse the spool, snapshot, acknowledgement, replay and publisher code. A run record is one more frozen item in an invocation's run directory.

**Non-Goals:**
- `history compare`, uploading old local history files, CI providers other than GitHub Actions, and a branch fallback from CI variables (see Open Questions).
- Any change to the test-report wire format, the local history file bytes, or `graph_sha256`.

## Decisions

### D1. Setting: `[reporting] history = true`, default `false`

`Reporting` gains `history: bool` (serde default `false`), the name D8 suggested. It controls upload only. Local history keeps its own precedence (`[settings]`/workflow/profile `history`, `--history`, `--no-history`, `--history-dir`), and neither setting implies the other. `--no-history` does not stop uploads: the flag is documented as controlling the history *file*, and coupling the two would make `--no-history` in CI silently drop server data. To skip uploads on one machine, leave the reporting key variable unset. The default is `false` because the record carries every configured command text (including the graph's commands for jobs that did not run), and a `[reporting]` block that today sends only test timings must not start sending command text without the user asking.

`[reporting]` is top-level only, so there is no per-profile override. Gating on credentials (D5) covers the common case "upload from CI, not from laptops".

*Alternative considered:* `upload_history`. Rejected: the key already sits inside `[reporting]`, and D8's spelling keeps the two repos consistent.

### D2. One record builder, optional upload keys

`history.rs` gets a public builder that returns the `Record` value for a run. `history::write` serializes it unchanged. Upload passes `Upload { graph, branch, ci }`. The new fields use `skip_serializing_if = "Option::is_none"`, so local bytes are unchanged:
- `source.branch` is added after `dirty`, and only when uploading and the captured source has a branch.
- `ci` is placed after `source`.
- `graph` is placed last, after `results`, because it is the largest key.

`graph_sha256` is refactored to return `(hash, GraphDocument)`, so the uploaded graph is the same document that was hashed. The hash is computed first over the untruncated text, and then a truncated copy is made by cutting every job `command`, step `command` and condition `text` to 1,024 bytes on a character boundary (the existing `truncate`). Called workflows are not inlined. A call job keeps its `call` object with the target's own hash, and the target's graph travels in the child invocation's own record. The document has the same depth as the hashed one.

Local files stay as they are: no branch (the run-history spec says so on purpose, because history files are often committed), no graph (it would add every command text to a committed file and duplicate what `graph_sha256` already identifies), and no CI context (meaningless off CI). An agent comparing local files still compares by `graph_sha256`.

*Alternative considered:* add all three to local files too. Rejected for the reasons above, and because it would change the bytes of every local file for no local reader.

### D3. CI context from GitHub Actions only

New module `reporting::ci` reads the caller's process environment once per invocation, never settings `env`, which only reaches commands. When `GITHUB_ACTIONS == "true"`, it sets `provider: "github_actions"` and adds each of the following that passes its rule:

| Key | Source | Rule (server) |
|---|---|---|
| `event` | `GITHUB_EVENT_NAME` | name rule |
| `run_id` | `GITHUB_RUN_ID` | 1–256 bytes, no control chars |
| `attempt` | `GITHUB_RUN_ATTEMPT` | ASCII digits only, 1 ≤ n ≤ 2^53−1 |
| `pull_request` | `GITHUB_REF` = `refs/pull/<n>/merge` | ASCII digits only, 1 ≤ n ≤ 2^53−1 |
| `runner` | `RUNNER_NAME` | name rule |
| `url` | `GITHUB_SERVER_URL` + `/` + `GITHUB_REPOSITORY` + `/actions/runs/` + `GITHUB_RUN_ID` | the server URL is an origin-only `https` URL; the repository is `owner/name` with `[A-Za-z0-9_.-]` segments; the run id is digits; the result parses as an absolute `https` URL with a host, at most 2,048 bytes |

Otherwise `ci` is absent. Variables that are empty, malformed or too long are dropped one at a time, and they never make the record invalid. The values are identities and links, not secrets, but they are still never printed in diagnostics.

### D4. Local validation mirrors the server and checks the bytes that will be sent

New module `reporting::run_contract` validates the serialized `serde_json::Value`, not the Rust structs, so it checks exactly what is sent and what a replay reads back. It implements every rule in the server spec, with the additions from "Server contract changes needed":
- The top-level, `host`, `source`, `ci`, `graph`, graph job, `partitions`, step, condition, result, `partition` and command allowlists, with optional keys absent rather than null.
- The name rule: 1–64 bytes, no control characters, not only whitespace. Text is at most 1,024 bytes. `run_id` is a lowercase canonical UUID. `started_at` matches `YYYY-MM-DDTHH:MM:SSZ` exactly.
- Integer kinds and bounds (nonnegative, positive, `exit_code` i32, at most 2^53−1). `requested_jobs` and `commands` have 1–1,024 entries. `graph.jobs` has 1–1,024 entries, and `depends_on` and `steps` have at most 1,024 each. Partitions satisfy `1 ≤ index ≤ count ≤ 64`.
- The outcome and status agreement rule.
- Graph consistency: unique names, known dependencies, no cycle, and every result naming a graph job.
- No U+0000 in any string, nesting depth at most 8, and an encoded body of at most 4,194,304 bytes.

Errors are reported as `(path, code)` from a fixed vocabulary, with paths in the server's `results[3].name` style. Only the first error is reported. Unknown keys are reported on their enclosing object, so no submitted value or key ever reaches output.

A record that fails is not frozen and not sent. The invocation emits `run_upload_skipped` with `reason: "invalid_record"` and `path`, and in text mode prints `run history upload skipped: invalid record at <path>`. The exit code and the local history file are unaffected. This is a goodci bug or a not-yet-supported config shape (for example, a job name over 64 bytes before `validate-job-names`), so nothing replayable is kept.

*Alternative considered:* reuse the test-report `contract.rs` field table. Rejected: that table describes flat JSONL records, while this document is nested. The new module still reuses `contract.rs`'s integer and text helpers.

### D5. Spool snapshot and manifest version 3

The run record is frozen only when `[reporting] history = true` and, at invocation start, the credentials resolve (`Credentials::Ready`: the key and URL variables are set and the URL is valid). Otherwise the invocation emits `run_upload_skipped` with `credentials_unavailable`, `invalid_url` or `spool_unavailable`, and builds and keeps nothing. This keeps laptops without a key from filling the spool with snapshots that nobody will replay. Remote Sprite invocations are the one exception (D11): they freeze without a key, because the controller replays the spool.

When upload is on and credentials are ready, `Session::start` creates the spool run directory even if no job declares `reports` (the manifest then lists no jobs). It also captures `Source` (removing the token variable from git, as today), and `main.rs` reuses that capture for local history.

Freeze order, after the outcome snapshot:
1. Build and validate the record.
2. Create `<run_dir>/run-record.json` (mode 0600, create-new, relative to the open run directory, refusing symlinks).
3. Atomically rewrite the already finalized manifest with `run_record: {"schema_version": 1}` and `version: 3`.

The file names `run-record*.json` contain a `.`, which `path_component` always encodes. They therefore cannot collide with a job directory. The acknowledgement goes in `run-record.ack.json`, and a server rejection in `run-record.rejected.json`.

Manifest versions: version 1 has no partitions and no run record, version 2 has partitions and no run record, and version 3 has a run record with or without partitions. Replay accepts versions 1–3 and refuses a version that does not match the content. An older goodflows refuses version 3 with its existing clear "unsupported manifest version" error rather than silently dropping the run record. A crash between steps 2 and 3 leaves a finalized manifest without `run_record`. Replay ignores the orphaned file, and the record is lost, which is the same trade-off as a crash before freezing a test report.

### D6. Ordering inside an invocation

Today the order is: execute → `session.finish` (collect and publish test reports) → outcome snapshot → history file → summary. The new order is: execute → `session.finish` → outcome snapshot → **build, validate, freeze and send the run record** → history file → summary, with `summary.duration_ms` measured after the run upload so it still covers all reporting. The run record must follow the snapshot because it carries `outcome`. A signal that arrives after the snapshot cancels the upload (`run_upload_pending` with `cancelled`, snapshot kept) without changing the outcome.

The `Session` keeps one `Publisher`, created on first use, for both test reports and the run record. This makes the 30-second budget truly invocation-wide. Today `deliver()` creates a new publisher on each call, and the run record must not get a fresh budget. Child invocations finish, including their upload, before their call job completes, as today.

### D7. Publisher: one client, two document kinds

`Target` keeps the validated origin and project. Requests name a `DocumentKind`:
- `TestReport { report_id }` posts to `/test-reports` and keeps its existing acknowledgement and status mapping unchanged.
- `WorkflowRun { run_id }` posts to `/workflow-runs`. Its acknowledgement is `data` with a UUID `id`, `run_id` equal to the sent id, and a boolean `duplicate`.

Status mapping for `WorkflowRun`:

| Response | Result | Reason |
|---|---|---|
| 200/201 with a valid acknowledgement | uploaded, `run-record.ack.json` written | — |
| 200/201 with an invalid or mismatched acknowledgement | pending | `invalid_acknowledgement` / `mismatched_acknowledgement` |
| 408, 429, 5xx, timeout, transport error | retried within the budget (3 attempts, `Retry-After` as today), then pending | `request_timeout`, `rate_limited`, `service_unavailable`, `server_error`, `timeout`, `transport_error` |
| TLS failure | pending, not retried | `tls_error` |
| 409 | rejected and terminal, `run-record.rejected.json` written | `conflict` |
| 400, 415 | rejected and terminal | `http_error` |
| 413, 422 | pending, not retried in this attempt | `request_too_large`, `rejected` |
| 401, 403, 404, 3xx, other 4xx | pending | as today |

`422` and `413` stay pending because the server, not the bytes, may change. A server older than the CLI contract rejects a valid record (D1 of the server design), and a deployment can lower its body limit. Replay resends them. `400` and `415` can only mean these bytes are broken, so resending cannot help. The User-Agent becomes `goodflows/<version> test-reports/1 workflow-runs/1`.

### D8. Replay

`reports upload <run-directory>` first handles test reports as today. Then, when the manifest lists `run_record`, it reads `run-record.json` without following symlinks, bounded to 4 MiB + 1 byte. It re-validates the record (D4) and checks that its `run_id`, `workflow` and `environment` equal the manifest's `workflow_run_id`, `workflow` and `environment`. It then skips a record that is acknowledged (`run-record.ack.json` with a matching `run_id`), reports one that is rejected, and sends one that is pending within the same budget. `reports_upload_summary` gains `run_record`: one of `acknowledged`, `pending`, `rejected`, `invalid`, or `null` when the manifest has none. A pending, rejected or invalid run record makes the command exit nonzero, the same as test reports.

### D9. Read commands

`goodflows history list` and `goodflows history show <run_id>` load the config (`--config` is allowed) and require a `[reporting]` block. They resolve `url_env`, `token_env` and `project` the same way the publisher does, and accept a `pk_` or `sk_` key. They reject every flag in `main.rs`'s `RUN_ONLY_FLAGS` (which includes `--env`, because `--environment` is the filter), with the same "`<flag>` cannot be used with …" error that `reports upload` and the `sprite` commands use, and `Cli::parser` hides those flags from `history` help as it does for `sprite`. Like `reports upload`, they install the signal handler so Ctrl-C stops a request. `show` checks that `run_id` is a lowercase canonical UUID before any request. `list` checks every filter with the server's rule before any request:
- `workflow`, `environment` and `event` use the name rule.
- `outcome` is one of the three outcomes.
- `commit` is 40 or 64 lowercase hex characters.
- `branch` is 1–256 bytes with no control characters.
- `graph_sha256` is 64 lowercase hex characters.
- `limit` is 1–200.
- `cursor` is a nonempty base64url string of at most 512 bytes.

A bad value is an argument error (exit 2) and no request is sent. Requests are GETs with `Accept: application/json` and no retries. A failure (transport, 401/403/404/422/5xx) prints a structural message with the HTTP status and exits 1. `show` reports a `404` as "run not found". Responses are untrusted. They are bounded (16 MiB), must parse as JSON with the documented `data` shape, and every string printed in text mode is escaped like job-provided text in the run report.

- Text `list`: one row per run, with the columns `STARTED`, `OUTCOME`, `WORKFLOW`, `ENV`, `COMMIT` (12 characters, `*` when dirty), `BRANCH`, `EVENT`, `DURATION` and `RUN ID`, followed by `next: --cursor <c>` when there is another page.
- Text `show`: a header (workflow, environment, outcome, start, duration, parallelism and `max_in_flight`, graph hash, source, CI and host), then one row per result (name and partition, status, duration, blocked, queued, exit code, summed CPU, peak RSS), then the linked test reports.
- `--json` emits one versioned event: `history_runs` `{runs, next_cursor}` for `list`, or `history_run` `{run}` with the server's `data` for `show`. `pk_` redaction comes from the server and is passed through as is.

### D10. Prerequisite: `validate-job-names`

Result names, `requested_jobs`, `caller_job`, graph job names and `depends_on` are all job names. Until `validate-job-names` lands, D4 rejects a run whose job names break the rule with `invalid_record`, so the server never sees one. This change does not add its own job-name validation. Implementation can start before `validate-job-names` merges, but a release that enables uploads must include it.

### D11. Sprite runs: frozen remotely, published by the controller

A remote Sprite invocation (`Run::sprite_state` is set) has no publisher key by design. If D5's credential gate applied there, no Sprite run would ever upload a run record. So when `[reporting] history = true`, a remote invocation creates its spool run directory, builds, validates and freezes the record exactly as in D5 and D6, and does not send it. It emits `run_upload_pending` with reason `credentials_unavailable`, because, unlike the laptop case, the snapshot is kept and will be published. Test reports already behave this way remotely. An invalid record is still `run_upload_skipped` with `invalid_record`.

The controller needs no new publication path. After collection it already walks the root's and each bound child's spool, checks it with `trusted_spool`, and calls `replay::upload`, which (D8) now sends the run record after the reports. `trusted_spool` gains one check: a manifest that lists `run_record` is refused unless the controller's trusted configuration has `[reporting] history = true`. A remote config that turned upload on cannot make the controller send command text the caller did not opt into. Replay's own checks cover the rest: re-validation and matching `run_id`, `workflow` and `environment` against the manifest, which `trusted_spool` has already bound to this invocation. As today, delivery never changes the run's exit code. A detached run's record waits in the collected bundle for `reports upload`.

What a remote record contains:
- **No `source`.** The archive excludes `.git`, so native capture finds no commit, and the sprite-execution spec forbids injecting the controller's HEAD, branch or dirty state. The record is still valid, because `source` is optional on the server, but analytics cannot place these runs on a commit. Adding controller provenance would need a sprite-execution spec change and is left as an open question.
- **`ci` only through `pass_env`.** The remote runner's environment is `base_env()` plus forwarded values, so `ci` appears only when `[sprite] pass_env` lists `GITHUB_ACTIONS` and the other D3 variables. Forwarded values also reach hooks and jobs, which the README must point out. None of these names is denied by sprite preflight.
- **`host`** is the Sprite's, and `duration_ms` is the remote invocation's, matching the existing rule that transport and collection time are not added to native metrics.

*Alternative considered:* the controller builds the run record from the collected events. Rejected: it would duplicate the builder, and it could not reproduce the remote `max_in_flight`, host and graph exactly.

## Contract alignment

Checked against `src/history.rs` on `main` at `87da18e` (`git diff main -- src Cargo.toml Cargo.lock` is empty on this branch) and against two history files produced by a binary built from that tree. `goodflows 0.6.0`, compiled with `rustc 1.88.0`, ran `goodflows run --history --json` in a clean git checkout whose only file was `fixtures/goodflows.toml`: a `default` workflow with job `prepare` (`echo prepare`) and job `call-suite` (`workflow = "suite"`, `depends_on = ["prepare"]`), and workflow `suite` with job `work` (`echo work`). The command exited 0. The bytes it wrote are `fixtures/parent-default.json` (sha256 `885e038c0f9b987034b408e021199f81da87b707b7f19d97781cc9a1ff79b6dc`) and `fixtures/child-suite.json` (sha256 `72e944d953fea14064d52c743e85c247f1bbdcaacbf5a054def8053bab0020b1`). They are compact JSON with one trailing LF. Do not reformat them.

`schema_version` stayed 1 when the call fields were added. `history::write` still sets `schema_version: 1`. `git blame` shows that field, and the assignment, coming from `2b19a0a4` (2026-11-04, `feat: record run history`). Commit `999f80ec27c7b9b61c9fb85f374800effe30a317` (2026-11-07, `feat: add workflow-call jobs`) added `parent_run_id` and `caller_job` on `Record` and `workflow` and `child_run_id` on `Instance` in `src/history.rs`, and the requirement "History links workflow-call invocations" in `openspec/specs/run-history/spec.md`. That requirement says the additions keep schema version 1. Both golden files are `"schema_version": 1` and contain those fields: the parent result `call-suite` has `workflow` `suite` and `child_run_id` `0e54fb55-a115-4ac7-80fe-8fde26e76afb`, and the child file's `run_id` is that same id, with `parent_run_id` `fb7d2711-6b67-4f5a-9104-492032a9e1e5` and `caller_job` `call-suite`.

Presence comes from serde on the writer. A field with no `skip_serializing_if` is always present, and an `Option` there is JSON `null`. `skip_serializing_if = "Option::is_none"` omits the field, so it is never `null`. `skip_serializing_if = "<[_]>::is_empty"` omits an empty array, so the field is never `null` and never `[]`. Types below are the JSON types in these files and in that serializer.

### Document

| Field | JSON type | Presence | Null |
|---|---|---|---|
| `schema_version` | integer, value 1 | always | never |
| `workflow` | string | always | never |
| `environment` | string | omitted when no `--env` profile was selected | never |
| `requested_jobs` | array of strings | omitted when `--jobs` was empty, which includes every child invocation | never |
| `run_id` | string, lowercase UUID | always | never |
| `parent_run_id` | string, lowercase UUID | present on a child, omitted on a root | never |
| `caller_job` | string | present on a child together with `parent_run_id`, omitted on a root | never |
| `started_at` | string `YYYY-MM-DDTHH:MM:SSZ` | always | never |
| `outcome` | string `passed`, `failed`, or `cancelled` | always | never |
| `duration_ms` | integer | always | never |
| `parallelism` | integer | always | never |
| `graph_sha256` | string, 64 lowercase hex digits | always | never |
| `max_in_flight` | integer | always | never |
| `host` | object | always | never |
| `source` | object | omitted when no commit was captured | never |
| `results` | array of result objects | always | never |

The golden parent omits `environment`, `requested_jobs`, `parent_run_id`, and `caller_job`. The golden child omits `environment` and `requested_jobs` and includes `parent_run_id` and `caller_job`. Both include `source`.

### `host`

| Field | JSON type | Presence | Null |
|---|---|---|---|
| `os` | string | always | never |
| `arch` | string | always | never |
| `logical_cpus` | integer | always | never |
| `mem_total_bytes` | integer | omitted, together with `mem_available_bytes`, when the platform does not report both | never |
| `mem_available_bytes` | integer | same as `mem_total_bytes` | never |

Both golden files include both memory fields (`linux`, `x86_64`).

### `source`

| Field | JSON type | Presence | Null |
|---|---|---|---|
| `commit` | string, 40 or 64 hex digits | always when `source` is present | never |
| `dirty` | boolean | omitted when git status was unavailable; otherwise `true` or `false` | never |

History does not write `source.branch`. Both golden files have `source.commit` and `"dirty": false`.

### `results` entries

| Field | JSON type | Presence | Null |
|---|---|---|---|
| `name` | string | always | never |
| `partition` | object | omitted for an unpartitioned instance | never |
| `workflow` | string | omitted unless the job is a workflow call | never |
| `child_run_id` | string, lowercase UUID | omitted unless that call activated a child | never |
| `status` | string `passed`, `failed`, `skipped`, `timed_out`, or `cancelled` | always | never |
| `exit_code` | integer, or null | always | null when the instance has no exit code |
| `command` | string, or null | always | null when the job has no command |
| `error` | string, or null | always | null when there is no error |
| `duration_ms` | integer | always | never |
| `started` | boolean | always | never |
| `blocked_ms` | integer | always | never |
| `queued_ms` | integer | always | never |
| `commands` | array | omitted when the instance spawned nothing | never |

In the golden parent, `prepare` has `exit_code` 0, `command` `"echo prepare"`, and `error` null, and it omits `partition`, `workflow`, and `child_run_id`. `call-suite` has `workflow`, `child_run_id`, `exit_code` null, `command` null, and `error` null, and it omits `partition` and `commands`. Every result in both files has `"error": null`.

### `partition`, when present

| Field | JSON type | Presence | Null |
|---|---|---|---|
| `index` | integer | always | never |
| `count` | integer | always | never |

Neither golden file has a partition. The object is what `config::Partition` serializes.

### `commands` entries, when present

| Field | JSON type | Presence | Null |
|---|---|---|---|
| `kind` | string `condition` or `command` | always | never |
| `step` | string | omitted for a job command or a job shell condition; the step id when the process belongs to a step | never |
| `wall_ms` | integer | always | never |
| `cpu_user_ms` | integer | omitted when the measurement is unavailable | never |
| `cpu_system_ms` | integer | omitted when the measurement is unavailable | never |
| `peak_rss_bytes` | integer | omitted when no sample saw a live process with resident memory | never |

The golden commands are `kind` `command` with `wall_ms`, `cpu_user_ms`, and `cpu_system_ms`. They omit `step` and `peak_rss_bytes`. These processes were short enough that the sampler recorded no resident set.

The history file does not contain the graph document. `graph_sha256` is the hash of that document, and a workflow-call job adds a trailing `call` object (`workflow`, `graph_sha256`) to the hashed document only. That object is not in either golden file.

### Server allowlist

`agoodway/goodflows_web` is not visible to the GitHub credentials on this run (`gh` and the GitHub API both return 404), so this pass did not re-read PR #7's `specs/workflow-run-storage/spec.md` or `design.md`. The accepted and rejected keys are therefore not relisted here as if they had been read again. The comparison already recorded in "Server contract changes needed" stands as that earlier reading: the result allowlist has `partition` and `commands` beyond the required keys, and the document allowlist has no `parent_run_id` or `caller_job`. Under that reading, `fixtures/parent-default.json` is rejected for `results[1].workflow` and `results[1].child_run_id`, and `fixtures/child-suite.json` is rejected for `parent_run_id` and `caller_job`. Both are `schema_version` 1.

The same files show a second check the server reading has to get right. `command`, `exit_code`, and `error` are always present, and they are JSON `null` when the instance has no command, no exit code, or no error. A rule that says "optional keys are omitted rather than null" fits these three fields only when they are required and nullable. Treating them as omit-only would reject every golden result on `error`, and the call result on `command` and `exit_code`, before the four new keys matter.

**Recommendation:** keep `schema_version` 1 and ask the server to accept `parent_run_id`, `caller_job`, `results[].workflow`, and `results[].child_run_id` as optional keys on that version. The writer and `openspec/specs/run-history` already emit them as version 1, and both golden files are version 1 with those keys set. Bumping the version would describe a document this CLI does not write. `command`, `exit_code`, and `error` stay required and nullable, matching every history file since `2b19a0a4`. Open question 1 is the confirmation, because the merged allowlist could not be opened here.

## Server contract changes needed

These are the differences between goodci `main` (checked again at `87da18e`, after the sprite-execution merge, which did not change `history.rs` or the graph) and goodflows_web `add-workflow-run-storage`. They come from comparing that change's `specs/workflow-run-storage/spec.md` and `design.md` with `src/history.rs`, `src/runner.rs` and `src/main.rs`. Without them, any run that uses workflow calls gets a `422`. goodci keeps those records pending (D7), so they can be replayed once the server accepts them.

1. **Graph job `call` key (new key).** `GraphJob` has an optional trailing `call` object `{"workflow": <name>, "graph_sha256": <64 lowercase hex>}`. It is present only for workflow-call jobs and absent for ordinary jobs, which keeps their bytes and hashes. The server's graph job is "exactly" eight keys, so it rejects `call` as an unknown field. **Proposed:** allow an optional `call` with exactly `workflow` (name rule) and `graph_sha256` (64 lowercase hex), with null invalid. A call job has `command: null` and `steps: []`, and may have a `condition`, which the existing rules already allow. `pk_` redaction does not need to touch `call`. Update design Context and D1's description of `GraphDocument` to include it.
2. **Called workflows are referenced by hash, not nested.** A called workflow's jobs are not inlined. `call.graph_sha256` is the target's own `graph_sha256` under the same profile. The target's graph arrives in the child invocation's own run record, whose `graph_sha256` equals that value. Each workflow is hashed once per invocation (`0ccc2d4`). As a result, depth is unchanged: the deepest path is still document › `graph` › `jobs` › job › `steps` › step › `condition` (7), and `call` sits at depth 5 with scalar fields. No depth-limit change is needed. **Proposed:** state this in the server spec, so analytics joins a call to its target by `call.graph_sha256` = child `graph_sha256`. Note also that a call skipped by a false condition or fail-fast creates no child run, so that target's graph is not uploaded anywhere.
3. **Top-level `parent_run_id` and `caller_job` (new keys).** A child invocation's record has `parent_run_id` (lowercase canonical UUID) and `caller_job` (job name), both present together and only in child records. **Proposed:** allow both as optional and require them together, `parent_run_id` as a lowercase canonical UUID and `caller_job` under the name rule. Add `caller_job` to the "Names follow one rule" list. Consider copying `parent_run_id` into a column so that the children of a run can be listed.
4. **Result `workflow` and `child_run_id` (new keys).** Each workflow-call result has `workflow` (the target name, present even when the call was skipped) and, once a child was activated, `child_run_id` (lowercase canonical UUID). The server's result allows only `partition` and `commands` beyond the required keys. **Proposed:** allow both as optional, `workflow` under the name rule and `child_run_id` as a canonical UUID, with `child_run_id` allowed only alongside `workflow`. Add `workflow` to the name-rule list.
5. **Graph job keys are not "exactly" eight.** Because of item 1, the requirement "The graph describes the jobs that ran" must list `call` as an optional ninth key. Its OpenAPI `WorkflowRunGraphJob` must not list `call` in `required`.
6. **Design Context names the wrong source of job-name validation.** The server design says job-name checks live in `validate_jobs` / `valid_report_name`. That is still accurate, but `validate-job-names` will change it. No contract change is needed, only a reminder that task 7.4's version gate must name a goodci release that includes both `validate-job-names` and this change.
7. **`requested_jobs` for child records.** A child invocation always runs its whole workflow and omits `requested_jobs`. The server already accepts the key's absence. No change is needed, but this explains why most child records lack it.
8. **Branch on CI is often absent.** `source.branch` comes from `git symbolic-ref`, which fails on GitHub Actions `pull_request` checkouts (detached `refs/pull/<n>/merge`). Pull-request runs therefore usually have no `source.branch`, and the server's `branch=main&event=push` filter still works. No contract change is needed. `add-workflow-analytics` should not assume every run has a branch, and should use `ci.pull_request` for pull-request runs.

9. **Sprite runs usually have no `source`.** Records from `goodflows run --sprite` omit `source` (D11). The server already accepts this. No change is needed, but `add-workflow-analytics` must not assume that every run has a commit, and the list's `commit=` and `branch=` filters will never match these runs.

Items 1, 3, 4 and 5 are required. Items 2, 6, 7, 8 and 9 are clarifications.

## Risks / Trade-offs

- [Command text leaves the machine] → Opt-in `history = true`. The server redacts it for `pk_` reads. The README states that graph commands include jobs that did not run. This is the same exposure the server design already accepts.
- [Contract drift with the server] → Local validation implements the full server rule set, and `422` stays pending, so a lagging server costs only a replay. Fixture tests share one full record with the server's task 2.1 fixtures (tasks 2.4 and 6.3).
- [Spool growth on CI agents that keep their disk] → Snapshots are frozen only when credentials are ready. A snapshot is one file of a few KB per invocation, next to test-report snapshots that are already kept. Spool pruning is out of scope, as it is for test reports.
- [The run record uses up budget left by large test reports] → The run record is sent last and may be left `budget_exhausted`. It stays replayable, and `reports upload --budget` covers slow links.
- [`--no-history` users may expect no upload] → The README and `--help` text for `--no-history` say it controls the local file only. Unsetting the key variable stops uploads.
- [Remote Sprite spools fill with run records] → They live in the identity's results and are kept until `sprite destroy`, like remote test reports. Each one is a few KB.
- [Sprite runs are hard to place on a commit] → They have no `source` (D11). This is a sprite-execution rule, not something this change can fix on its own; see Open Questions.
- [Remote runs show `credentials_unavailable` before the controller publishes] → The relayed remote event says the record is pending rather than skipped, and the controller's `reports_upload_summary` that follows reports the final `run_record` state.

## Migration Plan

1. Land `validate-job-names`, then this change. Uploads stay off unless `[reporting] history = true`, so existing configs see no change. The only visible differences are the new subcommand and the manifest version 3 for upload-enabled runs.
2. Deploy `add-workflow-run-storage` with the server contract changes above before announcing the setting.
3. Rollback: set `history = false` (or remove it). Spool directories with version 3 manifests need this or a newer goodflows to replay.

## Open Questions

1. **Stay on `schema_version` 1 if the merged server allowlist still rejects the call fields?** (major)
   - **Why it matters.** The golden files are version 1 and contain `parent_run_id`, `caller_job`, `results[].workflow`, and `results[].child_run_id`. A server that rejects those keys answers 422, and the upload stays pending, for every workflow-call run. They also contain JSON `null` for `error` on every result, and for `command` and `exit_code` on a call. This pass could not re-read goodflows_web PR #7, so the merged allowlist is the reading in "Server contract changes needed", not a fresh copy of the spec.
   - **Options.**
     1. Keep `schema_version` 1 and ask the server to allow the four keys as optional on that version, with `command`, `exit_code`, and `error` required and nullable.
     2. Bump new history files to `schema_version` 2 and ask the server to accept version 2.
     3. Keep the local file at version 1 and omit the four keys from the uploaded copy until the server allows them.
     4. Stop writing the four keys in the local file as well, until the server allows them.
   - **Recommended default:** option 1. The writer and `openspec/specs/run-history` have emitted these keys as version 1 since `999f80e`.

2. **Fill `source.branch` from GitHub Actions ref variables when git has no branch?** (minor)
   - **Why it matters.** A `pull_request` checkout is detached at `refs/pull/<n>/merge`, so `git symbolic-ref` fails and the uploaded record usually has no `source.branch`. List filters on `branch` then miss those runs. Test-report `source.branch` is captured the same way.
   - **Options.**
     1. Leave `source.branch` absent when git reports none.
     2. Use `GITHUB_HEAD_REF` for pull requests.
     3. Use `GITHUB_REF_NAME` when `GITHUB_REF_TYPE=branch`.
     4. Try `GITHUB_HEAD_REF`, then `GITHUB_REF_NAME`.
   - **Recommended default:** option 1. The run's branch then stays the same value test reports already store, and the fallback can be added later without a contract change.

3. **Put the controller's commit and branch on a Sprite run record?** (major)
   - **Why it matters.** A remote record has no `source`, because the archive has no `.git`. `commit` and `branch` filters never match those runs. Copying the controller's HEAD would put controller provenance into a record the sprite-execution spec currently leaves without `source`.
   - **Options.**
     1. Keep omitting `source` when native capture finds none.
     2. Copy the controller's commit and branch into `source` before the controller uploads.
     3. Add a separate provenance key, which needs a server contract change.
     4. Record the controller commit only, and leave branch absent.
   - **Recommended default:** option 1. The sprite-execution spec keeps controller provenance out of native `source`, and the server already accepts a missing `source`.
