# Design

## Context

See `proposal.md` for motivation and the delta specs for behavior. The remote backend described here is implemented in `src/sprite`; `tasks.md` records its verification and `docs/sprites.md` its documented limits.

### Existing execution boundary

- `src/main.rs` resolves config/workflow/profile/jobs, creates one run UUID, initializes logs/history/reporting, calls `runner::execute`, and emits a final summary with a signal-derived exit code.
- `src/config.rs` rejects unknown fields and canonicalizes workdirs for all workflows and profiles during load. Sending its serialized resolved config verbatim would embed controller paths. `--env` already means a named settings/partition profile.
- `src/runner.rs` schedules a complete graph against one workdir. Commands use fresh noninteractive shells/process groups with timeout and cancellation, collect step/job outputs, and measure CPU and sampled process-group RSS. Replacing each command with an RPC would duplicate these semantics.
- `src/output.rs` emits schema-1 JSON events or text, with the workflow summary last. Native `execute_ms` excludes reporting; `duration_ms` includes native post-job reporting, not provisioning.
- `src/reporting` already separates frozen report spools from replay; publisher keys are scrubbed from children and expressions. `reports upload` accepts finalized spools without a checkout. History/source capture uses Git; a tar tree without `.git` has no native Git context.
- `.github/workflows/release.yml` and `scripts/package-release.sh` produce versioned Linux/macOS archives and SHA-256 files. Linux releases target glibc 2.36. A controller's macOS executable cannot run on a Linux Sprite.

### Evidence and its limits

