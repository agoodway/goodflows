# Spec Delta

## Purpose

Run a developer's current workflow and local working-tree changes on one prepared goodflows_web pool member while retaining interactive output and the pool's existing session lifecycle.

## ADDED Requirements

### Requirement: Pool execution is explicitly selected
The CLI SHALL accept `--pool <slug>` for explicit and implicit runs, with optional `--project <slug>` falling back to the config's reporting project. It MUST reject these flags on other commands and `--project` without `--pool`. Runs without `--pool` SHALL retain existing behavior.

#### Scenario: Named remote workflow
- **WHEN** the user invokes `goodflows run ci --project app --pool rust-ci`
- **THEN** the CLI targets project app's rust-ci pool and workflow ci

#### Scenario: Default remote workflow
- **WHEN** the user invokes `goodflows --pool rust-ci` and reporting config supplies project app
- **THEN** the default workflow is selected by the remote goodflows in that project's pool

#### Scenario: Invalid selection
- **WHEN** the project cannot be determined, a slug is invalid, or remote flags are supplied to list, validate, init, or reports upload
- **THEN** the CLI reports an actionable error before creating a session

#### Scenario: Project without pool
- **WHEN** the user invokes `goodflows run ci --project app` without `--pool`
- **THEN** the CLI reports an actionable error and starts neither a local nor a remote run

### Requirement: Pool credentials are transport credentials
Pool requests SHALL use `GOODFLOWS_URL` and a private `GOODFLOWS_API_KEY`, refusing redirects. `GOODFLOWS_URL` MUST be an origin only, validated with the existing publisher origin rule (HTTPS or loopback HTTP; no userinfo, path, query, or fragment) and TLS root policy. Authentication MUST follow the documented pool REST/socket protocol. The pool key MUST NOT appear in URLs, archives, diagnostics, or job environments; reporting credential forwarding is governed separately below.

#### Scenario: Missing or public key
- **WHEN** the pool key is missing or has the public `pk_` prefix
- **THEN** the invocation fails locally before any pool request, upload, or job execution

#### Scenario: Invalid origin
- **WHEN** `GOODFLOWS_URL` is non-loopback HTTP or contains userinfo, a path, a query, or a fragment
- **THEN** the invocation fails before any pool request

#### Scenario: Socket authentication
- **WHEN** the CLI opens the pool socket
- **THEN** it uses the Phoenix authentication subprotocol and sends no key in the URL

### Requirement: Upload reflects local working files
The CLI SHALL require a Git working tree and upload current tracked files plus nonignored untracked files beneath its root. Tracked edits and executable modes MUST be preserved; locally deleted tracked paths, ignored untracked paths, and Git administrative metadata MUST be absent. The invocation directory and selected config MUST resolve inside the root, and the config MUST belong to the upload.

#### Scenario: Uncommitted work
- **WHEN** a tracked file is edited, another is deleted, a nonignored file is added, and an ignored cache exists
- **THEN** the remote workspace contains the edited bytes and added file, with neither the deleted file nor ignored cache

#### Scenario: Nested config
- **WHEN** execution is invoked from a repository subdirectory with a nested `--config` path inside the upload
- **THEN** remote invocation cwd and config selection preserve their relative relationship

#### Scenario: Excluded config or external path
- **WHEN** the explicit config is ignored, the config resolves outside the root, or invocation is outside a Git working tree
- **THEN** the CLI fails before leasing and explains the unsupported workspace boundary

### Requirement: Snapshot transfer is bounded and confined
Workspace upload SHALL use a frozen tar.gz with bounded memory and temporary storage cleanup. The CLI MUST reject special files, absolute or escaping links, traversal through symlinked ancestors, submodule gitlinks, untracked nested repositories, tracked skip-worktree paths absent from disk, and detected file-read races. Internal relative symlinks MUST be preserved without dereferencing. Default client caps SHALL be 256 MiB compressed, 2 GiB expanded data, and 100,000 entries; server rejection MUST be surfaced.

#### Scenario: Link inside workspace
- **WHEN** an included relative symlink points to a path inside the root
- **THEN** its link value is preserved without copying bytes through the link

#### Scenario: Unsafe entry or submodule
- **WHEN** an archive candidate is an absolute or escaping link, is a special file, is a Git submodule or untracked nested repository, or is a sparse-checkout path missing from disk
- **THEN** the CLI rejects it before leasing and identifies the path

#### Scenario: Archive limit
- **WHEN** snapshot construction exceeds a client size or entry cap
- **THEN** construction stops, temporary storage is removed, and no session is created

### Requirement: One member owns the entire workflow
The CLI SHALL request exactly one member with a 120-second acquisition timeout, join the session channel before uploading, upload once per session, await member readiness within a client deadline of 120 seconds plus 60 seconds per started 512 MiB of expanded payload, and submit one workflow exec. Every selected job, step, condition, dependency, and partition SHALL execute under the member's goodflows scheduler against its shared workspace. The client MUST NOT dispatch individual jobs or automatically restart a submitted workflow.

