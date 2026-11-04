# Spec Delta

## Purpose

Keeps each workflow run's stdout and stderr in its own directory and retains a configurable number of those runs.

## ADDED Requirements

### Requirement: Each log run has its own directory
When `--log-dir` is set, a run SHALL write that invocation's logs in a new directory named `<YYYYMMDDTHHMMSS>Z-<run-id>` under the effective log directory used today. The timestamp SHALL be the run's UTC start time to one second, and the run id a lowercase UUID. Named workflows keep their encoded workflow subdirectory. Legacy top-level jobs stay directly under `--log-dir`. The same timestamp and run id SHALL name that run's history file when history is also written.

#### Scenario: Named workflow
- **WHEN** workflow `ci` runs with `--log-dir logs`
- **THEN** its job logs are under `logs/ci/<timestamp>Z-<run-id>/` and `summary.log_dir` names that directory

#### Scenario: Legacy top-level jobs
- **WHEN** a config with top-level jobs runs with `--log-dir logs`
- **THEN** its job logs are under `logs/<timestamp>Z-<run-id>/` and not under `logs/default/`

#### Scenario: No log directory
- **WHEN** a run does not pass `--log-dir`
- **THEN** no log run directory is created

### Requirement: Log bytes and basenames stay the same
Files inside the run directory SHALL keep today's basenames and contents: raw stdout and raw stderr bytes, one pair per instance that enters execution, including output from a shell condition and from each step, with no prefixes or timestamps. An instance that never enters execution SHALL create no log files. A later run MUST NOT truncate a run directory that retention still keeps.

#### Scenario: Step output is still raw
- **WHEN** a job's shell condition and command both write to stdout
- **THEN** that run's stdout log contains both outputs as raw bytes and its basename still ends in `.stdout.log`

#### Scenario: Second run keeps the first
- **WHEN** the same selection runs twice with `--log-dir` and `log_runs` at least 2
- **THEN** the first run's log files still exist in their original directory and the second run writes a different directory

### Requirement: Log retention keeps the newest runs
A workflow's log directory SHALL retain at most `log_runs` matching run directories, default 10, minimum 1. After the new directory is created, older matches SHALL be deleted until the cap holds, newest by name kept. That run's new directory MUST NOT be deleted. An unfinished run counts. Unrelated names MUST stay. Deletion MUST NOT follow or remove a symlink. Creating the directory SHALL fail the run before jobs start. A prune failure SHALL be reported and MUST NOT change the exit code.

#### Scenario: Default keeps ten
- **WHEN** eleven runs use `--log-dir` and `log_runs` is unset
- **THEN** the newest ten run directories remain and the oldest is gone

#### Scenario: Configured cap
- **WHEN** a selected profile sets `log_runs = 2` and three runs use `--log-dir`
- **THEN** only the two newest run directories remain

#### Scenario: Unrelated file
- **WHEN** the log directory contains a file named `notes.txt` and an eleventh run is pruned
- **THEN** `notes.txt` remains

#### Scenario: Invalid cap
- **WHEN** configuration sets `log_runs = 0`
- **THEN** validation fails before jobs run

### Requirement: Log retention follows settings precedence
`log_runs` SHALL be accepted on shared settings, workflow settings, and profile settings. The effective value SHALL follow CLI, then selected profile, then workflow, then shared settings, then the default. `--log-runs` SHALL be that CLI override. The value SHALL have no effect without `--log-dir`.

#### Scenario: Flag wins
- **WHEN** shared settings set `log_runs = 2` and the run passes `--log-runs 4`
- **THEN** four run directories are kept

#### Scenario: Profile isolates workflows
- **WHEN** only workflow `ci` sets `log_runs = 2`
- **THEN** workflow `other` still keeps the default of ten
