# Spec Delta

## Purpose

Keeps each workflow run's stdout and stderr in its own directory and retains a configurable number of those runs.

## ADDED Requirements

### Requirement: Each log run has its own directory
When `--log-dir` is set, a run SHALL write that invocation's logs in a new directory named `YYYYMMDDTHHMMSSZ-<run-id>` under the effective log directory used today. The timestamp SHALL be the run's UTC start time to one second, and the run id a lowercase UUID. When reporting is configured, the run id SHALL be the same UUID used as the reporting `workflow_run_id`. Named workflows keep their encoded workflow subdirectory. Legacy top-level jobs stay directly under `--log-dir`. The same timestamp and run id SHALL name that run's history file when history is also written.

#### Scenario: Named workflow
- **WHEN** workflow `ci` runs with `--log-dir logs`
- **THEN** its job logs are under `logs/ci/<YYYYMMDDTHHMMSSZ>-<run-id>/` and `summary.log_dir` names that directory

#### Scenario: Legacy top-level jobs
- **WHEN** a config with top-level jobs runs with `--log-dir logs`
- **THEN** its job logs are under `logs/<YYYYMMDDTHHMMSSZ>-<run-id>/` and not under `logs/default/`

#### Scenario: No log directory
- **WHEN** a run does not pass `--log-dir`
- **THEN** no log run directory is created

### Requirement: Log bytes and basenames stay the same
Files inside the run directory SHALL keep today's basenames and contents: raw stdout and raw stderr bytes, one pair per instance that enters execution, including output from a shell condition and from each step, with no prefixes or timestamps. The only other entry SHALL be the run's lock file, `.goodflows.lock`. Directories goodflows creates for logs (the run directory and any missing parent) SHALL be private to the invoking user (mode 0700), and the lock and log files SHALL have mode 0600. Each log file SHALL be created new without following a symlink; an existing entry at a log path SHALL fail that instance rather than be followed or truncated. An instance that never enters execution SHALL create no log files. A later run MUST NOT truncate a run directory that retention still keeps.

#### Scenario: Step output is still raw
- **WHEN** a job's shell condition and command both write to stdout
- **THEN** that run's stdout log contains both outputs as raw bytes and its basename still ends in `.stdout.log`

#### Scenario: Planted symlink
- **WHEN** a symlink already exists at an instance's log path inside the run directory
- **THEN** that instance fails with a log creation error, its command does not run, and the symlink target is unchanged

#### Scenario: Second run keeps the first
- **WHEN** the same selection runs twice with `--log-dir` and `log_runs` at least 2
- **THEN** the first run's log files still exist in their original directory and the second run writes a different directory

### Requirement: Log retention keeps the newest runs
A workflow's log directory SHALL retain at most `log_runs` matching run directories, default 10, minimum 1. After the new directory is created, older matches SHALL be deleted until the cap holds, newest by name kept, counting every matching directory in the workflow's log directory whichever profile produced it and using the cap of the run doing the prune. That run's new directory MUST NOT be deleted. A directory whose run is still active MUST NOT be deleted: each run SHALL hold an exclusive advisory lock on its `.goodflows.lock` for the whole run, taken before the directory first appears under its run-stem name, and prune SHALL skip any directory whose lock it cannot take without blocking, so the cap may be exceeded while runs overlap. Prune SHALL treat only a missing lock file as unlocked; a lock file it cannot open for another reason SHALL keep that directory and be reported like a prune failure. An unfinished run that is no longer active counts toward the cap. An entry that is already gone when prune reaches it (for example because a concurrent run pruned it) SHALL count as pruned and MUST NOT be reported as a failure. Unrelated names MUST stay. Deletion MUST NOT follow or remove a symlink. A failure to create the run directory SHALL fail the run before jobs start. A prune failure SHALL be reported and MUST NOT change the exit code. A CLI value of 0 for `--log-runs` SHALL be rejected as an argument error with exit code 2.

#### Scenario: Default keeps ten
- **WHEN** eleven runs use `--log-dir` and `log_runs` is unset
- **THEN** the newest ten run directories remain and the oldest is gone

#### Scenario: Configured cap
- **WHEN** a selected profile sets `log_runs = 2` and three runs use `--log-dir`
- **THEN** only the two newest run directories remain

#### Scenario: Unrelated file
- **WHEN** the log directory contains a file named `notes.txt` and an eleventh run is pruned
- **THEN** `notes.txt` remains

#### Scenario: Overlapping runs
- **WHEN** `log_runs` is 1 and a second run starts in the same log directory while the first is still running
- **THEN** the first run's directory is not deleted and its jobs still write their logs

#### Scenario: Prune failure
- **WHEN** an older run directory's lock file exists but cannot be opened
- **THEN** that directory is kept, a `failed to prune log runs` warning is reported on stderr, and the run's exit code is unchanged

#### Scenario: Invalid cap
- **WHEN** shared, workflow, or profile settings set `log_runs = 0`
- **THEN** validation fails with exit code 1 and a message naming the setting before jobs run

#### Scenario: Invalid CLI cap
- **WHEN** the run passes `--log-runs 0`
- **THEN** argument parsing fails with exit code 2

### Requirement: Log retention follows settings precedence
`log_runs` SHALL be accepted on shared settings, workflow settings, and profile settings. The effective value SHALL follow CLI, then selected profile, then workflow, then shared settings, then the default. `--log-runs` SHALL be that CLI override. The value SHALL have no effect without `--log-dir`. The effective value is the cap the run applies to the whole workflow log directory; it does not reserve room for one profile's runs, which need their own `--log-dir` to be kept apart.

#### Scenario: Flag wins
- **WHEN** shared settings set `log_runs = 2` and five runs pass `--log-dir` and `--log-runs 4`
- **THEN** four run directories are kept

#### Scenario: Workflow setting isolates workflows
- **WHEN** only workflow `ci` sets `log_runs = 2` and workflow `other` runs eleven times with `--log-dir`
- **THEN** `ci` keeps two run directories and `other` keeps the default of ten

#### Scenario: Profile cap prunes other profiles' runs
- **WHEN** profile `tight` sets `log_runs = 1` and a run with `--env tight` follows runs without it in the same log directory
- **THEN** only the `tight` run's directory remains
