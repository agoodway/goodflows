# Spec Delta

## Purpose

Records an opt-in, immutable JSON file for each workflow run so an agent can compare scheduling, CPU time, and memory across commits.

## ADDED Requirements

### Requirement: History is opt-in
A run SHALL write history only when enabled by shared, workflow, or selected-profile `history = true`, by `--history`, or by `--history-dir`. The default SHALL be off. `--no-history` SHALL force history off and SHALL conflict with `--history`. `--history-dir` SHALL enable history unless `--no-history` is set. CLI flags SHALL beat profile settings, which beat workflow settings, which beat shared settings. `validate`, `list`, and `init` MUST NOT write a history file.

#### Scenario: Default writes nothing
- **WHEN** a run has no history setting and no history flag
- **THEN** no history file is created and the `summary` event has no `history_file` field

#### Scenario: Config enables it
- **WHEN** shared settings set `history = true`
- **THEN** the run writes one history file

#### Scenario: Flag disables config
- **WHEN** settings set `history = true` and the run passes `--no-history`
- **THEN** no history file is written

#### Scenario: Directory flag enables it
- **WHEN** history is unset and the run passes `--history-dir runs`
- **THEN** the history file is written under `runs`

### Requirement: History path follows the config file
The default directory SHALL be `goodflows-history`, resolved from the config file's directory. A configured `history_dir` SHALL resolve the same way. `--history-dir` SHALL resolve from the invocation directory, like `--log-dir`. Absolute paths SHALL be used as given. The file SHALL be `<history dir>/<encoded workflow>/<YYYYMMDDTHHMMSS>Z-<run-id>.json`. Legacy top-level jobs SHALL use the workflow component `default`. The workflow component SHALL use the same encoding as workflow log directories.

#### Scenario: Default beside the config
- **WHEN** history is enabled and `goodflows.toml` is in `/work`
- **THEN** the file is under `/work/goodflows-history/<workflow>/`

#### Scenario: Profile overrides the directory
- **WHEN** shared settings set `history_dir = "shared"` and the selected profile sets `history_dir = "profile"`
- **THEN** the file is written under `profile` resolved from the config file

### Requirement: Each history file is written once
The file SHALL be one compact UTF-8 JSON object with LF newlines and a single trailing newline. Its `schema_version` SHALL be 1. After it is written, goodflows MUST NOT modify it. A symlink at the history directory or workflow directory SHALL be refused. A write or prune failure SHALL be reported and MUST NOT change the run's exit code. The new file SHALL still be kept when it alone exceeds `history_bytes`.

#### Scenario: Second run leaves the first file untouched
- **WHEN** two enabled runs of the same workflow finish
- **THEN** two files exist and the first file's bytes are unchanged

#### Scenario: Disk error
- **WHEN** the history directory cannot be created
- **THEN** the jobs' exit code is unchanged and the failure is reported

