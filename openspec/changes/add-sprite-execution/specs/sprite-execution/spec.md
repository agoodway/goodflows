# Spec Delta

## Purpose

Runs an existing GoodFlows workflow from a verified working-tree snapshot on its owned Sprite, with durable status and recoverable execution results independent of the controller connection.

## ADDED Requirements

### Requirement: Source reflects the current working tree
Remote runs SHALL snapshot the Git repository containing the selected config, using current tracked file contents and nonignored untracked files rather than cloning a commit. Deleted files SHALL be absent. Git metadata, GoodFlows controller state, and documented credential files SHALL be excluded with a visible exclusion summary. A required config or hook excluded from the snapshot SHALL fail preflight. Non-Git roots SHALL fail with an explicit limitation.

#### Scenario: Dirty source and untracked input
- **WHEN** a tracked file is edited, another is deleted, a new nonignored file exists, and an ignored build output exists
- **THEN** the remote snapshot contains the edit and new file, omits the deleted and ignored files, and does not substitute HEAD contents

#### Scenario: Sensitive file listed by Git
- **WHEN** a tracked `.env` or private credential file is present
- **THEN** the file is excluded with a path/reason diagnostic and no credential content is printed or archived

### Requirement: Source and result paths stay contained
Source staging SHALL reject escapes, unsupported special files, and unresolved or external symlinks. Internal symlinks SHALL only materialize eligible files within the source root. Remote extraction and local result collection SHALL reject absolute paths, traversal, special entries, and symlink/hardlink escape targets. Neither operation SHALL overwrite an existing unrelated destination or follow planted symlinks.

#### Scenario: External source symlink
- **WHEN** a Git-listed path links outside the source root
- **THEN** preflight fails without reading the external target into the snapshot

#### Scenario: Malicious result entry
- **WHEN** a remote result archive contains `../../outside`, an absolute path, or a link that redirects extraction
- **THEN** collection fails without modifying the destination or any external path

### Requirement: Source activation requires complete verification
Each run SHALL receive a fresh source directory. Before hooks or jobs execute, the delivered inventory, SHA-256 hashes, sizes, and executable modes SHALL match the staged manifest. Interrupted or mismatched transfer SHALL leave no runnable partial snapshot and SHALL NOT modify earlier runs. Transport SHALL support source payloads larger than the documented HTTP POST exec stdin limit without selecting that bounded mode.

#### Scenario: Transfer stops midway
- **WHEN** upload disconnects before extraction and verification complete
- **THEN** no hook or job runs against the partial tree and earlier source/results remain unchanged

#### Scenario: Rename and removal between runs
- **WHEN** a later snapshot omits or renames a file from an earlier run
- **THEN** the later source directory has exactly the later manifest, with no leftover earlier file

### Requirement: Remote paths are portable or rejected
Config and all declared workdirs SHALL map relative to the source root into the verified remote tree; outside-root paths SHALL fail preflight. Sprite runs SHALL override ordinary run-logs placement: managed remote log/history/spool paths SHALL appear in saved events, with separate collection-manifest mappings to local bundle paths. Local path rules and saved event/history bytes SHALL remain unchanged. User commands SHALL NOT be rewritten; controller-only paths SHALL be documented as nonportable.

#### Scenario: Profile workdir and config subdirectory
- **WHEN** a config in a repository subdirectory selects a profile whose workdir is another in-repository directory
- **THEN** the remote invocation preserves their relative relationship and executes in the selected workdir

#### Scenario: External workdir
- **WHEN** any declared workdir resolves outside the source root
- **THEN** preflight reports the unsupported mapping before creating or changing the Sprite

### Requirement: Remote workflow execution reuses GoodFlows semantics
The complete selected graph SHALL execute on the Sprite using a compatible matching GoodFlows runner, not per-job transport calls. Workflow/profile selection, prerequisites, partitions, environment precedence, outputs, fail-fast, timeouts, and job cancellation SHALL retain existing meanings. Runtime host measurements SHALL describe the Sprite, not the controller. A missing or incompatible runner SHALL fail before preparation or jobs.

#### Scenario: Selected partitioned job
- **WHEN** a remote run selects a partitioned job with prerequisites and an explicit profile
- **THEN** the remote runner executes the same expanded graph and configured environment as a local run on that supplied runtime

