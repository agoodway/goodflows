# Spec Delta

## MODIFIED Requirements

### Requirement: Output identifies each instance
Text output SHALL show instances as `<name> (<index>/<count>)`. JSON job and step events and job results for an instance SHALL include `partition: {"index": i, "count": N}`. Events for unpartitioned jobs MUST keep their current shape. With a log directory, each instance SHALL write its own stdout and stderr log files inside that invocation's run directory, as specified by `run-logs`. A later run MUST NOT truncate a run directory that retention still keeps. JSON run summary counts and the `run_started` job count SHALL count instances. The text run report SHALL count configured jobs and show a partitioned job as one row, as specified by `run-report`.

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
- **THEN** three distinct stdout logs and three distinct stderr logs are written inside that run's directory, named with the zero-padded partition index (for example `002-test.p01.stdout.log`), none overwriting another

#### Scenario: Summary counts instances
- **WHEN** a workflow has `compile` and a 3-partition `test`, everything passes, and the CLI runs with `--json`
- **THEN** `run_started` reports 4 jobs and the `summary` event reports 4 passed

#### Scenario: Text report counts configured jobs
- **WHEN** a workflow has `compile` and a 3-partition `test`, everything passes, and the CLI runs in text mode
- **THEN** the run report shows one row for `compile`, one row for `test`, and counts 2 passed

#### Scenario: Later run keeps instance logs
- **WHEN** a 3-partition job runs twice with `--log-dir` and `log_runs` at least 2
- **THEN** the first run's six log files remain in their original directory and the second run writes six files in a different directory