The [dogfood report](https://ampcode.com/artifacts/goodflows-on-a-sprite-dogfood-experiment-93389b17-6746-40de-b689-6b7418fe2060) records two successful unchanged Rust workflows: four jobs, 385 passing tests, about 25.6 seconds each. Comparable warm orb runs averaged 28.65 seconds versus 25.61 seconds remotely; hardware/OS differed, so this is not a controlled speed claim. Setup needed explicit rustup bootstrapping; a permission test required dropping Sprite-default Linux capabilities. Spry provisioned the experiment, but it need not own the product's lifecycle.

The [transfer report](https://ampcode.com/artifacts/sprite-file-transfer-benchmarks-recursive-push-vs-tar-eb7dffd3-65cf-4143-bf5b-40b29f29588f) and [execution thread](https://ampcode.com/threads/T-01a1b078-82b3-7059-8e73-2373e589b9d0) record 5,023 files verified by hash/size/mode. A 14,028,543-byte gzip archive streamed over WebSocket stdin transferred/extracted in 1.727 seconds; the full recursive retry took 558.744 seconds against a previously partial destination. These single-run, different-destination observations justify choosing archive transfer, not an unlimited-transfer or causal-overhead claim. The initial recursive 502 remains unexplained.

The [lifecycle and identity-lock report, revision 6](https://ampcode.com/artifacts/goodflows-sprite-lifecycle-verification-measured-results-ab38d04a-b946-4c49-aa81-5490fe96416f) measured Sprite CLI 2026-11-03 (`4eb1509`) with sprite-env 0.0.4. Plain non-TTY exec stopped about 10 seconds after controller SIGKILL. A service-owned worker completed 90 steps through 30 renewable task-hold upserts during 100 seconds without a new connection. A corrected 25-step run durably finalized before reconnect by writing terminal state before releasing its last hold. A real restart emitted a streamed exit failure despite CLI exit 0; later tests checked supervisor generation/readiness instead of trusting that exit code. The platform failure's root cause is unproven.

The original UUID-only lock admitted different runs with 7.002 seconds of overlap. The separate caller-scoped lock follow-up admitted eight runs once each, rejected seven distinct UUIDs without work records, and saved disjoint acceptance-to-release intervals across preparation, execution after controller death, and finalization. Worker/service-loss tests preserved partial output and reconciled without replay. This establishes bounded mechanism feasibility, not GoodFlows integration or universal exactly-once behavior. Actual cold wake, cross-scope independence, remaining admission/spawn/finalization crash windows, PID-reuse signalling races, disk/API faults, and escaping descendants remain untested. Service failure may kill worker cgroups; isolated-supervisor-death orphan survival was not established.

## Goals / Non-Goals

**Goals:** Keep one workflow engine; make remote environment ownership explicit; separate controller transport from durable execution; fail closed on uncertain ownership; preserve local behavior and report wire formats.

**Non-Goals:** A generic cloud scheduler/provider framework, Spry compatibility layer, per-job remote placement, automatic dependency detection, cross-identity cache sharing, hostile-code isolation within a reused Sprite, arbitrary filesystem synchronization, or exactly-once execution across platform/VM loss. Existing retained experiment Sprites are not migration targets.

## Decisions

### 1. GoodFlows owns the lifecycle; Sprite CLI is the transport

Add a focused `src/sprite` module for scope, transport, snapshots, remote supervision, and recovery. Keep `runner::execute` local to whichever machine executes the graph. Extract only the invocation/finalization boundary from `main.rs` needed to supply a preallocated run identity and managed paths; do not build a second scheduler or a general backend trait hierarchy.

Use installed `sprite` commands with explicit organization and Sprite arguments, non-TTY WebSocket stdin, and `--no-port-forward`. Never call `sprite use`, change global/local active context, expose public URLs, or auto-forward job ports. Resolve organization once from the Sprite CLI's authenticated context and persist it in the local invocation reference; subsequent calls use that resolved organization. Do not assume a token belongs to this repository. Tokens embed their organization and the API lists that organization's Sprites even when `-o`/`SPRITE_ORG` names another, so do not trust the requested organization: before any receipt, create, exec, read, or destroy, verify that `sprite list --json` reports it as the top-level organization and on every listed Sprite, and fail closed when either is missing or differs. Missing tools/authentication fail remote preflight without exposing values.

Alternative: invoke Spry recipes. Rejected because GoodFlows would then split ownership of preparation, locks, status, and recovery across two CLIs. Sprite CLI remains the controller transport; the in-Sprite supervisor uses the local Tasks API for wake holds. Plain attached exec is not the execution owner: the measured disconnect cutoff cannot satisfy the contract. Fail closed if the installed service/hold mechanism cannot satisfy the integrated acceptance gates.

### 2. Explicit configuration and CLI surface

Proposed configuration (illustrative commands, not script discovery):

```toml
[settings]
sprite_prefix = "goodflows"

[sprite]
setup = "bash scripts/sprite-setup.sh"
resume = "bash scripts/sprite-resume.sh"
pass_env = []
```

`sprite_prefix` is project-wide, not a workflow/profile override. Validate it as 1–20 lowercase ASCII letters/digits/hyphens, starting with a letter or digit. `[sprite]` is also top-level only. `setup`/`resume` are required when running remotely; use explicit `true` for a no-op. Partial configurations remain inspectable, but remote run preflight requires all runtime fields. Malformed configured values fail ordinary config validation, including in unused scopes. `validate`/`list` remain offline and do not require allowlisted values to exist.

Command surface:

```text
goodflows run [workflow] --sprite <identity> [existing run flags]
goodflows sprite status <identity> [--json]
goodflows sprite logs <identity> --run <uuid> [--json]
goodflows sprite collect <identity> --run <uuid> [--output <directory>] [--json]
goodflows sprite cancel <identity> --run <uuid> [--json]
goodflows sprite destroy <identity> [--yes]
```

Recovery commands accept config selection for the project prefix but do not require current source, preparation commands, or run-only environment values. Reject unrelated global execution flags rather than silently applying them. `--sprite` belongs to `run`; leaving it out preserves the existing local default command behavior. No implicit remote default is added.

### 3. Stable names and verified ownership

Treat identity as opaque, case-sensitive caller data, not a Sprite name or shell fragment. Accept 1–256 UTF-8 bytes without control characters. Compute a lowercase hexadecimal SHA-256 digest over a versioned, length-delimited prefix/identity tuple, using 32 hex characters in `<prefix>-<digest>`. This yields names at most 53 bytes without lossy punctuation/case normalization. Store full prefix, exact identity, full digest, ownership schema, and stable Sprite identifier in private remote metadata. Verify exact metadata on every operation; a truncated-hash collision fails rather than reuses another identity. Confirm generated names against current Sprite constraints in the initial transport gate.

The organization plus configured prefix is the project namespace: projects must choose distinct prefixes if they must not share identities. Do not infer project identity from a Git remote, machine path, branch, or reporting project. Do not claim ownership is an authorization boundary against other organization administrators or same-user malicious jobs.

First creation must distinguish its successful create from a name-conflict response. Only the successful creator may initialize an unowned Sprite. A racing loser waits only for bounded ownership initialization, then verifies or fails; it never claims a preexisting unmarked Sprite. A crash between creation and ownership initialization leaves an unowned resource for explicit manual reconciliation, not automatic adoption.

Alternative: readable slug of the identity. Rejected because punctuation/case normalization can merge callers and thread IDs can exceed name limits.

### 4. Remote lock and durable supervisor own the run

Use one private remote control root, proposed `/home/sprite/.goodflows/`, with `owner.json`, a process-held advisory lock, persistent state directory, versioned runners, and `runs/<uuid>/`. Directories are 0700; state/log/input files are 0600; code/executable modes are deliberately preserved without setuid/setgid bits. Refuse symlinked control components.

After local validation/snapshot staging, the service-owned remote supervisor acquires a nonblocking process-held lock keyed by the organization/project-prefix/caller identity, not the run UUID. Admission serializes lock acquisition and durable active-owner publication before acceptance; independent UUID records prevent duplicate submission/replay. Hold the caller lock through source activation, setup/resume, graph execution, descendant cleanup, and durable terminal status/result-manifest writes. The worker inherits the lock descriptor so supervisor loss alone cannot free a live worker. The remote processes, not the controller, own exclusion. Mutating commands, including cancel/reconciliation/destroy, coordinate against the same ownership record; snapshot reads do not take over live work.

Before jobs there is a bounded upload phase. Incomplete transfer leaves a non-runnable staging directory and a transfer-failed/interrupted record; a timeout can abandon only that phase, never steal a possibly executing workflow. The upload path is UUID-bound and receiver commands cannot modify another run. Acceptance of a repeated request with the same UUID is a lookup, not a second launch.

A durable active record remains after process death. An unlocked OS lock alone is insufficient to declare the environment free: reconcile against boot ID, PID/start ticks, execution-generation token, session ID, and surviving worker/descendant groups. Register worker identity durably before a GO handshake permits user work; an unresolved spawn/registration window blocks new admission. Service startup reconciles before declaring readiness. `sprite status` may request that same serialized remote reconciliation and persist `interrupted` only after ownership checks, exclusion, and proof the old execution and descendants are gone. Logs/collect only read saved snapshots. A later run must pass the same gate. None of these paths transfers source, runs hooks, or replays jobs as part of reconciliation. No heartbeat expiry, forced lock deletion, automatic rerun, or stale-PID signal is safe.

Run the supervisor as a Sprite-managed service (`sprite-env services`), not a shell background job or detached local CLI. The service creates/renews a per-run Tasks API hold over `/.sprite/api.sock` while accepted work is active; the measured recipe used `PUT /v1/tasks/<run-key>` with `{"expire":"12s"}` about every 3 seconds. These are tested probe values, not a universal production timeout. A hold prevents sleep; it does not supervise processes. Renewal failure blocks new work and triggers bounded cleanup before expiry where possible; uncertainty never frees the identity. The controller may disappear without affecting renewal. Stop descendants, flush/fsync output and result files, and atomically persist/fsync terminal metadata and result manifests before deleting the final hold. No required final writes may depend on the DELETE response or a reconnect. Retain exclusion until finalization is durable and old work is gone; reconcile any interrupted hold/pointer/unlock cleanup on startup. Expiry or hold release alone is not proof of worker death.

Service create/start/restart must inspect streamed errors and verify protocol readiness plus the expected supervisor generation; CLI exit 0 alone is insufficient. A restarted service reconciles saved nonterminal records and never relaunches them. A supported actual cold-wake test with a changed boot identity remains required before claiming cold-wake support; ordinary service restarts do not satisfy it. Platform/VM loss is an interrupted outcome, never an instruction to restart the workflow.

### 5. Stage the current tree, transfer one archive, activate only after verification

Find the Git root containing the selected config. Enumerate `git ls-files -z --cached --others --exclude-standard`, deduplicate, and read current filesystem contents, skipping deleted paths. Capture a private immutable local staging copy and derive hashes from those bytes, not from a second read of a changing worktree. A torn/change-during-copy detection fails preflight; this is not a filesystem-wide atomic snapshot. Record HEAD/dirty information as controller provenance and use the file manifest as the authoritative snapshot description.

Initial exclusions: `.git`, `.amp`, `.sprite` controller context, managed `.goodflows` state, `.env`/`.env.*`, conventional credential/key files (`.netrc`, `.npmrc`, `.pypirc`, `.git-credentials`, `credentials*`, `secrets.*`, private-key extensions/names), and `.ssh`/`.aws`/`.gnupg`. Also reject actual known controller/forwarded credential bytes and recognized private-key contents rather than archive them. Show omitted paths/reasons without contents. This is a conservative heuristic, not a secret-scanning guarantee; synthetic key fixtures may be excluded too. Git ignore rules, not broad language-directory guessing, determine ordinary build/cache exclusions. Tracked noncredential files remain eligible even when normally generated.

Reject special files, submodule gitlinks, unresolved/cyclic links, and external symlink targets. Materialize internal symlinks only from the already eligible in-root file set, as the benchmark did; never recurse into an arbitrary link target. Detect path collisions. Fail preflight if source/config or a path-based hook command depends on excluded/unavailable files; arbitrary shell commands cannot be fully statically analyzed and fail clearly at hook execution if their own inputs are missing.

Create one gzip tar and stream it with explicit `sprite exec --stdin` over WebSocket, not `--http-post` and not recursive `file push`. The [HTTP POST exec API](https://docs.fly.io/sprites/api/exec/execute-command) documents a 10 MiB stdin cap; filesystem write's separate 16 MiB request limit does not establish a CLI file-push cap. Do not infer unlimited WebSocket support from a successful 14 MB archive.

A private, fresh `runs/<uuid>/source.incoming/` receives only validated regular-file/directory entries, no archive-owned links/special files or restored ownership. Verify exact inventory, SHA-256, size, and ordinary mode against the staged manifest, then rename to `source/` on the same filesystem. Hooks never see an unverified tree. Failure leaves earlier source/caches/results intact. Archive/snapshot scratch is removed locally after transfer; remote partial upload cleanup is scoped to its own inactive run.

### 6. Provision a compatible runner and map config rather than copying host paths

Use the controller executable itself when it is a compatible Linux binary; otherwise fetch the exact controller version's official Linux release archive and checksum, stage/cache it privately, and verify checksum/version/architecture plus the internal remote protocol handshake. Do not use `latest`, assume CPU architecture/glibc, or build the project's workflow just to obtain the runner. An unreleased macOS build without a matching Linux artifact fails clearly. Install atomically under a version/content-addressed runner path, without replacing a running executable. These runner requirements are separate from user dependency setup.

Keep the original source config unchanged for workflow use. Generate a private remote invocation config preserving workflow/profile/job definitions and config-relative relationships, remapping all declared canonical workdirs to the remote snapshot. Because current loading validates all workflows/profiles, map all of them or fail before side effects, not only the selected one. Reject configs/workdirs outside the Git root. Do not rewrite shell command text or env values containing controller paths.

Put requested raw logs, enabled history, and reporting spools under `runs/<uuid>/results/`. Their existing basenames, permissions, contents, opt-in rules, and native retention semantics stay intact. Remote per-run storage isolates these from source replacement; the transport does not prune whole prior run bundles automatically. A configured/CLI path selects that artifact kind remotely, not a write to the same absolute controller path. For Sprite runs this explicitly overrides ordinary run-logs path placement: summary `log_dir` and history paths name remote managed artifacts. A collection manifest maps them to local bundle-relative paths; do not mutate saved event/history bytes. Ordinary local path semantics do not change.

### 7. Explicit preparation, persistent caches, bounded environment

After verified source activation, run exactly one hook in the effective workdir: setup if preparation has not succeeded, otherwise resume. Run `sh -c <configured-command>` with null stdin and separately captured stdout/stderr. Write the preparation marker atomically only after successful setup. A failed resume leaves it intact. Record hook-command digest for diagnosis, but changing text does not silently rerun setup: use resume for migration or explicitly destroy/recreate an idle environment.

Both hooks and jobs receive `GOODFLOWS_SPRITE_STATE_DIR`; reserve it against configured/forwarded overrides and remove ambient inherited copies for ordinary local runs. The directory lives outside fresh source. Hooks can explicitly arrange Cargo/npm/Mix caches or local services. A hook's `export` is not persistent: use configured env, fixed executable paths, or explicit scripts for later commands. Do not assume login-shell PATH mutation survives. The dogfood fixture must bootstrap actual rustup rather than mistake a Sprite wrapper for an installation, and explicitly route build caches into persistent state.

Local preflight resolves all `pass_env` names before create/wake/upload; unset fails, empty succeeds. Reject `SPRITE_TOKEN`, `SPRITES_TOKEN`, and each configured reporting `token_env` in `pass_env` and in every configured settings/workflow/profile/job/step env layer for remote runs. Also forbid `pass_env` entries and configured overrides for runner-owned `GOODFLOWS_RUN_ID`, `GOODFLOWS_REPORT_DIR`, `GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT`, `GOODFLOWS_ENVIRONMENT`, `GOODFLOWS_OUTPUT`, and `GOODFLOWS_SPRITE_STATE_DIR`; preserve existing local reserved-name validation. Reporting `url_env` is not itself denied unless it names a denied variable; it is forwarded only when explicitly allowlisted. Send permitted values as a separate framed stdin payload to the accepted supervisor, never via `--env KEY=value`, shell interpolation, archive, or argv. Keep values in memory; if a transport staging file is unavoidable it is private and removed immediately after reading. An interrupted handoff never starts hooks without a complete payload and never retains a resumable secrets file.

Launch hooks/jobs from an explicit remote base environment (`HOME`, `USER`, `PATH`, locale) plus forwarded values and runner-owned state. Do not inherit the controller environment or broad Sprite service/CLI environment. Forwarded values are the lowest-precedence caller layer for jobs; retain settings → workflow → profile → job → step overrides. Hooks receive exact allowlisted values in the base runtime, not configured job/step env. Children/expression contexts still scrub the configured reporting key. Secrets intentionally given to trusted hooks can be persisted or printed by those hooks; document that this is not sandbox isolation or complete redaction.

Permit explicit privileged installation in trusted setup/resume scripts where the Sprite supports it, but launch workflow jobs with effective/inheritable/ambient/bounding capability sets dropped, verified before execution. Do not let a permission workaround silently skip tests. Restrict the execution process rather than changing unrelated permission tests. A reused Sprite and its persistent services/caches are one trust domain, not clean-room isolation between runs.

### 8. Durable records, protocol, and cancellation

Use versioned atomic control JSON recording scope, run UUID, manifest hash, workflow/profile/job selections, remote runner/protocol version, controller provenance, phase, boot/process/session identity, phase timestamps/durations, preparation outcome, and terminal exit/result references. Never persist environment values. Lifecycle phases are `accepted`, `uploading`, `verifying`, `setup` or `resume`, `running`, `finalizing`, and terminal `completed`, `preparation_failed`, `transfer_failed`, `cancelled`, or `interrupted`. `completed` carries the actual native workflow outcome/exit code; failed jobs are not a transport failure. `unreachable`/`unknown` are controller observations, not proof of a remote terminal state.

Allocate one UUID before first submission and inject it through a private versioned invocation protocol into the existing run identity path. A same-UUID submit can only query the original record. Capture workflow JSONL and stderr to durable files before streaming to the controller. The supervisor must not block on or lose disk output because a follower disappears. `logs` reads persisted output; `collect` saves a point-in-time manifest labeled partial unless terminal. Finalized bundle content remains stable for repeat collection; later report-delivery acknowledgements live in the controller's copy.

Emit new schema-1 `sprite_*` lifecycle events separately from native workflow events. They carry run identity, phase, sanitized diagnostics, and phase timing; hook output is structured data, never bare stdout in JSON mode. The remote native summary is retained verbatim. For an attached completed run, hold the summary for final rendering until controller-side collection/publication diagnostics have been emitted; summary stays last. Do not add network/setup time to native duration metrics. In text mode render the final summary using original controller config spelling and Sprite identity for rerun hints.

Use exit 0/1/native signal-derived codes when a terminal workflow result and required collection are known; use transport/infrastructure exit 3 for unresolved execution or incomplete required collection. Lost cancellation acknowledgement returns 3 even after an attached interrupt, unless terminal state and required collection have subsequently been confirmed. Clap usage errors remain 2; configuration/runtime/preparation errors remain 1 without fabricated job results. Report publication is best-effort and never changes a known workflow result. A broken local output pipe is a disconnect, not remote cancellation.

An explicit attached interrupt or `sprite cancel --run` sends a UUID-bound cancellation request to the supervisor. It signals the current hook process group or the remote GoodFlows process, allowing the existing runner to cancel child groups and finalize. It does not kill a transport process and assume that cancelled the workflow. If acknowledgement is lost, return the unresolved run reference and keep the lock until remote reconciliation. Never signal by stale PID alone or cancel a later run while handling an older request.

### 9. Collect safely and publish only from the trusted controller

Default local result bundle: `<invocation directory>/.goodflows/sprites/<scope-digest>/<run-uuid>/`; `collect --output` selects a different bundle directory. Durably save a private local invocation receipt containing UUID/scope before the first remote submission. A repeated UUID only looks up its saved record, never relaunches work. Recovery from another controller uses the project's prefix plus remote ownership/run record, not a local lock or the original checkout snapshot.

Collect only known result classes, through a manifest of regular files with lengths/hashes and explicit incomplete/complete state. Validate all remote names, entry types, containment, file counts/aggregate sizes against documented finite collection limits before allocation/extraction. Extract to a private sibling staging directory using no-follow paths, verify, then publish without replacement. Repeated identical finalized collection succeeds; conflicting content fails. Partial collections use separate snapshots so a later complete collection does not overwrite a published partial bundle silently. Do not copy arbitrary job workdir output or link targets.

The remote runner receives no publisher key; it collects/finalizes existing report snapshots with publication unavailable. The attached controller verifies invocation UUID/project/workflow/profile/job/partition scope and configured credential names before best-effort replay with its own credentials, retaining existing replay bounds and status semantics. Resolve credentials only by the trusted invocation's configured names after matching the spool; differing manifest names must never trigger unrelated local lookups. Existing finalized-spool validation is necessary but is not this additional trust binding. Controller HEAD/branch/dirty provenance stays only in the control record, never injected into native remote report/history source fields; those retain existing no-Git behavior. Preserve original remote events/history; controller collection/publication events describe controller activity separately. Recovery `collect` only retrieves; later `reports upload <collected-spool>` explicitly publishes on the controller under its existing trusted-invocation rules.

### 10. Retain until explicit destructive cleanup

Never destroy after success/failure, controller loss, or cancellation. `destroy` verifies full ownership and inactive/reconciled state, coordinates against the identity lock, and requires an interactive confirmation or explicit `--yes` for automation. A run concurrent with confirmed destruction must fail rather than recreate state under an in-progress delete. Check resource identity again immediately before delete. Lost delete acknowledgements are reconciled, not blindly retried against a newly created Sprite of the same name. Warn that dependency state and uncollected runs are permanently removed.

## Risks / Trade-offs

- [Service/hold behavior differs by runtime or failure window] → Reproduce measured controller-kill, renewal, terminal-before-release, and readiness checks in the integrated feature. Inject faults around lock/accept metadata, hold creation, spawn/registration/GO, descendant registration, terminal durability/hold/pointer/unlock, and startup reconciliation; test disk/API failures and generation-safe signalling. Keep uncertain state closed. Require supported actual cold-wake evidence separately; do not equate service restart with VM loss.
- [First-create, lock handoff, or stale worker races] → Keep ownership and execution control remote, publish durable acceptance before work, bind operations to UUID and process generation, and test with two controllers and lost acknowledgements.
- [Secret/config leakage] → Empty allowlist by default, missing-name preflight, credential deny rules, separate private input, conservative source exclusions, and sentinel-secret checks across argv/logs/metadata/results. Raw job output remains user-controlled.
- [Growing retained source/results consume disk] → Expose paths/state and explicit destruction; surface storage failures without corrupting earlier results. Automatic remote retention is deliberately deferred, not silently assumed.
- [Environment drift and persistent privileged services] → Explicit resume plus per-identity trust boundary; preparation hooks own migration/readiness. Fresh source and dropped job capabilities are not a claim of pristine-machine isolation.
- [Config paths or repository features are nonportable] → Reject outside-root workdirs/submodules and document absolute command paths and Git-dependent commands as initial limitations. Do not send `.git` or controller credentials to make them work implicitly.
- [Controller binary cannot execute remotely] → Verify platform/version/protocol; use exact checksummed releases where needed, never a similarly named global binary.
- [Timing comparisons mislead] → Preserve native workflow metrics and host information; record lifecycle timing separately; report warm/cold conditions and nonmatched hardware.

## Migration Plan

1. Implement the opt-in surface and protocol behind explicit `--sprite`; leave existing configurations/local execution unchanged. Keep unknown-field validation strict with the new fields intentionally recognized.
2. Translate the bounded service/hold and identity-lock evidence into integrated fault-injection gates before treating remote execution as supported. Require supported cold-wake verification or leave that support gate unresolved explicitly. By owner decision the gate stays unresolved: recovery after an actual cold boot with a changed boot identity is unverified and unsupported, a documented limitation pending future validation; unchanged-boot service restarts, suspend/resume, and checkpoint/restore do not count as cold boot. Use only specifically authorized disposable Sprites, not retained benchmark resources.
3. Dogfood GoodFlows' existing graph with explicit Sprite setup/resume scripts, persistent script-managed caches, both first-use and subsequent runs, and controller recovery. Compare graph/test counts and native metrics to the local run, not only command exit codes.
4. Document installation, prefix ownership, source exclusions/limitations, credential policy, result layout, cold-loss behavior, and explicit cleanup. No automatic migration of Spry recipes or existing Sprites.
5. Roll back adoption by omitting `--sprite`; existing remote data remains recoverable by a compatible CLI. Destruction remains a separate explicit action. Future incompatible remote metadata/protocol versions fail clearly rather than being rewritten automatically.

## Open Questions

- What finite default count/byte budgets best fit collected raw logs and reporting spools? Pick and document them with oversize/partial-bundle acceptance tests; do not weaken existing report-size limits.
- What progress frequency is useful for lifecycle timing/output without excessive noise? This affects presentation only, not lock ownership or liveness.

Service-owned supervision plus renewable holds is the selected mechanism, supported by bounded live evidence rather than an attached-exec lifetime assumption. Integrated crash-window gates and the owner-accepted, unverified cold-wake limitation remain fail-closed, not permission to weaken recovery. User decisions on identity, prefix, exclusive runs, hooks, source inclusion, caches, and allowlisted environment are settled by this proposal.
