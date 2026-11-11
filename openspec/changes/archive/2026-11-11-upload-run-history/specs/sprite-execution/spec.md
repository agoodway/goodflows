# Spec Delta

## MODIFIED Requirements

### Requirement: Remote reporting collection does not require publisher credentials
Remote jobs SHALL collect/finalize reports without publisher keys. When `[reporting] history = true`, each remote invocation SHALL also freeze its run record in its spool without a publisher key. Before replay, the controller SHALL match spool scope and credential names to the trusted invocation, then use only its configured names for lookup; mismatches SHALL fail without unrelated secret lookups. A spool that lists a run record SHALL be refused for automatic publication unless the trusted invocation configuration sets `[reporting] history = true`. Attached runs SHALL attempt best-effort publication after collection, including run records; explicit collection SHALL NOT publish. Delivery failure SHALL NOT change a known workflow exit status. A remote run record's `source` SHALL be the snapshot provenance from "Run identity and timings are coherent", and the record SHALL carry CI context only from variables forwarded with `[sprite] pass_env`.

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
- **WHEN** a remote run record is built from a source archive without `.git` and the snapshot HEAD is a clean branch
- **THEN** the root and each child record's `source` is that snapshot's `commit`, `dirty` false, and `branch`, and jobs do not see those values

#### Scenario: Archive-derived dirty
- **WHEN** a tracked file's shipped bytes differ from HEAD, or a tracked file is missing
- **THEN** the uploaded run record's `source.dirty` is true

#### Scenario: Untracked-only shipment
- **WHEN** the only difference from HEAD is a nonignored untracked file
- **THEN** that file is in the archive and `source.dirty` is true

#### Scenario: Forwarded CI variables
- **WHEN** `[sprite] pass_env` lists `GITHUB_ACTIONS`, `GITHUB_EVENT_NAME` and `GITHUB_RUN_ID` and the caller sets them
- **THEN** the remote run record's `ci` carries `provider`, `event` and `run_id`, and without forwarding it has no `ci`

### Requirement: Run identity and timings are coherent
A remote invocation SHALL have one canonical root run UUID shared by its control record, root summary, requested history, and reporting spool; called workflows keep their own. Lifecycle status SHALL expose preparation/transfer/collection timing separately from native workflow `execute_ms` and `duration_ms`. Transport overhead SHALL NOT be added to those existing metrics. While the controller builds the source archive it SHALL capture `commit`, `branch`, and `dirty` from that archive and SHALL send them on the submission header's provenance. The remote runner SHALL read that provenance once, before jobs run, and SHALL write the same `source` into the root and every child history file and test-report record. Jobs and hooks SHALL NOT receive those values through `pass_env` or argv. `dirty` SHALL be true when the archive is not the tree of `commit`: a tracked file's shipped bytes differ from HEAD, a tracked file is missing, or a nonignored untracked file is included. Ignored files and `.git` SHALL NOT set `dirty`. `dirty` SHALL be false only when the archive matches HEAD. `branch` SHALL be present only when HEAD is a branch, and SHALL be omitted when git reports none, without reading GitHub Actions environment variables. When HEAD cannot be read, `source` SHALL be omitted. Native git capture on the Sprite finds no `.git` and SHALL NOT override the supplied `source`. The control record SHALL keep the same provenance.

#### Scenario: Provisioning takes longer than jobs
- **WHEN** creation and setup take 30 seconds and workflow execution takes 2 seconds
- **THEN** native execution metrics describe the workflow interval and lifecycle metrics separately describe creation/setup

#### Scenario: Snapshot lacks Git metadata
- **WHEN** the source archive omits `.git` and HEAD is the branch `main` at a clean commit
- **THEN** the root and child history files and test-report records have that commit, `dirty` false, and `branch` `main`, the control record has the same provenance, and no job environment contains those values

#### Scenario: Archive-derived dirty
- **WHEN** a tracked file's shipped bytes differ from HEAD, or a tracked file is missing, and the archive is built from that tree
- **THEN** history and test-report `source.dirty` is true

#### Scenario: Untracked-only shipment
- **WHEN** the only difference from HEAD is a nonignored untracked file, and ignored files are also present
- **THEN** the archive contains the untracked file, omits the ignored files, and `source.dirty` is true

#### Scenario: Detached HEAD
- **WHEN** HEAD is detached and the archive matches that commit
- **THEN** `source` has `commit` and `dirty` false, and `branch` is absent
