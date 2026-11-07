# Spec Delta

## Purpose

Lets a job synchronously invoke another workflow in the same configuration, reusing its job graph while preserving dependency ordering, settings isolation, bounded concurrency, and traceable results.

## ADDED Requirements

### Requirement: Jobs can invoke a workflow
A job SHALL define exactly one execution form: a command, nonempty steps, or a nonempty `workflow` name. A workflow-call job SHALL support `name`, `depends_on`, and `condition`. It MUST reject `timeout_secs`, declared `partitions`, nonempty `env`, `outputs`, or `reports`, and `shell = false`. Omitted or empty collection defaults and the default shell value SHALL remain harmless. Call jobs SHALL publish no outputs.

#### Scenario: Workflow-call job
- **WHEN** job `integration` declares `workflow = "integration-tests"` and depends on `prepare`
- **THEN** validation accepts it and it invokes that workflow only after `prepare` passes

#### Scenario: Ambiguous execution form
- **WHEN** a job declares `workflow` together with a command or nonempty steps
- **THEN** validation rejects the job before any commands execute

#### Scenario: Unsupported call modifiers
- **WHEN** a call declares a timeout, partitions, nonempty environment overrides, outputs, reports, or `shell = false`
- **THEN** validation names the job and unsupported field before any commands execute

#### Scenario: Reading call outputs
- **WHEN** a condition or output mapping references `needs.checks.outputs.value` and `checks` is a call job
- **THEN** validation rejects the reference rather than exposing child job outputs

### Requirement: Calls are validated across the whole document
Every call SHALL target an existing workflow in the same configuration, including the implicit `default` when root jobs exist. Validation MUST reject direct and indirect workflow-call cycles, including a workflow calling itself, and report the involved workflows and call jobs. Validation SHALL include unselected workflows and conditionally disabled calls. Existing workflow-local job dependency validation SHALL remain in effect.

#### Scenario: Unknown target outside selection
- **WHEN** an unselected workflow contains a call to an unknown workflow
- **THEN** validation fails before any selected job executes

#### Scenario: Indirect recursive call
- **WHEN** workflow `a` calls `b` and `b` calls `a`, even behind a false condition
- **THEN** validation rejects the cycle and identifies both workflows and the call jobs

#### Scenario: Self-call
- **WHEN** a job in workflow `ci` declares `workflow = "ci"`
- **THEN** validation rejects that direct cycle, identifies `ci` and the call job, and does not apply a separate nesting-depth limit

#### Scenario: Legacy default as target
- **WHEN** a named workflow calls `default` and root jobs define the default workflow without calling back
- **THEN** the call targets those root jobs

### Requirement: Selection stays local to the caller
Root `--jobs` SHALL select jobs and their local dependency closure only in the root workflow. Each selected, activated call SHALL invoke all jobs in its target workflow. Unselected call jobs MUST NOT start child invocations. List and validate SHALL expose the call target without executing commands, creating run state, or contacting reporting endpoints, and SHALL check profile compatibility throughout the reachable call graph before execution.

#### Scenario: Select a dependent of a call
- **WHEN** `publish` depends on call `checks` and the user selects `--jobs publish`
- **THEN** the selection includes `checks` and its local prerequisites and the child runs its complete graph

#### Scenario: Root filter does not leak
- **WHEN** the root selects a call named `integration` and the target has jobs `setup`, `unit`, and `test`
- **THEN** the child runs all three rather than applying `--jobs integration` to it

#### Scenario: Incompatible selected profile in a child
- **WHEN** the selected profile supplies a partition override incompatible with a job in a reachable child workflow
- **THEN** run, list, and selected validate fail before any root job executes, including when a call condition would be false

### Requirement: Calls are explicit independent executions
Each activated call job SHALL create a distinct child invocation, even when sibling or nested calls target the same workflow. Calls MUST NOT reuse another call's result or a prior invocation's result. File order SHALL NOT create dependencies: jobs without dependency paths to a call SHALL remain eligible to run alongside it. No workflow-level prerequisite field SHALL be introduced.

