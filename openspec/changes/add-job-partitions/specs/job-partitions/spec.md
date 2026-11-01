# Spec Delta

## Purpose

Lets a job declare a partition count so Goodflows runs it as several scheduled instances, each told its partition identity, instead of a tool fanning out inside one job.

## ADDED Requirements

### Requirement: Partition count is declared per job
A job SHALL accept an optional `partitions` setting, either an integer or a table `{ count = N, strategy = "index" }`, with `strategy` defaulting to `index`. The count MUST be an integer from 1 to 64. Any other strategy, any other table key, or an out-of-range count MUST fail validation before jobs execute. Jobs without `partitions` SHALL keep their existing behavior.

#### Scenario: Integer shorthand
- **WHEN** a job declares `partitions = 3`
- **THEN** validation succeeds and the job is treated as three partitions using the index strategy

#### Scenario: Table form
- **WHEN** a job declares `partitions = { count = 2, strategy = "index" }`
- **THEN** validation succeeds and the job is treated as two partitions

#### Scenario: Count out of range
- **WHEN** a job declares `partitions = 0` or `partitions = 65`
- **THEN** validation names the job and the field, and no jobs execute

#### Scenario: Unsupported strategy or key
- **WHEN** a job declares `partitions = { count = 3, strategy = "timings" }` or includes a key such as `files`
- **THEN** validation fails stating the value is not supported, and no jobs execute

### Requirement: Partitioned jobs expand into scheduled instances
A job with count N SHALL run as N instances, numbered 1 to N. Each instance MUST occupy its own parallelism slot and have its own condition evaluation, steps, timeout budget, status, duration and failure tails. Instances SHALL be scheduled in index order at the job's position in the job list. Instances that cannot all start because of the parallelism limit SHALL wait like any other ready job.

#### Scenario: Instances use separate slots
- **WHEN** parallelism is 4 and a ready job has `partitions = 3`
- **THEN** three instances run concurrently and one slot remains for other ready jobs

#### Scenario: More partitions than slots
- **WHEN** parallelism is 2 and a ready job has `partitions = 3`
- **THEN** two instances start, and the third starts when a slot frees

#### Scenario: Per-instance timeout
- **WHEN** a partitioned job sets `timeout_secs` and one instance exceeds it
- **THEN** only that instance times out, and the other instances keep their own budgets

### Requirement: Instances receive their partition identity
Every command, step and shell condition of an instance SHALL receive `GOODFLOWS_PARTITION` (the 1-based index) and `GOODFLOWS_PARTITION_COUNT` (N), and expression `env` lookups SHALL see the same values. These values MUST override job, step and inherited values. Commands of unpartitioned jobs MUST NOT receive either variable, even if the CLI process inherited it. Any job or step `env` that sets either name MUST fail validation.

#### Scenario: Command reads its partition
- **WHEN** instance 2 of a 3-partition job runs `echo $GOODFLOWS_PARTITION/$GOODFLOWS_PARTITION_COUNT`
- **THEN** its output is `2/3`

#### Scenario: Condition targets one partition
- **WHEN** a partitioned job has condition `{ expression = 'env.GOODFLOWS_PARTITION == "1"' }`
- **THEN** instance 1 runs and the other instances are skipped by condition

#### Scenario: Nested invocation
- **WHEN** Goodflows is launched from an environment that already exports `GOODFLOWS_PARTITION`
- **THEN** its unpartitioned jobs do not receive that variable

#### Scenario: Reserved name in config
- **WHEN** any job or step `env` sets `GOODFLOWS_PARTITION` or `GOODFLOWS_PARTITION_COUNT`
- **THEN** validation fails and no jobs execute

### Requirement: Instances are addressed by their job name
Selection and dependencies SHALL refer to a partitioned job by its name only. `--jobs <name>` MUST select all of its instances. A dependent of a partitioned job SHALL start only after every instance passes. Any instance that does not pass SHALL skip the dependent with the usual dependency reason, after all instances have finished.

#### Scenario: Selecting a partitioned job
- **WHEN** the user runs `goodflows --jobs test` and `test` has `partitions = 3`
- **THEN** all three instances run, along with their transitive prerequisites