#### Scenario: Dependent jobs share files
- **WHEN** a compile job creates a file consumed by a dependent partitioned test job
- **THEN** both run on the same member and the dependent instances can read that file

#### Scenario: Member unavailable
- **WHEN** no member arrives within the acquisition deadline
- **THEN** the CLI reports the existing pool timeout and executes no workflow

#### Scenario: Hydration never succeeds
- **WHEN** no member becomes ready within the client readiness deadline after upload
- **THEN** the CLI fails and releases the session instead of waiting indefinitely

### Requirement: The member resolves configuration and tools
The uploaded config SHALL be validated and resolved on the member. The CLI MUST forward workflow, config, environment profile, job selection, explicit parallelism, and fail-fast choices without forwarding remote-selection flags. The member MUST provide the caller's exact goodflows package version and project tools. Missing or mismatched goodflows MUST fail before any workflow job runs, reported through a recognizable bootstrap marker and exit code that the client maps to an `error` naming expected and actual versions; inner argument (2) and configuration (1) exit codes MUST propagate unchanged.

#### Scenario: Member CPU default
- **WHEN** the caller omits parallelism and the config uses the CPU-count default
- **THEN** the remote runner uses the member's CPU count

#### Scenario: Profile and subset
- **WHEN** a remote invocation selects a profile and `--jobs test`
- **THEN** the remote runner applies that profile and executes test with its prerequisite closure, preserving effective partition counts and settings env

#### Scenario: Remote workdir validation
- **WHEN** a workdir is unavailable on the member
- **THEN** remote config validation fails before workflow jobs execute, even if that path exists locally

#### Scenario: Binary version mismatch
- **WHEN** the member's goodflows version differs from the caller or goodflows is missing
- **THEN** the invocation fails with recipe guidance and executes no jobs

### Requirement: Remote environment and reporting remain explicit
Remote commands SHALL inherit the member's environment and config-defined overrides, not the caller's general environment. Only locally present configured reporting endpoint/key variables SHALL be explicitly forwarded to the remote goodflows process when reporting exists, including the pool key when reporting names the same variable (the default). Forwarded values MUST be removed from every job and step command environment. Existing publisher scrubbing and best-effort status semantics MUST remain in force. The client MUST NOT treat inner reports as the outer exec's report manifest.

#### Scenario: Caller-only environment
- **WHEN** the caller has an environment variable absent from the member and the config
- **THEN** it is not automatically available to remote jobs

#### Scenario: Remote reporting
- **WHEN** a reporting workflow has configured publisher credentials locally
- **THEN** its remote publisher receives those named values, jobs and expressions do not receive its token, and reporting events retain their existing workflow and partition identities

#### Scenario: Default reporting credential names
- **WHEN** reporting uses the default `GOODFLOWS_URL`/`GOODFLOWS_API_KEY` names shared with pool authentication
- **THEN** the remote goodflows process receives those values for publishing, and no job, step, or expression can read the key from its environment

#### Scenario: Pool key without reporting
- **WHEN** the workflow has no reporting block
- **THEN** the pool key is not supplied to the workflow exec

### Requirement: Output preserves workflow event semantics
The client SHALL reconstruct versioned JSONL events across remote byte chunks and render them through existing human/JSON output. Job, step, partition, environment, report, and result metadata MUST remain intact. Relayed events MUST be forwarded verbatim without re-tagging their workflow, environment, or schema version. Structural pool progress and bounded remote stderr (64 KiB total) SHALL use `remote_status` with a `phase` field; failures SHALL use `error`. JSON stdout MUST contain only versioned JSONL. Invalid, unsupported, or oversized (over 16 MiB per event) event data MUST fail the invocation explicitly. A stalled output consumer MUST NOT stop socket heartbeats; relay buffering MUST be bounded and its overflow MUST fail the invocation explicitly.

#### Scenario: Fragmented output
- **WHEN** a JSON event or multibyte character spans several pool output frames
- **THEN** it is reconstructed once and retains its original event fields

#### Scenario: Human partition output
- **WHEN** a remote partitioned job emits events in human mode
- **THEN** its output identifies the job and partition using existing rendering

#### Scenario: Corrupt stream
- **WHEN** remote stdout contains invalid JSONL, an unsupported schema, or an event exceeding 16 MiB
- **THEN** the client emits a structural error, returns nonzero, and releases the session

#### Scenario: Stalled output consumer
- **WHEN** local stdout stops draining while the remote workflow keeps producing events
- **THEN** heartbeats continue, and if the bounded relay queue overflows the client emits a structural error, cancels the exec, and releases the session