#### Scenario: Elevated platform capabilities
- **WHEN** the Sprite's default shell has permission-bypassing capabilities
- **THEN** workflow jobs run without those elevated capabilities, including inherited bounding/ambient capabilities, or fail before launch if the restriction cannot be established

### Requirement: Run identity and timings are coherent
A remote invocation SHALL have one canonical run UUID shared by its control record, workflow summary, requested history, and reporting spool. Lifecycle status SHALL expose preparation/transfer/collection timing separately from native workflow `execute_ms` and `duration_ms`. Transport overhead SHALL NOT be added to those existing metrics. Controller-captured source provenance SHALL be identified separately from native remote Git context when `.git` is absent.

#### Scenario: Provisioning takes longer than jobs
- **WHEN** creation and setup take 30 seconds and workflow execution takes 2 seconds
- **THEN** native execution metrics describe the workflow interval and lifecycle metrics separately describe creation/setup

#### Scenario: Snapshot lacks Git metadata
- **WHEN** the source archive omits `.git`
- **THEN** remote native history/report source capture retains existing unavailable-source behavior; controller HEAD, branch, and dirty provenance stays only in the control record and is never injected into native history/report source fields

### Requirement: Remote work survives controller disconnection
After durable acceptance, a Sprite-managed service supervisor SHALL own progress independently, renew Tasks API wake holds, and persist phase, process generation, output, and terminal status. Plain attached exec SHALL NOT own execution lifetime. Controller EOF, broken pipes, or connection loss SHALL NOT cancel work, release exclusion, or fabricate a summary. Descendant cleanup and durable terminal results SHALL precede final hold release without depending on reconnect or the release response.

#### Scenario: Disconnect during workflow
- **WHEN** the controller connection is lost after remote acceptance
- **THEN** jobs continue, logs/status remain durable, and recovery can observe completion without executing jobs again

#### Scenario: Launch acknowledgement is lost
- **WHEN** the remote worker accepted a run but the controller did not receive confirmation
- **THEN** the controller uses its private UUID/scope receipt, made durable before first remote submission, to report an unresolved reference and check that UUID; repeated submission only looks up the original record and never relaunches it

#### Scenario: Worker is gone after cold process loss
- **WHEN** recovery proves the accepted supervisor/process generation no longer exists without a terminal workflow result
- **THEN** status records interruption with partial results, does not synthesize success, and does not automatically restart the workflow

#### Scenario: Disconnected completion releases its last hold
- **WHEN** work finishes without a controller connection
- **THEN** descendant cleanup and durable terminal status/results precede final hold release, and later recovery observes that original terminal result without reexecution

#### Scenario: Service restart reports misleading success
- **WHEN** a service command exits zero but its stream reports failure or the expected supervisor is not ready
- **THEN** GoodFlows does not admit new work on that evidence and reports an infrastructure failure or unresolved state until expected supervisor identity, protocol readiness, and reconciliation are confirmed

#### Scenario: Wake hold renewal fails
- **WHEN** the supervisor cannot renew an active run's hold
- **THEN** it blocks new admission, attempts bounded cleanup, preserves saved progress, and does not infer worker death or free the identity merely because the hold expires

### Requirement: Recovery commands do not rerun work
Status SHALL report preparation, active/latest UUID, phase, and terminal state when known. Status/service startup MAY persist `interrupted` after ownership-checked remote reconciliation proves old worker/descendant generations gone; uncertainty SHALL block admission. Logs/collect SHALL read saved results without reconciliation writes. Recovery SHALL NOT transfer source, run hooks, or replay jobs. Unreachable SHALL NOT mean idle. Authorized in-scope controllers SHALL support recovery.

#### Scenario: Recover from a second controller
- **WHEN** the original controller exits and another controller requests the accepted run UUID
- **THEN** status/logs/results are recovered without a new source snapshot or workflow execution

#### Scenario: Incomplete run collection
- **WHEN** collection targets a running or interrupted run
- **THEN** available output is labeled partial and no finalized workflow result is invented

#### Scenario: Status reconciles process loss
- **WHEN** status proves the accepted worker and descendants are gone under ownership, exclusion, and boot/PID/start-tick/generation checks
- **THEN** remote reconciliation durably records interruption before a fresh UUID may acquire the caller identity, retaining partial results without replaying the recorded UUID