#### Scenario: All instances pass
- **WHEN** every instance of `test` passes
- **THEN** jobs depending on `test` become ready

#### Scenario: One instance fails
- **WHEN** instance 2 of `test` fails and instances 1 and 3 pass
- **THEN** jobs depending on `test` are skipped as dependency failures

#### Scenario: Dependency skipped
- **WHEN** a dependency of a partitioned job fails
- **THEN** every instance is recorded as skipped, each with its partition identity

### Requirement: Partitioned jobs publish no job outputs
Partitioned jobs SHALL publish no job-level outputs. An `outputs` mapping on a partitioned job and any `needs.<job>.outputs.*` reference to a partitioned job MUST fail validation. Command-only partitioned jobs MUST NOT receive `GOODFLOWS_OUTPUT`. Steps inside an instance SHALL keep receiving it, and step outputs SHALL remain available to later steps of the same instance.

#### Scenario: Output mapping rejected
- **WHEN** a partitioned job with steps declares an `outputs` mapping
- **THEN** validation fails and no jobs execute

#### Scenario: Dependent reads partitioned outputs
- **WHEN** a job's condition references `needs.test.outputs.x` and `test` is partitioned
- **THEN** validation fails and no jobs execute

#### Scenario: Step outputs within an instance
- **WHEN** step `a` of an instance writes outputs and step `b` reads `steps.a.outputs.key`
- **THEN** step `b` sees the value written by step `a` of the same instance

### Requirement: Existing failure rules apply per instance
Fail-fast, cancellation, condition and step rules SHALL apply to each instance exactly as they apply to an ordinary job. With fail-fast enabled, a failed or timed-out instance SHALL prevent sibling instances that have not started from starting. Running siblings SHALL finish.

#### Scenario: Fail-fast with siblings waiting
- **WHEN** fail-fast is enabled, parallelism is 2, instance 1 fails and instance 3 has not started
- **THEN** instance 3 is skipped for fail-fast and instance 2 runs to completion

#### Scenario: Signal during partitions
- **WHEN** a signal arrives while instances are running
- **THEN** running instances are cancelled, unstarted instances are recorded as cancelled, and the signal exit code is preserved

### Requirement: Output identifies each instance
Text output SHALL show instances as `<name> (<index>/<count>)`. JSON job and step events and job results for an instance SHALL include `partition: {"index": i, "count": N}`. Events for unpartitioned jobs MUST keep their current shape. With a log directory, each instance SHALL write its own stdout and stderr log files. Run summary counts and the `run_started` job count SHALL count instances.

#### Scenario: Text output
- **WHEN** instance 2 of `test` with 3 partitions starts in text mode
- **THEN** the line identifies it as `test (2/3)`

#### Scenario: JSON output
- **WHEN** the CLI runs with `--json` and an instance finishes
- **THEN** its `job_finished` result includes `"name": "test"` and `"partition": {"index": 2, "count": 3}`, and the line is a valid versioned event

#### Scenario: Unpartitioned job events
- **WHEN** a job without `partitions` runs with `--json`
- **THEN** its events contain no `partition` field

#### Scenario: Separate logs
- **WHEN** a 3-partition job runs with `--log-dir`
- **THEN** three distinct stdout logs and three distinct stderr logs are written, none overwriting another

#### Scenario: Summary counts instances
- **WHEN** a workflow has `compile` and a 3-partition `test`, and everything passes
- **THEN** `run_started` reports 4 jobs and the summary reports 4 passed

### Requirement: Partitions are not combined with reporting
A job that declares both `partitions` and `reports` MUST fail validation until per-instance report collection is specified.

#### Scenario: Partitioned reporting job
- **WHEN** a job declares `partitions = 3` and `reports = ["exunit-timings"]`
- **THEN** validation fails, naming both fields, and no jobs execute

### Requirement: Inspection shows partition configuration
`goodflows validate` and `goodflows list` SHALL show each job's partition count and strategy without running commands. Their defined-job counts SHALL keep counting configured jobs, not instances.

#### Scenario: List a partitioned job
- **WHEN** the user runs `goodflows list` for a config whose `test` job has `partitions = 3`
- **THEN** the output shows `test` once with a count of 3, and no command executes
