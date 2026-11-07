# sprite-environments Specification

## Purpose

Provides reusable, project-scoped Sprite environments whose ownership, preparation, credentials, and exclusive use are controlled explicitly by GoodFlows callers.

## Requirements

### Requirement: Remote execution requires an explicit caller identity
`goodflows run [workflow] --sprite <identity>` SHALL select remote execution. The identity MUST be nonempty and caller-supplied, never inferred or generated. Runs without `--sprite` SHALL retain local behavior. `--env` SHALL remain a workflow profile, independent of execution location.

#### Scenario: No identity argument
- **WHEN** `run --sprite` has no value
- **THEN** argument parsing fails without contacting Sprite infrastructure

#### Scenario: Local run with Sprite configuration
- **WHEN** a valid configuration contains Sprite settings but the caller omits `--sprite`
- **THEN** the existing local workflow runs without invoking Sprite operations or preparation hooks

#### Scenario: Explicit profile on a remote run
- **WHEN** `run ci --sprite T-example --env test` is invoked
- **THEN** `T-example` selects the environment identity and `test` selects only the configured workflow profile

### Requirement: Project prefix scopes a stable owned environment
Remote operations SHALL require project-wide `[settings] sprite_prefix`. Within the selected Sprite organization, prefix and exact identity SHALL deterministically select one Sprite. Workflow/profile overrides of the prefix SHALL be rejected. Naming MUST NOT silently merge different identities. Reuse and destruction SHALL require matching GoodFlows ownership metadata, not merely a matching name.

#### Scenario: Reuse across workflows and controllers
- **WHEN** two controllers use the same organization, project prefix, and identity for different workflows at different times
- **THEN** both address the same owned environment

#### Scenario: Different project prefixes
- **WHEN** two projects use different prefixes and the same caller identity
- **THEN** their selected Sprite names differ

#### Scenario: Unowned name collision
- **WHEN** a Sprite has the computed name but lacks matching ownership metadata
- **THEN** the operation fails without adopting it, running hooks, replacing source, or destroying it

#### Scenario: Platform-reported organization is missing or differs
- **WHEN** a run, `sprite status`, `logs`, `collect`, `cancel`, or `destroy` finds the platform-reported organization, or any listed Sprite's organization, missing or different from the requested organization
- **THEN** the command fails before saving a receipt or creating, executing in, reading from, or destroying any Sprite

#### Scenario: Invalid or missing configuration
- **WHEN** remote execution lacks a prefix or a workflow/profile attempts to override it
- **THEN** preflight fails before creating a Sprite

### Requirement: Same identity cannot run concurrently
An identity SHALL allow one mutating run across controllers, workflows, and profiles. A remote process-held caller-identity lock, separate from UUID idempotency, SHALL cover source delivery through preparation, execution, descendant cleanup, and durable finalization. Contenders SHALL fail promptly with the active UUID, without queuing or work records. Disconnect SHALL NOT release ownership; ambiguity SHALL fail closed. Local locks or expiring client heartbeats SHALL NOT establish exclusivity.

#### Scenario: Two controllers race on first use
- **WHEN** two controllers concurrently run the same previously unused identity
- **THEN** at most one starts preparation or workflow execution and the other reports a conflict without modifying that run

#### Scenario: Controller disappears during a run
- **WHEN** the controlling process exits unexpectedly while its remote worker remains alive
- **THEN** another run of the same identity is rejected and recovery reads remain available

#### Scenario: Independent identities
- **WHEN** different identities run under the same project prefix
- **THEN** their exclusivity controls do not serialize them

#### Scenario: Different UUIDs contend through finalization
- **WHEN** an accepted run holds one caller identity and another controller submits a different UUID for that identity during source delivery, preparation, execution, or finalization
- **THEN** the contender is rejected with the accepted UUID, creates no work record, and leaves the accepted run unchanged

#### Scenario: Fresh run after confirmed termination
- **WHEN** a prior run is durably finalized or reconciled as interrupted with its worker and descendants confirmed gone
- **THEN** a fresh UUID may acquire that caller identity, but submitting the recorded UUID never reexecutes it

### Requirement: Preparation uses only configured commands
`[sprite] setup` and `resume` SHALL be explicit nonempty shell commands, required for remote runs; deliberate no-ops SHALL be expressible as `true`. GoodFlows SHALL NOT discover scripts or infer toolchains. Hooks SHALL run noninteractively against the verified current source before jobs, with an absolute persistent `GOODFLOWS_SPRITE_STATE_DIR`. Hook logs and failures SHALL remain recoverable.

#### Scenario: No discovered setup
- **WHEN** `.agents/setup` exists but `[sprite] setup` is missing
- **THEN** remote preflight fails rather than executing the discovered file

#### Scenario: Hook environment is not a shell session
- **WHEN** setup exports a variable without persisting configuration or using configured workflow env
- **THEN** subsequent workflow commands do not inherit that hook-local export

### Requirement: Setup and resume have distinct lifecycle semantics
An unprepared environment SHALL run setup and not resume. Only successful setup SHALL mark it prepared. A prepared environment SHALL run resume and not setup on subsequent invocations. Either hook's failure SHALL prevent jobs for that invocation. Failed setup SHALL be retried next time; failed resume SHALL NOT clear preparation. Changed hook text SHALL NOT silently reclassify a prepared environment.

#### Scenario: First and second runs
- **WHEN** the first invocation succeeds at setup and a second invocation later starts
- **THEN** the first runs setup only and the second runs resume only