### Requirement: Explicit cancellation targets the remote run
`sprite cancel <identity> --run <uuid>` SHALL request cancellation of exactly that run, including a running preparation hook or workflow process group. An attached invocation's explicit interrupt SHALL request remote cancellation; connection loss alone SHALL not. If cancellation acknowledgement is unavailable, the controller SHALL report the unresolved state and preserve exclusivity until reconciliation.

#### Scenario: Cancel a timeout-capable workflow
- **WHEN** an explicit cancel is acknowledged while jobs and their descendants are executing
- **THEN** the existing runner cancellation mechanism terminates them and persists the resulting summary and signal semantics

#### Scenario: Cancel an older run
- **WHEN** cancellation names a finished run while a newer run is active
- **THEN** it does not signal the newer run or its preparation hook

### Requirement: Output protocol distinguishes lifecycle from workflow results
Remote operations SHALL provide text and versioned JSON modes. JSON stdout SHALL contain only versioned events, never raw hook output or transport diagnostics. Existing workflow fields and saved native summaries SHALL remain unchanged. Controller collection/publication diagnostics SHALL use separate lifecycle events before the one final rendered workflow summary, when one exists. Transport failure SHALL NOT fabricate a job failure or terminal summary.

#### Scenario: Setup prints plain output
- **WHEN** a JSON-mode remote run's setup prints text to stdout and stderr
- **THEN** that output is stored and emitted only through structured lifecycle output, not bare JSONL-breaking lines

#### Scenario: Disconnected controller exits
- **WHEN** transport is lost before a terminal result is retrieved
- **THEN** the controller returns nonzero, identifies the remote run for recovery, and emits no fabricated terminal workflow summary

### Requirement: Exit codes distinguish unresolved remote execution
Known workflow outcomes with required collection complete SHALL use existing 0/1/signal-derived codes. Unresolved execution or incomplete required collection SHALL return 3 with a recoverable run reference. Usage errors SHALL remain 2; config/runtime/preparation errors SHALL remain 1. Publication failure SHALL NOT change a known workflow result. Local exit semantics SHALL remain unchanged.

#### Scenario: Cancellation acknowledgement is lost after interrupt
- **WHEN** an attached interrupt requests cancellation but its acknowledgement is lost
- **THEN** exit 3 takes precedence over the local signal-derived code unless terminal state and required collection are subsequently confirmed

### Requirement: Result collection is verified and repeatable
Saved results SHALL include supervisor status, hook logs, workflow events/stderr, requested raw logs/history, and reporting spools when present. Collection SHALL verify inventory and content into private staging before publishing a local bundle. Repeated collection of the same finalized content SHALL be idempotent; conflicts SHALL not overwrite local data. Remote results SHALL remain available until explicit destruction; arbitrary workspace outputs SHALL not be synchronized.

#### Scenario: Interrupted download
- **WHEN** collection disconnects before its bundle verifies
- **THEN** no complete local bundle is published and retry can fetch the original remote result without rerunning work

#### Scenario: Collected paths are remote paths
- **WHEN** the summary names remote log or history paths
- **THEN** original event bytes retain those paths and the collection manifest separately maps artifacts to local bundle-relative paths

### Requirement: Remote reporting collection does not require publisher credentials
Remote jobs SHALL collect/finalize reports without publisher keys. Before replay, the controller SHALL match spool scope and credential names to the trusted invocation, then use only its configured names for lookup; mismatches SHALL fail without unrelated secret lookups. Attached runs SHALL attempt best-effort publication after collection; explicit collection SHALL NOT publish. Delivery failure SHALL NOT change a known workflow exit status.

#### Scenario: Failed tests produce reports
- **WHEN** a remote test job fails after emitting timing reports
- **THEN** its reports are retained and can be collected/published while the workflow remains failed

#### Scenario: Controller is absent at completion
- **WHEN** a remote reporting run finishes after disconnect
- **THEN** a finalized spool remains available for later collection and explicit `reports upload` without rerunning tests

#### Scenario: Tampered publisher variable name
- **WHEN** a collected spool names credential variables different from the trusted invocation configuration
- **THEN** automatic controller publication is refused without looking up or sending those unrelated credentials
