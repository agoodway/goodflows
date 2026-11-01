# Inspector Review-Update — upload-test-timings

**Reviewed:** 2026-11-01
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement

## Summary

The change adds opt-in collection of ExUnit timing reports into a persistent per-invocation spool. It also adds bounded, best-effort publishing to the GoodFlows web endpoint and a `goodflows reports upload` replay command. The design was already thorough. The review found one critical gap: the token-scrubbing plan could not work, because child commands inherit the CLI process environment and `${{ env.* }}` expressions could read the token. It also found envelope field names that drifted from the server contract, and several spec scenarios that the design and tasks implied but never stated. All findings have been patched and none remain open.

**Original counts:** Critical: 1 · Warning: 15 · Suggestion: 7
**Patches:** Auto-patched: 15 · User-guided: 0 · Model-recommended: 8 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/upload-test-timings/proposal.md`
- Design: `openspec/changes/upload-test-timings/design.md`
- Tasks: `openspec/changes/upload-test-timings/tasks.md`
- Deltas:
  - `openspec/changes/upload-test-timings/specs/test-timing-publishing/spec.md`
- Canonical specs consulted: none (`openspec/specs/` is empty)
- Other active changes consulted: none in this repo. Companion changes were checked for contract consistency: `../goodflows_web/openspec/changes/archive/2026-11-01-store-test-timings/` and `../goodcases/openspec/changes/publish-test-timings/`.

## Patches applied

15 findings were auto-patched. 0 were patched after user guidance. 8 were patched from model recommendations. 0 were skipped.

### Auto-patched

1. **Token scrubbing could not stop inheritance or expression leaks (Critical)** — `design.md:38`, spec "Credentials remain owned by the publisher", `tasks.md:2.3`
   - The design now requires `Command::env_remove(<token_env>)` after `.envs(env)` in `JobExecution::command`, matching how `GOODFLOWS_OUTPUT` is handled at `src/runner.rs:605-609`.
   - The token must also be removed from the merged env map before expression evaluation, so `${{ env.<token_env> }}` resolves to null.
   - Added a spec scenario for a token inherited from the parent shell. Task 2.3 now tests expressions, outputs and events.
2. **Envelope `suite_id` comes from the wrong source, and `partition_count` was missing** — `design.md:50` → `suite_id` now comes from the suite_started `run_id` (server design.md:44). `report_id`, `partition` and `partition_count` are copied from that event, and `truncated` is set from the collection outcome.
3. **"incomplete" was used where the field is `truncated`** — `spec.md:83,87` → reworded to use `truncated: true`. The server derives incomplete status itself.
4. **409 handling and acknowledgement validation were missing from the spec** — spec "Publishing is bounded…" → added the 200/201 acknowledgement rule and the no-retry rule for 409 and other 4xx, plus "Immutable conflict" and "Mismatched acknowledgement" scenarios.
5. **Several config validation rules had no scenario** — spec "Reporting is explicitly configured" → added duplicate format, missing reporting block, inline token and reserved-variable override scenarios.
6. **Project slug rule not validated** — `design.md:32`, spec requirement, `tasks.md:1.2` → `project` must be 1-64 characters matching `[a-z0-9][a-z0-9_-]*` (server design.md:40), and env names must be valid.
7. **Replay scenarios missing** — spec "Saved invocations can be retried…" → added scenarios for pending reports (nonzero exit), all reports acknowledged (exit 0, no requests), unfinalized manifest (refused) and signal (signal exit code).
8. **No scenario for an executed job that writes no reports** — spec "Completed and failed jobs…" → added a no-reports scenario.
9. **Bounds requirement had a single scenario** — spec "Report processing is bounded…" → added "Malformed earlier record" and "Report exceeds limits" scenarios.
10. **Envelope workflow, environment and status mapping unspecified** — `design.md:50` → workflow is the resolved name (`default` for top-level jobs), environment is null when no profile is selected, and job_status is the runner `Status` in snake_case.
11. **No-retry list read as exhaustive but omitted 415** — `design.md:56` → now reads "3xx and any 4xx other than 429 (including … 415 …) are not retried".
12. **`reports upload` CLI wiring gaps** — `tasks.md:4.1`
    - The command must be registered in the `cancellation()` match. Signals are otherwise wired only for `None`/`Run` (`src/main.rs:118`).
    - It must return `None` from `Cli::workflow()`.
    - It must reject global flags that don't apply, apart from `--json`.
13. **Text mode drops unknown events** — `design.md:68`, `tasks.md:3.4` → added text render arms for each new event, since `output::render` uses `_ => {}` at `src/output.rs:368`.
14. **Tests could reach a real endpoint through an inherited env** — `tasks.md:2.1` → `tests/common/mod.rs` now strips `GOODFLOWS_URL`, `GOODFLOWS_API_KEY`, `GOODFLOWS_RUN_ID` and `GOODFLOWS_REPORT_DIR`.
15. **Spool path resolution and workflow rebuild** — `design.md:32`, `tasks.md:1.1` → `spool_dir` is resolved in `Config::load` against `base`. The reporting block is carried through the `Config` rebuilt in `Config::workflow` (`src/config.rs:307-318`), which would otherwise drop it.

### Model-recommended patches

1. **Invalid origin and 401/403 outcomes undefined** — `design.md:58`, spec scenario "Invalid endpoint origin" → an invalid origin produces `reporting_skipped` with reason `invalid_url`, sends no request and keeps the exit status. 401/403 leave the report as `report_upload_pending`. In replay, both cases exit nonzero.
   - **Chose:** treat an invalid URL as a skip and 401/403 as pending.
   - **Rationale:** This matches the existing missing-credentials behavior and the proposal's rule that publishing never changes check status. Replay stays strict because its job is delivery.
2. **No event for spool failure or collection rejects** — `design.md:42,68`, spec scenario "Spool cannot be created" → spool failure reuses `reporting_skipped` with reason `spool_unavailable`. File rejections and empty directories become reason codes and counts on `reports_collected`.
   - **Chose:** reuse the existing events.
   - **Rationale:** This is the smallest change. It keeps D6's four-event vocabulary instead of adding new event types.
3. **Example `spool_dir` differs from the default** — `design.md:24,32`, `tasks.md:1.3` → kept the `.goodflows/reports` default and marked the example line as an optional override. Added `.gitignore` documentation and a repo `.gitignore` entry.
   - **Chose:** keep the stated default and annotate the override.
   - **Rationale:** The CLI has no default log directory (`src/main.rs:48-50`), so there was no existing location to align with. The GoodCases companion sets the path explicitly, so it is unaffected.
4. **Job directory encoding is lossy** — `design.md:36` → job names now use the reversible encoder `config::workflow_log_component` (generalized), not the truncated `take(60)` stem at `src/runner.rs:293-305`. Encoded reporting job names over 200 bytes are rejected.
   - **Chose:** reuse the existing percent encoder and add a length limit.
   - **Rationale:** The design claimed a reversible encoding already existed. It does, but only for workflow names. The limit keeps directory names under NAME_MAX, because job names have no length limit.
5. **`runner::execute` error and signal paths** — `design.md:46` → if `execute` returns an error, the manifest is marked aborted and nothing is collected. After a signal, snapshots are still collected and frozen locally, no request starts, and reports go to pending with reason `cancelled`.
   - **Chose:** abort on error and run local-only collection on a signal.
   - **Rationale:** Freezing snapshots after a signal keeps the run replayable without breaking the rule that no request starts after a signal. The error path never emits a summary, so collecting there would be undefined.
6. **HTTP client, TLS and MSRV resolution** — `design.md:54`, `tasks.md:2.1` → `reqwest` 0.12 without default features, using rustls on ring, plus `uuid` 1 with v4/serde. The lockfile is generated with `CARGO_RESOLVER_INCOMPATIBLE_RUST_VERSIONS=fallback` and verified with `cargo +1.88.0 check --locked` and the glibc packaging check.
   - **Chose:** rustls on ring with an MSRV-aware lock.
   - **Rationale:** This avoids a libssl dynamic dependency and aws-lc build tooling in the Debian 12 release builder. Edition 2021 does not make the resolver MSRV-aware by default.
7. **Git context directory and bound** — `design.md:50` → the three named git commands run in the resolved `settings.workdir` with a 2-second total timeout. Any failure gives null fields.
   - **Chose:** resolved workdir, 2-second limit.
   - **Rationale:** The workdir is the checkout the tests actually ran in. It can differ from the config directory (`src/config.rs:198-221`).
8. **Scope of reserved env rejection** — `design.md:38`, `tasks.md:1.2` → `validate_env` rejects `GOODFLOWS_RUN_ID` and `GOODFLOWS_REPORT_DIR` in every job and step env. `GOODFLOWS_OUTPUT` keeps its current overwrite behavior.
   - **Chose:** reject the reserved names in all jobs and leave `GOODFLOWS_OUTPUT` alone.
   - **Rationale:** A uniform rule is simpler to explain. Leaving `GOODFLOWS_OUTPUT` alone avoids breaking existing configs.

### Skipped

_None._

## Critical

_None remaining._

## Warning

_None remaining._

## Suggestion

_None remaining._

## Alignment notes

- **Other active changes:** None in this repo. Against the server companion, the envelope field names (`suite_id`, `partition_count`, `truncated`), the 409 semantics, the no-retry codes including 415, and the project slug rule are now consistent. Against the GoodCases companion, `GOODFLOWS_REPORT_DIR` and `GOODFLOWS_RUN_ID` match, and its explicit `.goodflows-logs/reports` spool_dir is a valid override.
- **Canonical specs:** None exist. This change introduces the first capability.
- **Codebase assumptions verified:**
  - Child processes inherit the parent environment (`src/runner.rs:597-609`).
  - Job log names are lossy (`src/runner.rs:293-305`).
  - `workflow_log_component` exists (`src/config.rs:155`).
  - Signals are wired only for `run` (`src/main.rs:118`).
  - Text mode drops unknown events (`src/output.rs:368`).
  - The config path is discarded in main (`src/main.rs:180`).
  - There is no HTTP dependency (`Cargo.toml`), and `.gitignore` has no spool entry.

## What looks good

- Capture and delivery are cleanly separated, with frozen immutable snapshots and per-report acknowledgement. Replay is safe and idempotent.
- Check results and exit codes are strictly isolated from publishing outcomes. The summary stays the last event.
- The security posture is careful: tokens are taken from env names only, origin-only URLs are enforced, redirects are disabled, and diagnostics are sanitized.
- The design defers to the server design as the single authoritative wire contract.
- Tasks are concrete and test-first, and each one names its verification.

## Contract sync — 2026-11-01

The server companion (`../goodflows_web/openspec/changes/archive/2026-11-01-store-test-timings/`) changed the shared wire contract after this review. This change has been updated to match:

1. **Endpoint**
   - Before: `POST /api/v1/test-timing-reports`.
   - Now: `POST /api/v1/projects/<project>/test-reports`, with the configured project as the path slug.
   - Updated in `design.md` D4, the spec requirement "Publishing uses the shared timing contract", and tasks 3.1 and 5.2.
2. **`project` removed from the body**
   - The server rejects a body `project` or `account_id` with a 422. The envelope now uses exactly the D1 top-level keys.
   - A new spec scenario, "Project selected by path", covers this.
3. **The project must already exist**
   - The server returns 404 when the account has no such project. The CLI does not retry it and leaves the report pending with code `project_not_found`.
   - Covered by a new spec scenario, the D1 note that the CLI never creates projects, the migration plan (apply `scope-recipes-by-project` and create the project first), and the README note in task 1.3.
4. **Job-name rule**
   - The server requires job names of at most 64 bytes with no control characters. Reporting jobs now fail validation otherwise.
   - This replaces model-recommended patch #4's 200-byte encoded-length rule, since a 64-byte name encodes to at most 192 bytes.
5. **`job_status` vocabulary** — `unknown` was added as the fallback for any runner result without a mapping.
6. **`report_id` must be unique within a job**
   - The server's identity key no longer includes `suite_id`. Reports in one job that share a `report_id` are now all rejected locally with code `duplicate_report_id`.
   - Model-recommended: rejecting every duplicate avoids guessing which one is authoritative, and avoids a guaranteed 409.
7. **Other D1 rules**
   - The UUID is serialized lowercase and canonical.
   - Byte-length, U+0000 and partition-grammar rules are validated before snapshotting.
   - A commit that is not 40 or 64 hex characters, or a branch over 256 bytes, becomes null.
   - 404 and 406 are terminal. 503 is retried with `Retry-After`.
