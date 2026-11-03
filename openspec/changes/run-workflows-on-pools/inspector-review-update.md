# Inspector Review-Update — run-workflows-on-pools

**Reviewed:** 2026-11-02
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement

## Summary

The change adds `goodflows run … --pool <slug>`. It snapshots the Git working tree, leases one goodflows_web pool member, runs the whole workflow there in a single exec, and relays the versioned event stream back.

The artifacts were already detailed, `openspec validate --strict` passed, and the protocol claims match `../goodflows_web/docs/pool-protocol.md`. The gaps were these:

- In the default configuration, the reporting credentials and the pool key are the same variable.
- The ordering of signals versus a confirmed exit, and what a second signal does, were not defined.
- The `remote_status` and bootstrap-failure contracts were not specified.
- Output backpressure had no defined handling.
- The Git enumeration missed some cases: gitlinks, nested repositories, absolute links and sparse paths.
- The CLI wiring details were incomplete.

All findings are now patched, and the change still validates.

**Original counts:** Critical: 0 · Warning: 9 · Suggestion: 14
**Patches:** Auto-patched: 13 · User-guided: 0 · Model-recommended: 10 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/run-workflows-on-pools/proposal.md`
- Design: `openspec/changes/run-workflows-on-pools/design.md`
- Tasks: `openspec/changes/run-workflows-on-pools/tasks.md`
- Deltas:
  - `openspec/changes/run-workflows-on-pools/specs/pool-workflow-execution/spec.md` (new capability)
- Canonical specs consulted:
  - `openspec/specs/job-partitions/spec.md`
  - `openspec/specs/test-timing-publishing/spec.md` (has uncommitted edits)
- Other active changes consulted: `add-settings-env`, `profile-partition-counts`, `improve-run-report`
- External: `../goodflows_web/docs/pool-protocol.md`, `../goodflows_web/app/lib/good_flows/pools/exec_server.ex`

## Patches applied

13 findings were auto-patched. 0 were patched after user guidance. 10 were patched from model recommendations. 0 were skipped.

### Auto-patched

1. **Origin validation and socket URL derivation** — `design.md` §1, `spec.md` "Pool credentials are transport credentials", `tasks.md` 1.1
   - `GOODFLOWS_URL` must be an origin only.
   - Validation reuses the publisher's `endpoint` check (`src/reporting/publish.rs`) and its TLS root policy.
   - The socket URL maps `https`→`wss` and `http`→`ws`, and the bearer token uses unpadded base64url.
   - Added an "Invalid origin" scenario.
2. **Local `pk_` / missing key preflight** — `spec.md` "Missing or public key" scenario, `design.md` §1 → a missing key or a public key is rejected locally before any request.
3. **Global flags and rejection sites** — `design.md` §1, `tasks.md` 1.1
   - `--pool`/`--project` are `global = true`.
   - They are rejected explicitly, with exit 1, in the list, validate, init and `reports upload` branches.
4. **`--project` without `--pool` scenario** — `spec.md` → added the "Project without pool" scenario.
5. **Git enumeration gaps** — `design.md` §2, `spec.md` "Snapshot transfer is bounded and confined", `tasks.md` 2.1/2.2
   - Tracked entries are listed with `ls-files --cached --stage -z`, so gitlinks (mode 160000) are visible.
   - The snapshot rejects untracked nested repositories, absolute symlinks, and skip-worktree paths that are missing from disk.
   - The archive builder uses `follow_symlinks(false)` and writes deterministic tar headers.
   - Skip-worktree paths fail rather than being silently omitted, which follows the design's existing "never silently send an incomplete checkout" rule.
6. **Dependency prerequisites** — `tasks.md` 1.2
   - Enable reqwest's `stream` feature.
   - Use the rustls `ring` provider only, through one `ClientConfig` shared by REST and WebSocket. Two providers would make `ClientConfig::builder()` panic at runtime.
7. **Exact forwarded flag names** — `design.md` §3, `tasks.md` 4.1 → `-c/--config`, `--env`, `--jobs`, `-p/--parallelism` (including 0), `--fail-fast` / `--no-fail-fast`.
8. **Verbatim event forwarding** — `design.md` §5, `spec.md` "Output preserves workflow event semantics", `tasks.md` 4.3
   - `Output::emit` re-tags `workflow`/`environment` and `render` sets `schema_version` (`src/output.rs:71-76,127`).
   - Added a requirement for a non-retagging forwarding path.
9. **16 MiB bound vs worst-case summary** — `design.md` §5, `tasks.md` 4.3 → the bound explicitly includes `summary`, and a large-summary test was added.
10. **Pre-summary exit codes** — `design.md` §5, `spec.md` "The member resolves configuration and tools" → inner exit codes 1 and 2 propagate unchanged; bootstrap failures map to 1.
11. **Exec reply ambiguity scope** — `design.md` §6, `tasks.md` 5.1
    - A push is ambiguous only after a socket drop or rejoin.
    - On a healthy socket the client waits for a slow reply, because `pool-protocol.md:145` guarantees the reply arrives before any output.
12. **Join before upload** — `design.md` §1, `spec.md` "One member owns the entire workflow", `tasks.md` 3.3
    - The channel is joined and the heartbeat keeps running during the HTTP PUT.
    - Otherwise an upload of up to 256 MiB could exceed the 60 s unjoined grace.
13. **Numeric bounds in the spec and remaining deadlines** — `spec.md`, `design.md` §6, `tasks.md` 4.2
    - The spec now states 16 MiB per event, the 60 s reconnect window, the 30 s cleanup and the 120 s acquisition limit.
    - The design gives concrete deadlines: 30 s for connect/request, and 30 s without progress for the upload.
    - Task 4.2 now verifies that a pending remote reporting status is relayed and the exit code is unchanged.

### Model-recommended patches

1. **The pool key doubles as the default reporting token** — `design.md` §4, `spec.md` "Remote environment and reporting remain explicit"
   - **Chose:**
     - Forward the key when the reporting `token_env` names it, which is the default.
     - It reaches only the remote goodflows process and is removed from every job/step environment.
     - Document that same-user processes can see it via `/proc/<ppid>/environ`, and recommend a separate `token_env`.
     - Added the "Default reporting credential names" scenario and a risk entry.
   - **Rationale:**
     - `src/config.rs:55-61` defaults reporting to `GOODFLOWS_URL`/`GOODFLOWS_API_KEY`, so the old line "never forward a pool key merely because reporting exists" contradicted the default.
     - The original design already allowed the shared-variable case. Refusing to forward would silently break default remote reporting.
2. **Signal arriving after a confirmed exit** — `design.md` §5, `spec.md` "Signal after confirmed result"
   - **Chose:** a signal before the confirmed `exit` returns 130/143. A signal after it only cuts cleanup short, and the confirmed 0/1 code stands.
   - **Rationale:** this resolves the conflict between "signal codes take precedence" and "preserve a confirmed status" without losing a real check result.
3. **`remote_status` schema and stderr carrier** — `design.md` §5, `spec.md`
   - **Chose:**
     - Shape: `{"type":"remote_status","phase","pool","project","message"?}`.
     - Phases: snapshot, session_created, uploaded, member_ready, executing, reconnecting, remote_stderr, releasing, released.
     - Remote stderr is relayed as `remote_stderr` events, capped at 64 KiB in total.
   - **Rationale:** a new versioned event needs a defined contract, and in `--json` mode stderr had no legal carrier.
4. **Bootstrap failure contract** — `design.md` §3, `spec.md`, `tasks.md` 4.1
   - **Chose:**
     - Missing binary: exit 127 with `goodflows-bootstrap: missing`.
     - Version mismatch: exit 3 with `goodflows-bootstrap: version <actual>`.
     - The client maps either case to an `error` naming the expected and actual versions, and returns 1.
   - **Rationale:** the spec requires recipe guidance and the expected/actual versions. Without a marker, an opaque exec cannot be told apart from an inner config error.
5. **`improve-run-report` rerun hint and wall time** — `design.md` §5, `proposal.md` Impact, `tasks.md` 4.4/6.3
   - **Chose:**
     - Leave the summary renderer unchanged.
     - In remote text mode, print `Remote: pool=<pool> project=<project>; append --pool <pool> to rerun remotely` before the summary.
     - Document that the reported duration covers member execution only.
   - **Rationale:** this is the smallest change, and it does not alter another active change's renderer.
6. **Exit-code goal wording** — `design.md:13`, `tasks.md` 1.3
   - **Chose:** keep exit 1 for transport failures. They are told apart by the `error` event and the absence of a confirmed summary. Remote-failure codes are documented in the README.
   - **Rationale:** this matches the existing README "runtime error" exit row and preserves compatibility.
7. **Second signal during cleanup** — `design.md` §6, `spec.md` "Every terminal path releases the member", `tasks.md` 5.2
   - **Chose:** a second SIGINT/SIGTERM abandons cleanup, warns that session expiry will reset the member, and exits immediately. Added a scenario for it.
   - **Rationale:** `cancellation()` (`src/main.rs:348-369`) handles only the first signal. Without this, a 30 s cleanup could only be escaped with SIGKILL.
8. **Output backpressure** — `design.md` §5, `spec.md` "Stalled output consumer", `tasks.md` 4.3
   - **Chose:** a bounded 32 MiB relay queue, decoupled from the socket and heartbeat task. Overflow produces a structural `error`, then cancel and release.
   - **Rationale:** `Output::emit` blocks on a 32-slot writer channel (`src/output.rs:30,80-89`), and the server push has no flow control. Unbounded buffering or a stalled socket would both break the design's guarantees.
9. **Proxy policy** — `design.md` §1 and Risks, `spec.md` limitations, `tasks.md` 1.2/1.3
   - **Chose:** in v1, both REST and WebSocket connect directly and ignore proxy variables. This is documented.
   - **Rationale:** reqwest honours `HTTPS_PROXY` but common WebSocket clients do not. A single consistent policy avoids "REST works, socket fails" behind a proxy.
10. **Readiness timeout scaling** — `design.md` §6, `spec.md`, `tasks.md` 3.3
    - **Chose:** 120 s plus 60 s per started 512 MiB of expanded payload, which is at most 360 s.
    - **Rationale:** hydrating a 2 GiB workspace can exceed a flat 120 s. The specialist also suggested a `GOODFLOWS_POOL_READY_TIMEOUT_SECS` override, which was left out as new configuration surface the proposal does not mention. Add it later if needed.

### Skipped

_None._

## Critical

_None._

## Warning

_None remaining._

## Suggestion

1. **The `GOODFLOWS_ENVIRONMENT` reservation exists only in the uncommitted canonical spec** — `openspec/specs/test-timing-publishing/spec.md`
   - This is outside this change's scope and was not patched.
   - The working tree edits the canonical spec directly, not through an active change. Reconcile it before archiving any change that touches `test-timing-publishing`.
   - Forwarding `--env` keeps remote behaviour correct.

## Alignment notes

- **Other active changes:**
  - `add-settings-env` has no conflict. On a remote run, "caller environment" means the member environment plus the forwarded reporting credentials.
  - `profile-partition-counts` has no conflict. It adds no flags, and the exact-version bootstrap guards against schema drift.
  - `improve-run-report` interacts through its rerun hint and wall time; this was resolved by model-recommended patch 5. Its `log_dir` field stays absent remotely because `--log-dir` is rejected.
- **Canonical specs:**
  - `job-partitions` is consistent; partitions run inside the remote binary.
  - `test-timing-publishing` is consistent: publisher scrubbing and best-effort semantics are reused, and the origin rule is now shared.
- **Codebase assumptions verified:**
  - `goodflows run [workflow]`, and running with no subcommand.
  - The `--jobs`, `-p/--parallelism` and `--fail-fast` / `--no-fail-fast` flags.
  - The codebase is already async with tokio; reqwest uses rustls/ring with redirects disabled.
  - `rust-version` 1.88, and CI runs on Linux/macOS.
  - Exit codes 130/143, `schema_version: 1`, and the `error` and `summary` events.
  - `Config::load` canonicalization and `valid_project_slug`.
  - A process group per command, and the reporting default env names.
  - No remote code exists yet, and `tests/common` has no WebSocket harness.
- **Protocol claims verified** against `pool-protocol.md`:
  - Endpoints, `max_members` and `acquire_timeout_ms`.
  - Subprotocol auth and the `pk_` 403.
  - `resume` with `last_seq` -1, seq starting at 0, base64 data, and the `gap` event.
  - Exit statuses, `collect_reports`, and the default for `timeout_ms`.
  - Upload once / 409 and the 256 MiB / 2 GiB / 100,000 limits.
  - The 60 s grace, `member_lost`, and an idempotent DELETE.

## What looks good

- A single member and a single exec keep the remote scheduler authoritative and avoid cross-member file sharing.
- The no-retry rule for ambiguous exec submission and session creation is applied consistently.
- The summary is held until a matching pool `exit` before it is emitted, which prevents a fabricated success.
- The workspace snapshot reads bytes from disk, excludes `.git`, and confines symlinks.
- Dynamic bootstrap values are passed as positional arguments, with no shell interpolation.
- The scope is explicit: `--log-dir`, artifacts, durable spools and detached execution are documented as out of scope.
- Every requirement has SHALL/MUST and at least two scenarios, and `openspec validate --strict` passes.