#### Scenario: Repeat after changing the workspace
- **WHEN** call `before` invokes `checks`, a job changes the workspace after `before`, and call `after` invokes `checks` after that job
- **THEN** `checks` runs twice and the second execution observes the changed workspace

#### Scenario: Independent root job
- **WHEN** a call appears first in the file and another job has no dependency on it
- **THEN** the other job is not gated by the call

### Requirement: Child settings are resolved independently
Each child SHALL resolve shared settings, its own workflow settings, the selected profile, and applicable CLI overrides with the existing per-key merge. An omitted child key inherits the shared value. Caller-only overrides MUST NOT become the child's base. Paths stay relative to the config directory. Job names and `needs` stay local. Child commands keep reserved-variable and publisher-credential isolation. Profile partition maps stay document-global by job name.

#### Scenario: Different working directories and environments
- **WHEN** the caller and child configure different workdirs and environment values
- **THEN** each uses its own values, except keys overridden by the selected profile or applicable CLI flags

#### Scenario: Omitted child setting inherits shared
- **WHEN** shared settings set `parallelism = 4` and `KEEP=shared`, the caller workflow overrides `parallelism = 1`, and the child workflow sets neither
- **THEN** the child uses parallelism 4 and `KEEP=shared`, not the caller's parallelism 1, because the omitted child key inherits shared settings

#### Scenario: Shared partition name
- **WHEN** the selected profile sets `test = 6`, the caller and a reachable child both partition a job named `test`, and another reachable workflow has an unpartitioned job named `test`
- **THEN** both partitioned jobs use count 6, and the unpartitioned reachable job fails selected-profile validation before any root job executes, including when its call condition would be false

#### Scenario: Profile reaches every invocation
- **WHEN** the root selects `--env local` and the child declares a partitioned job overridden by that profile
- **THEN** child commands receive the selected profile identity and the effective partition count

### Requirement: Calls share bounded execution capacity
The root's effective parallelism SHALL cap executing command/step job instances across the entire invocation tree. Each child's effective parallelism SHALL additionally cap such instances within its subtree. A call awaiting child completion MUST NOT occupy a slot. A spawned call-condition shell SHALL occupy one slot only while that shell runs, then release it before child activation. Child partitions SHALL consume slots individually as usual. One RSS sampler SHALL serve the whole tree.

#### Scenario: Single-slot nested execution
- **WHEN** parallelism is 1 and a root call invokes a workflow containing another call and a command job
- **THEN** the nested commands complete without deadlock and at most one command/step job instance executes at once

#### Scenario: Siblings share the root ceiling
- **WHEN** root parallelism is 3, two calls are active, and an independent root command is ready
- **THEN** no more than three command/step job instances execute across the whole tree

#### Scenario: Child-specific lower limit
- **WHEN** root parallelism is 4 and a child has effective parallelism 1
- **THEN** at most one instance executes in that child's subtree while other root work can use remaining slots

#### Scenario: Stepped job stays one slot
- **WHEN** a child job has several steps and a call-condition shell has already finished
- **THEN** the stepped job occupies one slot, the finished call-condition shell occupies none, and RSS peaks are recorded only for the invocation that spawned each process

### Requirement: A call completes with its child outcome
A call SHALL stay active until its child finishes execution and reporting finalization. Its status SHALL be the child's workflow outcome, using the existing failed, timed-out, cancelled, and signal-snapshot rule. A false call condition SHALL skip the call without creating a child. Non-passing calls SHALL block local dependents under existing dependency rules. `duration_ms` SHALL be integer milliseconds of the call's own condition and the child lifecycle, excluding dependency wait.

#### Scenario: Child failure blocks publication
- **WHEN** a child command fails and `publish` depends on the call
- **THEN** the call fails, `publish` is skipped with the dependency reason, and the root exits with failure