#### Scenario: Failed initial setup
- **WHEN** setup exits nonzero
- **THEN** no workflow job starts, no preparation success is recorded, and the next invocation attempts setup again

#### Scenario: Failed resume
- **WHEN** resume exits nonzero on a prepared environment
- **THEN** no workflow job starts and the next invocation still attempts resume, not setup

#### Scenario: Uncertain setup completion
- **WHEN** setup's process is lost before durable preparation success is recorded
- **THEN** recovery does not infer success and a later authorized run treats the environment as unprepared

### Requirement: Scripts own dependency caches
GoodFlows SHALL retain identity-specific disk state between runs and expose its stable state directory to hooks and jobs. Each run SHALL have a fresh source directory. Setup/resume commands SHALL own installation, cache locations, service readiness, and linking caches into fresh source. GoodFlows SHALL NOT automatically discover, mount, or copy language-specific caches.

#### Scenario: Fresh source with persistent cache
- **WHEN** setup creates a cache in the state directory and resume links it into a later run's source
- **THEN** the cache survives while source files from an earlier run are not reused implicitly

### Requirement: Environment forwarding is explicit and preflighted
`[sprite] pass_env` SHALL default to an empty list of valid, unique names. Only listed caller values SHALL join documented runtime defaults and runner-owned variables. Unset listed variables SHALL fail local preflight before remote side effects; empty values SHALL count as set. Hooks SHALL receive exact permitted values. For jobs, forwarding SHALL be the lowest-precedence caller layer beneath existing settings → workflow → profile → job → step overrides.

#### Scenario: Empty allowlist
- **WHEN** the caller has `PRIVATE_TOKEN` but `pass_env` is omitted
- **THEN** neither hooks nor jobs receive that caller value

#### Scenario: Missing allowlisted variable
- **WHEN** `pass_env` lists an unset variable
- **THEN** the command fails naming only the variable before creating or mutating any Sprite

#### Scenario: Empty and multiline values
- **WHEN** allowlisted values include an empty string, newlines, quotes, and shell metacharacters
- **THEN** hooks and jobs without configured overrides receive the exact values as data without shell evaluation

#### Scenario: Configured environment overrides forwarding
- **WHEN** `pass_env` forwards `MODE=caller` and settings configure `MODE=configured`
- **THEN** the hook receives `caller` and jobs receive `configured` unless a later configured layer overrides it

### Requirement: Controller credentials stay on the controller
Sprite authentication and publisher tokens SHALL stay on the controller. Remote preflight SHALL deny their names and runner-owned overrides in forwarding/configured env. Forwarded values MUST NOT enter archives, argv, metadata, or GoodFlows diagnostics. Transient input SHALL be private and removed after delivery. Raw user output is not redacted. Local validation SHALL remain unchanged except for reserving the state-directory variable.

#### Scenario: Allowlisted controller credential
- **WHEN** `pass_env` names `SPRITE_TOKEN`, `SPRITES_TOKEN`, or a configured reporting token variable
- **THEN** validation fails without showing its value or contacting the Sprite

#### Scenario: Configured credential or reserved override
- **WHEN** a remote invocation configures a denied credential or runner-owned variable in any env layer, including an unselected workflow/profile
- **THEN** preflight fails before remote side effects and reports only the variable name

#### Scenario: Denied names and publisher endpoint policy
- **WHEN** validating forwarding and configured settings/workflow/profile/job/step env for a remote invocation
- **THEN** denied credentials include `SPRITE_TOKEN`, `SPRITES_TOKEN`, and every configured reporting `token_env`; reserved runner names include `GOODFLOWS_RUN_ID`, `GOODFLOWS_REPORT_DIR`, `GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT`, `GOODFLOWS_ENVIRONMENT`, `GOODFLOWS_OUTPUT`, and `GOODFLOWS_SPRITE_STATE_DIR`
- **AND** reporting `url_env` is eligible for explicit forwarding only if it does not name a denied variable

#### Scenario: Runtime and diagnostic inspection
- **WHEN** a permitted secret is forwarded and a transport error occurs
- **THEN** GoodFlows diagnostics and durable run metadata contain no secret value and no secret-bearing command line is constructed

### Requirement: Environments are retained and destruction is explicit
Completion, failure, cancellation, and controller disconnect SHALL retain the Sprite and saved results. `goodflows sprite destroy <identity>` SHALL verify scope/ownership and require explicit confirmation. It SHALL refuse active or unresolved runs; callers MUST cancel or reconcile first. Recovery reads SHALL not execute hooks, transfer source, or start a workflow.

#### Scenario: Destroy active environment
- **WHEN** destruction targets an identity with an active or unresolved run
- **THEN** destruction is refused and existing work and results remain intact

#### Scenario: Confirmed owned destruction
- **WHEN** the caller confirms destruction of an owned, inactive identity
- **THEN** only that identity's Sprite is destroyed and the command explains that cached state and uncollected results are lost

### Requirement: Inspection remains offline
`validate` and `list` SHALL validate configured Sprite field shapes and show nonsecret configuration without requiring Sprite credentials, installed transport tools, or connectivity. Runtime allowlist presence, executable provisioning, and resource ownership checks SHALL occur only for relevant remote operations.

#### Scenario: Offline inspection
- **WHEN** `validate` or `list --json` inspects a Sprite-enabled config on a machine without Sprite credentials or a listed runtime variable
- **THEN** inspection succeeds if configuration is valid, contacts no remote system, and reveals names rather than caller values