### Requirement: Success requires a confirmed remote result
The CLI SHALL return the remote workflow's confirmed exit status. It MUST hold the final workflow summary until the pool confirms a matching normal exit, then emit the summary last. Missing summary with exit zero, contradictory status, pool timeout, or transport failure MUST NOT be presented as success. Bootstrap/configuration errors MUST NOT receive fabricated job results.

#### Scenario: Passing or failing checks
- **WHEN** the remote summary reports success with exit 0 or failed checks with exit 1
- **THEN** the client emits that summary last and returns the corresponding code

#### Scenario: Lost runner or outer timeout
- **WHEN** the pool exec ends as error, timed_out, or cancelled without a local signal
- **THEN** the client returns nonzero and identifies the outer execution failure without synthesizing a completed workflow summary

#### Scenario: Inconsistent success
- **WHEN** a successful inner summary is followed by an abnormal pool exit
- **THEN** the client emits an error instead of the held successful summary

#### Scenario: Signal after confirmed result
- **WHEN** SIGINT arrives after a matching pool exit and summary are confirmed but before cleanup finishes
- **THEN** the client stops cleanup, reports unconfirmed cleanup, and returns the confirmed 0/1 code rather than 130

### Requirement: Reconnection preserves execution identity
The client SHALL rejoin and resume the same exec from its last processed sequence after brief disconnects, deduplicating replay, within a 60-second local reconnect window. It MUST respect existing server grace, expiry, and lifetime rules and fail on output gaps. Ambiguous exec submission MUST be reconciled against the joined session; an unidentified exec MUST NOT be resubmitted.

#### Scenario: Temporary disconnect
- **WHEN** the socket reconnects before expiry and buffered output remains available
- **THEN** the client resumes the original exec without duplicate output or a second workflow run

#### Scenario: Replay gap
- **WHEN** the server reports evicted output
- **THEN** the client reports incomplete output, returns nonzero, and releases the session

#### Scenario: Exec reply lost
- **WHEN** an exec push loses its reply and rejoin lists the single running exec
- **THEN** the client resumes that exec rather than sending another execution request

#### Scenario: Exec identity unavailable
- **WHEN** submission is ambiguous and rejoin cannot identify the exec
- **THEN** the client fails and releases the session without automatically retrying the workflow

#### Scenario: Client remains disconnected
- **WHEN** the disconnect exceeds the existing server grace
- **THEN** existing session expiry cancels execution and resets the member; the CLI does not request detached continuation

### Requirement: Every terminal path releases the member
The client SHALL attempt session release, bounded to 30 seconds, after completion, failure, timeout, or interruption. Ctrl+C/SIGTERM received before a confirmed result MUST stop submission, request cancellation when the exec is known, and return 130/143. A second signal during cleanup MUST abandon remaining cleanup and exit immediately with an unconfirmed-cleanup warning. An outer cancellation MUST be followed by release/reset because nested jobs can have separate process groups. Cleanup failure MUST be visible without replacing a confirmed workflow status; existing expiry remains the fallback.

#### Scenario: Normal completion
- **WHEN** the workflow finishes
- **THEN** the client releases the session and the server resets the leased member

#### Scenario: Signal during execution
- **WHEN** SIGINT arrives during the remote workflow
- **THEN** the client requests cancellation and release, returns 130, and never starts another exec on that lease

#### Scenario: Signal while waiting or uploading
- **WHEN** a signal arrives before workflow submission
- **THEN** no workflow exec is submitted, any known session is released, and temporary files are removed

#### Scenario: Cleanup unavailable
- **WHEN** DELETE cannot be confirmed within the shutdown budget
- **THEN** the client reports unconfirmed cleanup and exits, leaving existing session expiry/reset to reclaim the member

#### Scenario: Second signal during cleanup
- **WHEN** a second SIGINT arrives while release is still pending
- **THEN** the client stops waiting, reports that session expiry will reset the member, and exits immediately

### Requirement: Initial remote limitations are explicit
The CLI SHALL reject remote `--log-dir` before leasing. Documentation MUST state recipe/version requirements, Git and upload boundaries, server execution limits, unavailable Git source metadata, loss of remote report spools after reset, direct (non-proxied) pool connectivity, that reported wall time covers member execution only, and that default reporting credential names forward the pool key to the remote goodflows process. This version MUST NOT promise artifact downloads, saved-spool retrieval, or detached execution.

#### Scenario: Raw log destination requested
- **WHEN** `--pool` and `--log-dir` are supplied together
- **THEN** the CLI explains that remote raw-log retrieval is unsupported and creates no session

#### Scenario: Pending report at reset
- **WHEN** the remote publisher leaves a report pending and the member is released
- **THEN** check status remains unchanged, pending status is visible, and documentation makes clear that no local replay spool is retained
