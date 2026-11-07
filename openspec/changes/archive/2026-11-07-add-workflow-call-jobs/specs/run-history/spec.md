# Spec Delta

## MODIFIED Requirements

### Requirement: Graph hash ignores environment values
`graph_sha256` SHALL be the lowercase hex SHA-256 of a canonical JSON document of the selected jobs, in configuration order, containing only job name, command, `depends_on`, effective partitions after selected-profile overrides (count and strategy), `timeout_secs`, shell flag, condition, and for each step its id, command, shell flag, and condition. Legacy and table shell conditions SHALL hash identically. It MUST exclude environment values, paths, and runtime output. The same selection SHALL hash the same on every run. A workflow-call job SHALL additionally include the trailing call object specified below. Invocation UUIDs MUST also be excluded. Existing field order SHALL remain unchanged, and selections with no workflow-call jobs SHALL retain their previous canonical bytes and hash.

#### Scenario: Env does not change the hash
- **WHEN** two runs select the same jobs and differ only in settings env values
- **THEN** their `graph_sha256` values are equal

#### Scenario: Command change
- **WHEN** a selected job's command changes
- **THEN** `graph_sha256` changes

#### Scenario: Step condition change
- **WHEN** a selected job's step condition changes
- **THEN** `graph_sha256` changes

#### Scenario: Called profile partition change
- **WHEN** the selected profile changes a partition count in a reachable called workflow
- **THEN** the root graph hash changes

#### Scenario: Unreachable workflow change
- **WHEN** a command changes only in a workflow not reachable from the selected root jobs
- **THEN** the root graph hash does not change

#### Scenario: No-call compatibility
- **WHEN** an existing command/step-only selection runs with the new binary
- **THEN** its hash is identical to that produced from its previous canonical document, including jobs whose command is null and whose steps are empty

## ADDED Requirements

### Requirement: Graph hash includes the called workflow
A workflow-call job SHALL keep the existing canonical fields and order and add one trailing object, omitted for ordinary jobs, after them. That object contains the target name and `graph_sha256`, the target's own graph hash under the same selected profile. Each reachable workflow SHALL be hashed once per invocation and reused for every call path. A false call condition SHALL still include the target. Root filtering determines reachability.

#### Scenario: Called command change
- **WHEN** a command in a reachable called workflow changes, including a grandchild workflow
- **THEN** the root graph hash changes even though the root call job's own command is unchanged

#### Scenario: False call condition still fingerprints the target
- **WHEN** a selected call's condition is false and a command in its target workflow changes
- **THEN** the root graph hash changes, and no child invocation is created

#### Scenario: Repeated nested calls
- **WHEN** each of 40 nesting levels calls the next level twice, behind a false root call condition
- **THEN** the root graph hash is computed without expanding every call path, and a command change in the deepest workflow still changes it

### Requirement: History links workflow-call invocations
A child's history record SHALL add `parent_run_id` and `caller_job`. Each activated call's parent instance record SHALL add its target `workflow` and `child_run_id`. Other instance records SHALL retain their existing field allowlist. These additions keep schema version 1. Each invocation SHALL independently obey effective history and retention settings. Linkage SHALL contain identities only, not resolved paths, output, or environment values.

#### Scenario: Parent and child history enabled
- **WHEN** a call starts a child and both invocations have history enabled
- **THEN** distinct immutable history files link the call result to the child and the child to its parent and caller job

#### Scenario: Parent history disabled
- **WHEN** the parent disables history but the child's own workflow enables it without a CLI history override
- **THEN** only the child writes history, retaining parent identity without a parent file, and that record omits `requested_jobs` rather than copying the root `--jobs` list

#### Scenario: CLI disables all history
- **WHEN** the root passes `--no-history`
- **THEN** neither the root nor any child writes history

### Requirement: Workflow-call cost is not synthetic process cost
A call's `duration_ms` SHALL be an integer millisecond count of its own condition evaluation and child lifecycle, excluding dependency wait. Its command-cost entries SHALL describe only its own spawned shell condition, if any, never copied child command costs. A run's `max_in_flight` SHALL count executing command/step job instances and in-flight call-condition shells in its subtree, excluding waiting call wrappers. Child command costs SHALL remain in child summaries and history.

#### Scenario: Command-free wrapper
- **WHEN** a call without a shell condition runs a child with two commands
- **THEN** its instance history has integer `duration_ms` and no `commands` field, its wait metrics are relative to its owning invocation, and child records contain the actual command measurements

#### Scenario: Single-slot call
- **WHEN** root parallelism is 1 and a call runs a child command
- **THEN** the root's `max_in_flight` is 1, not 2

### Requirement: Child wait metrics use the owning invocation clock
Each invocation SHALL measure `blocked_ms` from its own execution start until a job's dependencies pass, or until an earlier skip or cancellation. `queued_ms` SHALL measure only subsequent capacity wait until launch, skip, or cancellation. Child metrics MUST exclude time before child execution starts, including parent preparation and call-condition evaluation. Parent call wait metrics SHALL remain relative to the parent invocation.

#### Scenario: Ready child follows parent preparation
- **WHEN** a parent performs preparation before activating a child whose first job has no dependencies
- **THEN** that child's first job has `blocked_ms` near zero, excluding the parent preparation, and `queued_ms` includes only capacity wait after child execution starts

#### Scenario: Child dependencies and capacity both delay work
- **WHEN** a child job waits for its local prerequisite and then waits for capacity shared with another invocation
- **THEN** its `blocked_ms` covers only local dependency wait from child execution start and its `queued_ms` covers only the later capacity wait, without counting either delay twice
