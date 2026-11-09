# Spec Delta

## MODIFIED Requirements

### Requirement: Remote reporting collection does not require publisher credentials
Remote jobs SHALL collect/finalize reports without publisher keys. When `[reporting] history = true`, each remote invocation SHALL also freeze its run record in its spool without a publisher key. Before replay, the controller SHALL match spool scope and credential names to the trusted invocation, then use only its configured names for lookup; mismatches SHALL fail without unrelated secret lookups. A spool that lists a run record SHALL be refused for automatic publication unless the trusted invocation configuration sets `[reporting] history = true`. Attached runs SHALL attempt best-effort publication after collection, including run records; explicit collection SHALL NOT publish. Delivery failure SHALL NOT change a known workflow exit status. Remote run records SHALL omit `source` when native capture finds none, and SHALL carry CI context only from variables forwarded with `[sprite] pass_env`.

#### Scenario: Failed tests produce reports
- **WHEN** a remote test job fails after emitting timing reports
- **THEN** its reports are retained and can be collected/published while the workflow remains failed

#### Scenario: Controller is absent at completion
- **WHEN** a remote reporting run finishes after disconnect
- **THEN** a finalized spool remains available for later collection and explicit `reports upload` without rerunning tests

#### Scenario: Tampered publisher variable name
- **WHEN** a collected spool names credential variables different from the trusted invocation configuration
- **THEN** automatic controller publication is refused without looking up or sending those unrelated credentials

#### Scenario: Run record published after collection
- **WHEN** an attached remote run with `[reporting] history = true` and a workflow call completes and its results are collected
- **THEN** the controller sends the child's and the root's run records, each after that spool's test reports, and the workflow exit code is unchanged by their delivery

#### Scenario: Run record without trusted opt-in
- **WHEN** a collected spool lists a run record and the controller's trusted configuration does not set `[reporting] history = true`
- **THEN** automatic publication of that spool is refused and no run record or test report from it is sent

#### Scenario: Remote record provenance
- **WHEN** a remote run record is built from a source archive without `.git`
- **THEN** the record has no `source`, and the controller's commit and branch are not added to it

#### Scenario: Forwarded CI variables
- **WHEN** `[sprite] pass_env` lists `GITHUB_ACTIONS`, `GITHUB_EVENT_NAME` and `GITHUB_RUN_ID` and the caller sets them
- **THEN** the remote run record's `ci` carries `provider`, `event` and `run_id`, and without forwarding it has no `ci`
