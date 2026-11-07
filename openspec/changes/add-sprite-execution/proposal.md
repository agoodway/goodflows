# Proposal

## Why

GoodFlows can already run its unchanged workflow on a Sprite, but callers must separately provision the machine, deliver source, arrange dependencies, and recover results. Native CLI ownership would make a caller's project-scoped development environment reusable while preserving GoodFlows' existing workflow scheduler and output contracts.

## What Changes

- Add opt-in `goodflows run [workflow] --sprite <identity>`; require the caller's identity and a project-wide `[settings] sprite_prefix`. Runs without this flag remain local; `--env` remains an explicitly selected workflow profile.
- Automatically create or reuse the identity's Sprite, verify ownership, and reject overlapping runs of that identity across controllers. Retain the Sprite after completion; provide explicit, ownership-checked destruction rather than automatic teardown.
- Add explicit `[sprite] setup`, `resume`, and `pass_env` configuration. Run setup only until it succeeds; run resume only on later prepared invocations. Do not discover scripts or infer dependencies. Scripts own persistent dependency/build caches.
- Snapshot the current Git working tree, including tracked modifications and nonignored untracked files, subject to documented credential/metadata exclusions. Transfer a verified archive into a fresh per-run directory rather than updating an old checkout in place.
- Forward no caller environment by default. Fail preflight for unset allowlisted variables; keep Sprite authentication and reporting publisher credentials on the controller.
- Execute the complete selected workflow remotely with the same GoodFlows version and existing scheduling, partition, timeout, history, and reporting-collection semantics. Preserve remote execution measurements separately from provisioning/transfer timings.
- Use a Sprite-managed service supervisor plus renewable Tasks API holds to continue accepted work after controller disconnection. Keep caller-identity exclusion separate from run-UUID idempotency; persist terminal results before releasing the final hold. Add status, logs, collection, and explicit cancellation commands that do not rerun work. Distinguish connection loss from confirmed process loss.
- Preserve Sprite targeting in text rerun hints. Keep existing local CLI behavior and event meanings unchanged.

Not included: Spry integration, per-job distributed scheduling, automatic dependency discovery, generic cache management, arbitrary output-directory synchronization, public networking, checkpoints, or automatic reruns after remote process loss.

## Capabilities

### New Capabilities

- `sprite-environments`: project-scoped caller identities, ownership, exclusive lifecycle control, explicit preparation hooks, environment forwarding, and retained environments.
- `sprite-execution`: safe source delivery, portable remote workflow invocation, durable execution/recovery, result collection, and controller-side report publication.

### Modified Capabilities

- `run-report`: remote rerun hints preserve the original Sprite identity in addition to workflow, config, and profile.

## Impact

- Extend `src/main.rs` CLI dispatch and `src/config.rs` strict parsing/validation; introduce a bounded Sprite transport/lifecycle module rather than a second scheduler.
- Separate reusable invocation/finalization in the existing run path where needed; integrate `src/output.rs`, `src/history.rs`, retention, and `src/reporting/{mod,source,replay}.rs` without changing local defaults or report wire formats.
- Require the installed Sprite CLI for remote operations, local Git for source enumeration, archive support, and a compatible Linux GoodFlows executable remotely. No Spry binary or library is required.
- Add CLI integration tests, live disposable-Sprite acceptance coverage, configuration/usage documentation, and a repeatable GoodFlows dogfood configuration. Live resources and destructive tests require explicit authorization.
- Use the transfer, Rust-workflow, and lifecycle/identity-lock experiments as bounded mechanism evidence, not proof of the GoodFlows integration. Integrated disconnect, contention, cancellation, secret handling, crash-window reconciliation, and supported actual cold-wake tests remain implementation acceptance gates.