### Requirement: History records the run
The file SHALL name the workflow, run id, UTC `started_at`, and `outcome` (`passed`, `failed`, or `cancelled`, using the run report's outcome rules). It SHALL include `duration_ms` with the same meaning as `summary.duration_ms`, effective `parallelism`, `graph_sha256`, `max_in_flight`, and host `logical_cpus`. Host `mem_total_bytes` and `mem_available_bytes` SHALL be included when the platform provides them and omitted otherwise.

#### Scenario: Passing run
- **WHEN** every instance passes or is skipped and history is enabled
- **THEN** the file's `outcome` is `passed` and it includes `parallelism` and `logical_cpus`

### Requirement: History records each instance
The file SHALL include one entry per instance in summary-result order, with `name`, `status`, `started`, `blocked_ms`, `queued_ms`, `duration_ms`, and partition when the instance has one. `blocked_ms` SHALL be the wait until dependencies pass, or until the instance is skipped or cancelled if that happens first. `queued_ms` SHALL be the wait after that until execution starts, or until a later skip or cancel. `max_in_flight` SHALL be the largest number of instances in execution at once.

#### Scenario: Ready job waited for a slot
- **WHEN** parallelism is 1 and a second job is ready while the first is running
- **THEN** the second job's `blocked_ms` covers only dependency wait and its `queued_ms` covers the wait for a slot

#### Scenario: Skipped before launch
- **WHEN** a job is skipped because a dependency failed
- **THEN** its entry has `started` false, `queued_ms` 0, and no `commands` entry

### Requirement: History records command cost
A spawned shell condition, job command, or step SHALL add a `commands` entry with `kind`, `wall_ms`, and the step id when it is a step. `cpu_user_ms` and `cpu_system_ms` SHALL be that process's user and system time, including descendants it waited for, when available. `peak_rss_bytes` SHALL be the peak concurrent resident set of its process group when observable. Unavailable measurements MUST be omitted. Command and error strings SHALL be cut to 1024 bytes on a character boundary.

#### Scenario: Quiet platform
- **WHEN** the platform cannot observe peak RSS
- **THEN** finished commands omit `peak_rss_bytes` and still include `wall_ms`

### Requirement: History omits command output and secrets
The history file MUST NOT contain stdout, stderr, environment values, absolute paths, or log filenames.

#### Scenario: No command output stored
- **WHEN** a job prints a secret to stdout
- **THEN** the history file does not contain that text

### Requirement: Summary carries the same optimizer fields
The `summary` event SHALL include the same run, instance, and command-cost fields as the history file, plus `history_file` when a file was written. Existing summary fields SHALL keep their names and meanings. Those added fields MUST NOT appear on live `job_finished` events.

#### Scenario: JSON summary without history
- **WHEN** a workflow runs with `--json` and history off
- **THEN** `summary` includes `blocked_ms`, `queued_ms`, and `max_in_flight` for the run, and omits `history_file`

#### Scenario: Live events stay unchanged
- **WHEN** a job finishes with `--json`
- **THEN** its `job_finished` event has no `blocked_ms`, `queued_ms`, or `commands` field

### Requirement: Graph hash ignores environment values
`graph_sha256` SHALL be the lowercase hex SHA-256 of a canonical JSON document of the selected jobs, in configuration order, containing only job name, command, `depends_on`, partition count, `timeout_secs`, shell flag, step id and command, and condition text. It MUST exclude environment values, paths, and runtime output. The same selection SHALL hash the same on every run.

#### Scenario: Env does not change the hash
- **WHEN** two runs select the same jobs and differ only in settings env values
- **THEN** their `graph_sha256` values are equal

#### Scenario: Command change
- **WHEN** a selected job's command changes
- **THEN** `graph_sha256` changes

### Requirement: History retention is per workflow
Each workflow directory SHALL keep at most `history_runs` matching files, default 1000, and at most `history_bytes` bytes, default 10485760. Both minima SHALL be 1. After the new file is written, the oldest matching files by name SHALL be deleted until both caps hold. The new file MUST NOT be deleted. Unrelated names and other workflows MUST be left in place. Deletion MUST NOT follow or remove a symlink. Both caps SHALL use the same settings precedence as `history`.

#### Scenario: Byte cap drops the oldest
- **WHEN** `history_bytes` is smaller than two finished records and `history_runs` is 1000
- **THEN** only the newest file remains

#### Scenario: Count cap
- **WHEN** `history_runs` is 2 and three runs finish under the byte cap
- **THEN** the two newest files remain

#### Scenario: Other workflow
- **WHEN** workflow `ci` exceeds its cap
- **THEN** workflow `other` files are unchanged

#### Scenario: Invalid cap
- **WHEN** configuration sets `history_runs = 0` or `history_bytes = 0`
- **THEN** validation fails before jobs run

### Requirement: Inspection shows effective history settings
`list` and `validate` JSON settings SHALL include effective `history`, `history_dir`, `log_runs`, `history_runs`, and `history_bytes`. Human `list` output SHALL show the same values. The displayed `history_dir` SHALL be the configured or default path before invocation-relative CLI resolution is applied to a run.

#### Scenario: Defaults
- **WHEN** the user runs `goodflows list` with no history settings
- **THEN** the settings show history off, directory `goodflows-history`, `log_runs` 10, `history_runs` 1000, and `history_bytes` 10485760
