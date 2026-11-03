# Proposal

## Why

Developers need to check their current work on a prepared remote machine without committing it or managing that machine themselves. Goodflows_web already provides pools, workspace transfer, execution, and session cleanup; the CLI can use these to run an entire workflow on one leased member.

## What Changes

- Add `goodflows run <workflow> --pool <pool>` and equivalent default-workflow execution. Select the project with `--project <slug>`, falling back to `[reporting].project` when present, and authenticate with `GOODFLOWS_URL` and a private `GOODFLOWS_API_KEY`.
- Snapshot the Git working tree, including tracked edits, local deletions, and nonignored untracked files, then upload it through the existing pool workspace endpoint.
- Lease exactly one member and invoke its installed goodflows once for the workflow. All jobs, steps, partitions, dependency scheduling, and intermediate files stay on that member.
- Relay the remote versioned event stream into existing human/JSON output and propagate the remote result. Preserve the current pool reconnect grace, expiration, timeout, cancellation, and reset behavior.
- Document recipe prerequisites, workspace boundaries, environment behavior, and initial limitations. Remote `--log-dir`, artifact retrieval, durable report-spool retrieval, detached execution, distribution across members, SSH, and automatic binary installation are outside this change.

## Capabilities

### New Capabilities

- `pool-workflow-execution`: CLI selection, working-tree snapshot, single-member execution, event delivery, reconnect, cancellation, and release of a complete workflow through goodflows_web pools.

### Modified Capabilities

None. Existing scheduling, partitions, and reporting run inside the remote goodflows process. Local execution contracts remain unchanged.

## Impact

- `src/main.rs`: remote-run argument handling and dispatch before local filesystem-dependent configuration resolution.
- New `src/remote/` modules: Git snapshot/archive creation, REST and Phoenix Channels client, remote bootstrap, event framing, and session lifecycle.
- `src/output.rs`: render remote events and structural remote lifecycle diagnostics without inventing job results.
- `Cargo.toml`/`Cargo.lock`: bounded archive/compression, WebSocket/Phoenix transport, and base64 support as necessary, compatible with Rust 1.88 and native Linux/macOS targets.
- `tests/common/`, new remote integration tests, and `README.md`: mock-server contract tests and usage guidance.
- Integrates with `../goodflows_web/docs/pool-protocol.md` and its existing `pool-sessions`, `pool-execs`, and `sprite-transfers` specs. No server API or disconnect-policy changes are planned.
- Coordinate with `add-settings-env`, `profile-partition-counts`, and `improve-run-report`; forward selections rather than duplicating their resolution logic locally. `improve-run-report`'s rerun hint and wall time come from the relayed summary; remote text mode adds one pool/project line before the summary rather than changing that renderer.
