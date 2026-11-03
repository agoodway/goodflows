# Spec Delta

## ADDED Requirements

### Requirement: Environment profiles declare partition counts
An environment profile SHALL accept an optional `partitions` map from job name to an integer count from 1 to 64. An omitted or empty map SHALL contribute no overrides. A non-integer value, a table, or a count outside that range MUST fail validation before jobs execute, whether or not the profile is selected. A range error MUST name the profile and the job. A type error MUST identify the TOML source location, offending entry, and expected integer type; it need not include the full profile path.

#### Scenario: Profile sets a count
- **WHEN** the local profile sets `test = 6` and `test` declares `partitions`
- **THEN** validation of that entry succeeds

#### Scenario: Omitted or empty map
- **WHEN** a profile omits `partitions` or sets it to an empty map
- **THEN** validation succeeds and the profile contributes no count overrides

#### Scenario: Count out of range
- **WHEN** an unselected profile sets `test = 0` or `test = 65` for a job that declares `partitions`
- **THEN** validation names the profile and the job, and no jobs execute

#### Scenario: Non-integer value
- **WHEN** a profile sets `test` to a string or a table such as `{ count = 6 }`
- **THEN** parsing fails with the TOML source location, offending entry, and expected integer type, and no jobs execute even when the profile is not selected

### Requirement: Selected profiles replace declared partition counts
When a profile is selected, each partition entry SHALL replace the count of the same-named job in the selected workflow that declares `partitions`. The strategy and all other job fields MUST stay as declared. A job the profile omits, and every job when no profile is selected, SHALL keep its declared count. Partition-count overrides MUST NOT alter parallelism; shared, workflow, profile, and CLI parallelism settings SHALL retain their existing precedence.

#### Scenario: Profile raises the count
- **WHEN** `test` declares `partitions = 3` with strategy `index` and the selected local profile sets `test = 6`
- **THEN** the run schedules six instances, instance 2 sees `GOODFLOWS_PARTITION=2` and `GOODFLOWS_PARTITION_COUNT=6`, and the strategy stays `index`

#### Scenario: Profile lowers the count to one
- **WHEN** `test` declares `partitions = 3` and the selected profile sets `test = 1`
- **THEN** one instance runs and sees `GOODFLOWS_PARTITION=1` and `GOODFLOWS_PARTITION_COUNT=1`

#### Scenario: Declared count is kept
- **WHEN** `test` declares `partitions = 3` and the selected profile omits `test`, or no profile is selected
- **THEN** the run schedules three instances

#### Scenario: Parallelism stays independent
- **WHEN** parallelism is 2 and the selected profile sets `test = 6` on a job that declares `partitions`
- **THEN** two instances start, and the others start as slots free

### Requirement: Profile partition names must match a partitioned job
A profile partition entry MUST name a job that declares `partitions` in at least one workflow. An entry that names no such job MUST fail validation before jobs execute, whether or not the profile is selected. When the entry names no job in any workflow, the error MUST name the profile and the job, say the job is unknown, and list the jobs that declare `partitions`. When the selected workflow contains that job and the job does not declare `partitions`, validation MUST fail before jobs execute, and the error MUST name the profile, the workflow, and the job. An entry whose job is absent from the selected workflow SHALL be ignored. Aggregate `goodflows validate` without `--env` SHALL resolve every workflow under every declared profile and fail with that same error when any combination would fail.

#### Scenario: Unknown job
- **WHEN** a profile sets `missing = 2`, no workflow has a job named `missing`, and `test` declares `partitions`
- **THEN** validation fails with `environment '<profile>': unknown job 'missing' (partitioned jobs: test)`, and no jobs execute, even when that profile is not selected

#### Scenario: Never-partitioned job
- **WHEN** a profile sets `test = 6` and jobs named `test` exist but none declares `partitions`
- **THEN** validation fails with `environment '<profile>': job 'test' is not partitioned`, and no jobs execute, even when that profile is not selected

#### Scenario: Selected workflow's job is not partitioned
- **WHEN** the selected workflow `nightly` has an unpartitioned job named `test` and the local profile sets `test = 6`
- **THEN** validation fails with `environment 'local': workflow 'nightly': job 'test' is not partitioned`, and no jobs execute

#### Scenario: Same name differs by workflow
- **WHEN** workflow `ci` has partitioned `test`, workflow `nightly` has unpartitioned `test`, and the local profile sets `test = 6`
- **THEN** `goodflows run ci --env local` uses six instances, and `goodflows run nightly --env local` fails before jobs execute, even when `--jobs` selects only other `nightly` jobs

#### Scenario: Entry unused by the selected workflow
- **WHEN** only workflow `ci` has partitioned `test`, the local profile sets `test = 6`, and the user runs workflow `nightly` with `--env local`
- **THEN** `nightly` runs and the `test` entry is ignored

#### Scenario: Aggregate inspection rejects an incompatible workflow
- **WHEN** workflow `ci` has partitioned `test`, workflow `nightly` has unpartitioned `test`, the local profile sets `test = 6`, and the user runs `goodflows validate --env local` or `goodflows list --workflows --env local`
- **THEN** the command fails naming the profile, the workflow, and the job before any jobs execute

#### Scenario: Aggregate validation without a profile checks every profile
- **WHEN** workflow `ci` has partitioned `test`, workflow `nightly` has unpartitioned `test`, the local profile sets `test = 6`, and the user runs `goodflows validate` without `--env`
- **THEN** validation fails with `environment 'local': workflow 'nightly': job 'test' is not partitioned`, while `goodflows validate nightly` and `goodflows run nightly` without `--env` still succeed

## MODIFIED Requirements

### Requirement: Inspection shows partition configuration
`goodflows list` SHALL show each partitioned job's effective partition count and strategy, in text and in the JSON `jobs` event, without running commands. The effective count is the job's declared count, replaced by the selected profile when that profile sets the job. `goodflows validate` and `goodflows list` defined-job and selected-job counts SHALL keep counting configured jobs, not instances.

#### Scenario: List a partitioned job
- **WHEN** the user runs `goodflows list` for a config whose `test` job has `partitions = 3`
- **THEN** the output shows `test` once with a count of 3 and strategy `index`, and no command executes

#### Scenario: List uses the profile count
- **WHEN** the user runs `goodflows list --env local` and the local profile sets `test = 6` for a job that declares `partitions = 3`
- **THEN** the output shows `test` once with a count of 6 and strategy `index`, and no command executes

#### Scenario: List without a profile keeps the declared count
- **WHEN** the user runs `goodflows list` and a profile sets `test = 6` for a job that declares `partitions = 3`
- **THEN** the output shows `test` once with a count of 3 and strategy `index`, and no command executes

#### Scenario: Validate counts configured jobs
- **WHEN** the user runs `goodflows validate` for a config with `compile` and a 3-partition `test`
- **THEN** it reports 2 defined jobs