#### Scenario: Optional child jobs are skipped
- **WHEN** a child has only passed jobs and jobs skipped by conditions or their dependencies
- **THEN** the child succeeds and the call passes; `blocked_ms` is dependency wait from the owning invocation's execution start and `queued_ms` is subsequent capacity wait, excluding time before that invocation started

#### Scenario: False call condition
- **WHEN** a call's condition evaluates false
- **THEN** the call is skipped, its dependents are skipped as usual, and no child events, logs, history, or reporting session are created

#### Scenario: Child setup fails
- **WHEN** child activation fails before execution, such as failing to create its required log directory
- **THEN** the call fails with contextual diagnostics and the root still produces its normal failure summary

### Requirement: Failure and cancellation policies span active calls
A failure SHALL stop new work within every containing invocation whose effective fail-fast policy is enabled, including descendant work, without cancelling already executing jobs. A child with fail-fast disabled MUST NOT bypass an enabled ancestor policy. With all containing policies disabled, independent work SHALL continue. SIGINT and SIGTERM SHALL cancel active descendants, reap their process groups, prevent new calls, and preserve the root signal exit code.

#### Scenario: Child cannot bypass root fail-fast
- **WHEN** root fail-fast is enabled, a descendant fails, and another child's local fail-fast is disabled
- **THEN** no further jobs start in the root tree, running jobs settle, and pending dependency failures retain the existing dependency reason

#### Scenario: Local fail-fast does not stop independent root work
- **WHEN** root fail-fast is disabled and a child with fail-fast enabled has a failure
- **THEN** that child's queued work stops while independent root work remains eligible

#### Scenario: Signal reaches grandchildren
- **WHEN** SIGTERM arrives while commands in nested child workflows are running
- **THEN** all running descendants are cancelled and reaped, no new call starts, and the root exits 143

#### Scenario: Signal after child jobs passed
- **WHEN** a child's jobs have all passed and the shared signal snapshot is set before that child's outcome is mapped
- **THEN** the child outcome is cancelled, the call is cancelled, and the root keeps the signal exit code

### Requirement: Child executions have observable invocation identities
When the root's selected reachable graph contains calls, all runtime JSON events SHALL carry their owning `invocation_id`, equal to its summary `run_id`. Child events SHALL add `parent_run_id` and `caller_job`. Each child SHALL have its own UUID, summary, logs, and reporting session. Parent results SHALL contain only local jobs, counting each call once, with target `workflow` and, after activation, `child_run_id`.

#### Scenario: Two calls to the same target
- **WHEN** two root jobs invoke the same target concurrently
- **THEN** their child events and call results identify distinct UUIDs and the matching caller jobs

#### Scenario: Separate logs and report collection
- **WHEN** two calls run a target containing partitioned reporting jobs with `--log-dir`
- **THEN** each child uses its own standard workflow/run log directory and reporting identity, with no overwritten logs or cross-collected reports

#### Scenario: No synthetic process log
- **WHEN** a call without a shell condition runs with logs enabled
- **THEN** its child command jobs write their own logs and the call creates no synthetic command stdout/stderr files

#### Scenario: Existing workflow remains unchanged
- **WHEN** a selected graph contains no calls
- **THEN** existing events, summary counts, log layouts, and reporting envelopes retain their existing shapes and meanings

### Requirement: Live text distinguishes concurrent invocations
When calls are present, live job/step lines and streamed output SHALL include the owning invocation's workflow name and a short invocation distinguisher plus the local job or step. The event `workflow` field alone is not a visible label. New labels SHALL preserve existing terminal-control escaping. Runs without calls SHALL retain their existing labels. `list` text SHALL show a call job's target workflow.

#### Scenario: Same-named jobs in repeated calls
- **WHEN** two calls concurrently run the same target containing a job named `test`
- **THEN** each live status line and output chunk names the target workflow, a short invocation distinguisher, and `test`, so the two invocations are distinguishable

#### Scenario: List shows the call target
- **WHEN** the user runs `goodflows list` for a workflow whose job `integration` calls `suite`
- **THEN** the text lists `integration` with target `suite` and runs no command
