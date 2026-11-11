# run-history Specification

## Purpose

Records an opt-in, immutable JSON file for each workflow run so an agent can compare scheduling, CPU time, and memory across commits.

## Requirements

### Requirement: History is opt-in
A run SHALL write history only when enabled by shared, workflow, or selected-profile `history = true`, by `--history`, or by `--history-dir`. The default SHALL be off. `--no-history` SHALL force history off and SHALL conflict with `--history`. `--history-dir` SHALL enable history unless `--no-history` is set. CLI flags SHALL beat profile settings, which beat workflow settings, which beat shared settings, and an explicit `history = false` SHALL beat a lower `true`. These settings and flags SHALL control only the local history file; uploading the run record is controlled by `[reporting] history`. `validate`, `list`, `init`, `reports upload`, `history list`, and `history show` MUST NOT write a history file. `reports upload`, `history list`, and `history show` SHALL reject `--history`, `--no-history`, `--history-dir`, `--log-runs`, `--history-runs`, and `--history-bytes`.

#### Scenario: Default writes nothing
- **WHEN** a run has no history setting and no history flag
- **THEN** no history file is created and the `summary` event has no `history_file` field

#### Scenario: Config enables it
- **WHEN** shared settings set `history = true`
- **THEN** the run writes one history file

#### Scenario: Profile disables shared history
- **WHEN** shared settings set `history = true` and the selected profile sets `history = false`
- **THEN** no history file is written

#### Scenario: Flag disables config
- **WHEN** settings set `history = true` and the run passes `--no-history`
- **THEN** no history file is written

#### Scenario: Directory flag enables it
- **WHEN** history is unset and the run passes `--history-dir runs`
- **THEN** the history file is written under `runs`

#### Scenario: Reports upload rejects history flags
- **WHEN** the user runs `goodflows reports upload <dir> --history`
- **THEN** the command fails before uploading and names the rejected flag

#### Scenario: History commands reject history flags
- **WHEN** the user runs `goodflows history list --no-history`
- **THEN** the command fails before any request and names the rejected flag

#### Scenario: No-history does not stop the upload
- **WHEN** `[reporting] history = true`, the reporting key is set, and the run passes `--no-history`
- **THEN** no history file is written and the run record is still uploaded

### Requirement: History path follows the config file
The default directory SHALL be `goodflows-history`, resolved from the config file's directory. A configured `history_dir` SHALL resolve the same way. `--history-dir` SHALL resolve from the invocation directory, like `--log-dir`. Absolute paths SHALL be used as given. The file SHALL be `<history dir>/<encoded workflow>/YYYYMMDDTHHMMSSZ-<run-id>.json`. Legacy top-level jobs SHALL use the workflow component `default`. The workflow component SHALL use the same encoding as workflow log directories.

#### Scenario: Default beside the config
- **WHEN** history is enabled and `goodflows.toml` is in `/work`
- **THEN** the file is under `/work/goodflows-history/<workflow>/`

#### Scenario: Encoded workflow names
- **WHEN** workflows `ci`, `CI`, `%43%49`, and `../escape` each write history
- **THEN** their files are under `ci`, `%43%49`, `%2543%2549`, and `%2E%2E%2Fescape` inside the history directory, and nothing is written outside it

#### Scenario: Profile overrides the directory
- **WHEN** shared settings set `history_dir = "shared"` and the selected profile sets `history_dir = "profile"`
- **THEN** the file is written under `profile` resolved from the config file

### Requirement: Each history file is written once
A history file SHALL be written exactly when the run emits a `summary` event, including a run cancelled by a signal; a run that errors before a `summary` exists SHALL write none. The file SHALL be one compact UTF-8 JSON object with LF newlines and a single trailing newline. Its `schema_version` SHALL be 1. The file SHALL be created atomically without replacing an existing file, and after it is written goodflows MUST NOT modify it. A symlink at the history directory or workflow directory SHALL be refused. Directories goodflows creates for history SHALL be private to the invoking user (mode 0700) and the file SHALL have mode 0600. The workflow directory SHALL be opened once without following a symlink, and the file SHALL be created, linked into place, and pruned relative to that open directory, so a path component swapped for a symlink during the write cannot redirect it. A write failure SHALL be reported on stderr, MUST NOT change the run's exit code, and SHALL leave `history_file` absent. A prune failure after a successful write SHALL be reported on stderr, MUST NOT change the exit code, and SHALL leave `history_file` naming the written file. The new file SHALL still be kept when it alone exceeds `history_bytes`.

#### Scenario: Second run leaves the first file untouched
- **WHEN** two enabled runs of the same workflow finish
- **THEN** two files exist and the first file's bytes are unchanged

#### Scenario: Disk error
- **WHEN** the history directory cannot be created
- **THEN** the jobs' exit code is unchanged, the failure is reported on stderr, and `summary` has no `history_file`

#### Scenario: Prune failure after a write
- **WHEN** the history file is written but deleting an older file fails
- **THEN** the failure is reported on stderr, the exit code is unchanged, and `summary.history_file` names the new file

#### Scenario: Cancelled run
- **WHEN** a history-enabled run is interrupted by a signal and emits a `summary`
- **THEN** a history file is written with `outcome` `cancelled`

#### Scenario: Run errors before summary
- **WHEN** a history-enabled run fails before any `summary` is emitted, for example because the log run directory cannot be created
- **THEN** no history file is written

### Requirement: History records the run
The file SHALL include `schema_version`, `workflow`, `run_id`, `started_at`, and `outcome`. `run_id` SHALL be the run's lowercase UUID. `started_at` SHALL be RFC 3339 UTC to the second (`YYYY-MM-DDTHH:MM:SSZ`), equal to the stem timestamp. `outcome` SHALL be `failed` if any instance is `failed` or `timed_out`; otherwise `cancelled` if any instance is `cancelled` or the signal state snapshotted for the exit code is set; otherwise `passed`. That one snapshot SHALL drive the history file, `summary.outcome`, the report header, and the exit code, in text and `--json` mode alike. The file SHALL include `duration_ms`, measured from immediately before `runner::execute` until it returns (equal to `summary.execute_ms`, and excluding reporting after the last job), effective `parallelism`, `graph_sha256`, `max_in_flight`, and a `host` object. `max_in_flight` SHALL be the largest number of instances in execution at once. `host` SHALL be sampled at run start, before any job runs, and SHALL contain `os` and `arch` (`std::env::consts::OS` and `ARCH`) and `logical_cpus`, and `mem_total_bytes` and `mem_available_bytes` when the platform provides them, omitted otherwise. The file SHALL include `environment`, the selected `--env` profile, omitted when none was selected; `requested_jobs`, the `--jobs` values as given, omitted when empty; and `source`, an object with the `commit` the run started from and, when known, whether tracked files were `dirty`, captured before jobs run the same way reporting captures source context (reusing that capture when reporting made one) and omitted when no commit is available. `source` MUST NOT include the branch or any remote. These additions keep `schema_version` 1.

#### Scenario: Passing run
- **WHEN** every instance passes or is skipped and history is enabled
- **THEN** the file's `outcome` is `passed` and it includes `parallelism` and `host.logical_cpus`

#### Scenario: Timed-out run
- **WHEN** one instance times out and the rest pass
- **THEN** the file's `outcome` is `failed`

#### Scenario: Run context
- **WHEN** a history-enabled run in a clean git checkout passes `--env ci --jobs test`
- **THEN** the file has `environment` `ci`, `requested_jobs` `["test"]`, `source.commit` naming `HEAD`, `source.dirty` false, and `host.os` and `host.arch`

#### Scenario: Outside a git checkout
- **WHEN** a history-enabled run's workdir is not in a git checkout and no `--env` or `--jobs` is passed
- **THEN** the file has no `source`, `environment`, or `requested_jobs` field

#### Scenario: Reporting time is not job time
- **WHEN** reporting uploads take time after the last job finishes
- **THEN** the file's `duration_ms` equals `summary.execute_ms` and is at most `summary.duration_ms`

#### Scenario: Signal after the last job in JSON mode
- **WHEN** a `--json` run receives a signal after every job has finished but before the summary is emitted
- **THEN** the history file, `summary.outcome`, and the exit code all report `cancelled`

### Requirement: History records each instance
The file SHALL include one entry per instance in summary-result order, with `name`, partition when the instance has one, `status`, `started`, `exit_code` (null when none), `command` (null when none), `error` (null when none), `blocked_ms`, `queued_ms`, `duration_ms`, and `commands`. Only these fields SHALL be written; stdout and stderr tails and job or step `outputs` MUST NOT be written. `blocked_ms` SHALL be measured from the start of `runner::execute` until the last dependency's result is recorded, or until the instance is skipped or cancelled if that happens first; an instance with no dependencies SHALL have `blocked_ms` near 0. `queued_ms` SHALL be the wait after that until execution starts, or until a later skip or cancel. `command` and `error` strings SHALL be cut to 1024 bytes on a character boundary.

#### Scenario: Ready job waited for a slot
- **WHEN** parallelism is 1 and a second job is ready while the first is running
- **THEN** the second job's `blocked_ms` covers only dependency wait and its `queued_ms` covers the wait for a slot

#### Scenario: Skipped before launch
- **WHEN** a job is skipped because a dependency failed
- **THEN** its entry has `started` false, `queued_ms` 0, and its `commands` field is absent

#### Scenario: Failed job
- **WHEN** a job's command longer than 1024 bytes exits with code 3
- **THEN** its entry has `exit_code` 3 and a `command` of at most 1024 bytes ending on a character boundary

### Requirement: History records command cost
A spawned shell condition, job command, or step SHALL add a `commands` entry with `kind`, `wall_ms`, and `step` (the step id) when it is a step. `kind` SHALL be `condition` for a spawned shell condition of a job or step, and `command` for a job command or step command. `commands` SHALL be omitted when empty. `cpu_user_ms` and `cpu_system_ms` SHALL be that process's user and system time, including descendants it waited for, when available; for a `timed_out` or `cancelled` command they are a lower bound. `peak_rss_bytes` SHALL be the peak of sampled concurrent resident sets of the live processes still in its process group, and SHALL be omitted, never 0, when no sample saw a live process with resident memory. One sampler SHALL serve the whole run, reading `/proc` once per sample for every running command. Unavailable measurements MUST be omitted.

#### Scenario: Platform without RSS
- **WHEN** the platform cannot observe peak RSS
- **THEN** finished commands omit `peak_rss_bytes` and still include `wall_ms`

#### Scenario: Step kinds
- **WHEN** a step has a shell condition and a command and both run
- **THEN** the instance has a `condition` entry and a `command` entry, both with that step's `step` id

### Requirement: History omits command output and secrets
The history file MUST NOT contain stdout, stderr, environment values, or log filenames. goodflows MUST NOT add its own resolved paths (workdir, log directory, config path, history directory) to the file. Before truncation, in an `error` string, the per-run log directory together with any path beneath it (including a log filename) SHALL be replaced with `<log>`, and the workdir SHALL be replaced with `<workdir>` (a path below it keeps its tail), wherever either begins a whole path anywhere in the string. A replacement SHALL match only a whole path: it starts the string or follows a character that is neither `/` nor a path-name character, and it is followed by `/`, the end of the string, or a non-path character such as whitespace, a quote, or a colon, so a workdir `/home/u/proj` leaves `/home/u/proj2/x` and `/x/home/u/proj` unchanged. Configured command text SHALL be recorded verbatim, truncated, and is not path-scrubbed.

#### Scenario: No command output stored
- **WHEN** a job prints a secret to stdout, a failed job's stderr tail contains a secret, and a job writes a secret to `GOODFLOWS_OUTPUT`
- **THEN** the history file does not contain any of those secrets

#### Scenario: Log path in an error
- **WHEN** history is enabled and a job's log file cannot be created
- **THEN** that instance's `error` names `<log>` rather than the absolute log directory or the log filename

### Requirement: Summary carries the same optimizer fields
The `summary` event SHALL include the same run fields as the history file (`run_id`, `started_at`, `outcome`, `parallelism`, `graph_sha256`, `max_in_flight`, `host`) and integer `execute_ms`, the history file's `duration_ms`. The history-only `environment`, `requested_jobs`, and `source` fields are not added to the summary, which already carries `environment` when `--env` is set. Each `summary.results` entry SHALL include `started`, `blocked_ms`, and `queued_ms`, and `commands` when at least one process was spawned for that instance (omitted when empty), serialized by the same structure as the history file's instance fields, and `summary` SHALL include `history_file` when a file was written. `history_file` SHALL be the path the run wrote, as resolved for the run, converted lossily when not valid UTF-8; it SHALL NOT appear inside the history file. Existing summary fields SHALL keep their names and meanings, except that `log_dir` names the per-run log directory as specified by `run-logs`. Existing summary `command` and `error` strings SHALL NOT be truncated. Those added fields MUST NOT appear on live `job_finished` events.

#### Scenario: JSON summary without history
- **WHEN** a workflow runs with `--json` and history off
- **THEN** `summary` includes run-level `max_in_flight`, `parallelism`, and `graph_sha256`, each `summary.results` entry includes `blocked_ms` and `queued_ms`, and `history_file` is absent

#### Scenario: Live events stay unchanged
- **WHEN** a job finishes with `--json`
- **THEN** its `job_finished` event has no `blocked_ms`, `queued_ms`, or `commands` field

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

### Requirement: History retention is per workflow
Each workflow directory SHALL keep at most `history_runs` matching files, default 1000, and at most `history_bytes` bytes, default 10485760. Both minima SHALL be 1. After the new file is written, the oldest matching files by name SHALL be deleted until both caps hold, counting every matching file in the workflow directory whichever profile wrote it and using the caps of the run doing the prune. The new file MUST NOT be deleted. Unrelated names and other workflows MUST be left in place, except that a regular file named `.tmp` followed by ASCII letters and digits whose modification time is more than one hour old (a temporary file left by a crash mid-write) SHALL be deleted. A file that is already gone when prune reaches it (for example because a concurrent run pruned it) SHALL count as pruned and MUST NOT be reported as a failure. Deletion MUST NOT follow or remove a symlink. Both caps SHALL use the same settings precedence as `history`. A CLI value of 0 for `--history-runs` or `--history-bytes` SHALL be rejected as an argument error with exit code 2.

#### Scenario: Byte cap drops the oldest
- **WHEN** `history_bytes` is smaller than two finished records and `history_runs` is 1000
- **THEN** only the newest file remains

#### Scenario: Oversized newest file is kept
- **WHEN** `history_bytes` is 1 and a run finishes
- **THEN** that run's file remains and every older file is deleted

#### Scenario: Count cap
- **WHEN** `history_runs` is 2 and three runs finish under the byte cap
- **THEN** the two newest files remain

#### Scenario: Concurrent prunes
- **WHEN** two runs of the same workflow prune the same old files at once
- **THEN** neither reports a prune failure and both keep their `history_file`

#### Scenario: Stale temporary file
- **WHEN** the workflow directory holds a `.tmp<random>` file older than an hour and a newer one
- **THEN** prune deletes the old one and keeps the newer one

#### Scenario: Other workflow
- **WHEN** workflow `ci` exceeds its cap
- **THEN** workflow `other` files are unchanged

#### Scenario: Invalid cap
- **WHEN** shared, workflow, or profile settings set `history_runs = 0` or `history_bytes = 0`
- **THEN** validation fails with exit code 1 and a message naming the setting before jobs run

#### Scenario: Invalid CLI cap
- **WHEN** the run passes `--history-runs 0` or `--history-bytes 0`
- **THEN** argument parsing fails with exit code 2

### Requirement: Inspection shows effective history settings
`list` and `validate` JSON settings SHALL include effective `history`, `history_dir`, `log_runs`, `history_runs`, and `history_bytes`, applying CLI overrides the same way they apply `--parallelism`. Human `list` output SHALL show the same values. The displayed `history_dir` SHALL be the resolved path: the configured or default value joined to the config file's directory, or `--history-dir` joined to the invocation directory.

#### Scenario: Defaults
- **WHEN** the user runs `goodflows list` with no history settings and the config is in `/work`
- **THEN** the settings show history off, directory `/work/goodflows-history`, `log_runs` 10, `history_runs` 1000, and `history_bytes` 10485760

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

### Requirement: Reporting uploads each invocation's run record
When `[reporting] history` is `true`, each invocation that emits a `summary` (the root and every called workflow) SHALL upload its run record to `POST /api/v1/projects/<project>/workflow-runs`, whether or not the local history file is enabled. Without a `[reporting]` block, or with `history` unset or `false`, no run record SHALL be built, frozen, or sent.

#### Scenario: Upload without local history
- **WHEN** `[reporting] history = true`, the reporting key and URL are set, and local history is off
- **THEN** the run record is uploaded and no history file is written

#### Scenario: Reporting without history upload
- **WHEN** a `[reporting]` block omits `history` and a job declares `reports`
- **THEN** test reports are published as before and no request is sent to `workflow-runs`

#### Scenario: Called workflow uploads its own record
- **WHEN** upload is enabled and a call starts a child invocation
- **THEN** the child and the root each upload one record under their own `run_id`, the child's carrying `parent_run_id` and `caller_job` and the root's call result carrying `child_run_id`

#### Scenario: Run errors before summary
- **WHEN** an upload-enabled invocation fails before emitting a `summary`
- **THEN** no run record is built or sent

### Requirement: The uploaded record extends the history record
The uploaded record SHALL hold the same fields and values that the local history file would hold for that invocation. It SHALL add `source.branch` when the captured source has a valid branch, `ci` when CI context is available, and `graph`. The local history file MUST remain unchanged: no `branch`, `ci`, or `graph`. The uploaded record SHALL keep `schema_version` 1.

#### Scenario: Branch only in the upload
- **WHEN** local history and upload are both enabled in a git checkout on branch `main`
- **THEN** the uploaded record has `source.branch` `main` and the history file's `source` has no `branch`

#### Scenario: Same run fields
- **WHEN** local history and upload are both enabled
- **THEN** removing `graph`, `ci`, and `source.branch` from the uploaded record leaves the same JSON value as the history file

#### Scenario: Detached checkout
- **WHEN** the workdir is a git checkout with a detached `HEAD`
- **THEN** the uploaded record has `source.commit` and no `source.branch`

### Requirement: The uploaded graph is the hashed graph with text cut
`graph` SHALL be the canonical document that `graph_sha256` hashes: the same jobs, keys, order, and `call` objects. Each job `command`, step `command`, and condition `text` SHALL be cut to 1,024 bytes on a character boundary. `graph_sha256` SHALL still be computed over the uncut document. A called workflow's jobs SHALL NOT be inlined.

#### Scenario: Long command
- **WHEN** a selected job's command is 2,000 bytes long
- **THEN** that job's `graph` `command` is at most 1,024 bytes and `graph_sha256` equals the hash of the uncut document

#### Scenario: Workflow-call job
- **WHEN** a selected job calls workflow `suite`
- **THEN** its `graph` entry has `command` null, `steps` empty, and `call` with `workflow` `suite` and `suite`'s own `graph_sha256`, and `suite`'s jobs are not in the root record's `graph`

### Requirement: CI context comes from GitHub Actions variables
When the caller environment has `GITHUB_ACTIONS=true`, `ci` SHALL have `provider` `github_actions`. It SHALL also have `event`, `run_id`, `attempt`, `pull_request`, `runner`, and `url`, each taken from its GitHub variable and included only when the value passes its rule. Otherwise `ci` SHALL be absent. A value that fails its rule MUST be left out and MUST NOT make the record invalid.

#### Scenario: Push run
- **WHEN** `GITHUB_ACTIONS=true`, `GITHUB_EVENT_NAME=push`, `GITHUB_RUN_ID=123`, `GITHUB_RUN_ATTEMPT=2`, `RUNNER_NAME=runner-1`, `GITHUB_SERVER_URL=https://github.com`, and `GITHUB_REPOSITORY=acme/app`
- **THEN** `ci` is `{"provider":"github_actions","event":"push","run_id":"123","attempt":2,"runner":"runner-1","url":"https://github.com/acme/app/actions/runs/123"}`

#### Scenario: Pull request
- **WHEN** `GITHUB_ACTIONS=true` and `GITHUB_REF=refs/pull/42/merge`
- **THEN** `ci.pull_request` is `42`

#### Scenario: Invalid values are dropped
- **WHEN** `GITHUB_ACTIONS=true`, `GITHUB_RUN_ATTEMPT=0`, `RUNNER_NAME` is 65 bytes, and `GITHUB_SERVER_URL=http://ghe.local`
- **THEN** `ci` has `provider` and no `attempt`, `runner`, or `url`, and the record is still uploaded

#### Scenario: Not on GitHub Actions
- **WHEN** `GITHUB_ACTIONS` is unset or not `true`
- **THEN** the uploaded record has no `ci`

### Requirement: Run records are validated before they are frozen
Before freezing or sending, goodflows SHALL validate the encoded record against every rule of the server's run-history contract. These include the key allowlists, the name and text rules, the integer bounds, the 1,024-entry limits on `requested_jobs`, `commands`, `graph.jobs`, `depends_on` and `steps`, graph consistency, outcome agreement, no U+0000, nesting depth 8, and a 4 MiB body.

#### Scenario: Too many requested jobs
- **WHEN** an upload-enabled run passes 1,025 `--jobs` values
- **THEN** no snapshot is frozen, no request is sent, and the run's exit code is unchanged

#### Scenario: Long job name before job-name validation
- **WHEN** an upload-enabled run has a job whose name is 65 bytes
- **THEN** the record fails validation on that result's `name` and is not sent

#### Scenario: Oversized record
- **WHEN** an upload-enabled record would encode to more than 4,194,304 bytes
- **THEN** it fails validation at `body` and is not sent

### Requirement: Validation failures are reported structurally
A record that fails validation SHALL be reported with a `run_upload_skipped` event with `reason` `invalid_record` and the JSON path of the first violation. The event MUST NOT include any submitted value or unknown key name. The failure MUST NOT change the exit code or the local history file, and nothing SHALL be kept for replay.

#### Scenario: Invalid record event
- **WHEN** a record fails validation on `results[3].name` in `--json` mode
- **THEN** a `run_upload_skipped` event has `reason` `invalid_record` and `path` `results[3].name`, and contains no job name

#### Scenario: Unknown key path
- **WHEN** validation finds an unknown key inside `results[0]`
- **THEN** the reported `path` is `results[0]` and does not name the key

### Requirement: Stored runs can be listed
`goodflows history list` SHALL request `GET /api/v1/projects/<project>/workflow-runs` using the `[reporting]` project, URL variable, and key variable. It SHALL pass `--workflow`, `--environment`, `--outcome`, `--commit`, `--branch`, `--event`, `--graph-sha256`, `--limit`, and `--cursor` as the matching query parameters, and print the runs newest first as the server returns them.

#### Scenario: Filtered list
- **WHEN** the user runs `goodflows history list --workflow ci --branch main --event push`
- **THEN** one request is sent with `workflow=ci&branch=main&event=push` and each returned run is printed

#### Scenario: Next page
- **WHEN** the server returns a `next_cursor`
- **THEN** text output ends with the `--cursor` value for the next page, and `--json` output carries `next_cursor`

#### Scenario: Last page
- **WHEN** the server returns `next_cursor` null
- **THEN** text output has no next-page line and `--json` output has `next_cursor` null

### Requirement: A stored run can be shown
`goodflows history show <run_id>` SHALL request `GET /api/v1/projects/<project>/workflow-runs/<run_id>`. In text mode it SHALL print the run's fields, then one row per result, then its linked test reports. With `--json` it SHALL emit one `history_run` event holding the server's `data` unchanged.

#### Scenario: Show a run
- **WHEN** the user runs `goodflows history show <run_id>` for a stored run
- **THEN** the output shows its workflow, outcome, start, duration, results, and linked test reports

#### Scenario: Public key
- **WHEN** the configured key is a `pk_` key
- **THEN** the run is shown with the server's redacted `command`, `error`, and graph text, printed as absent

#### Scenario: Unknown run
- **WHEN** the server responds `404`
- **THEN** the command reports that the run was not found and exits 1

### Requirement: History read commands check input before sending
`history list` and `history show` SHALL check every filter, `limit`, `cursor`, and `run_id` against the server's rule before sending any request. A bad value MUST fail as an argument error with exit code 2 and no request. A missing `[reporting]` block, a missing key or URL, or an invalid URL MUST fail with exit code 1 and no request. `--env` MUST be rejected.

#### Scenario: Uppercase run id
- **WHEN** the user runs `goodflows history show` with a `run_id` containing uppercase hex
- **THEN** no request is sent and the command exits 2

#### Scenario: Invalid outcome filter
- **WHEN** the user runs `goodflows history list --outcome green`
- **THEN** no request is sent and the command exits 2

#### Scenario: No reporting block
- **WHEN** the config has no `[reporting]` block
- **THEN** `goodflows history list` exits 1 naming the missing block, and sends no request

### Requirement: History read output is safe and versioned
Responses SHALL be read up to a 16 MiB bound and MUST have the documented `data` shape. Otherwise the command fails with exit code 1. Non-2xx responses SHALL be reported by HTTP status only, never by body. Text output MUST escape control characters in server-provided strings. `--json` output SHALL be one versioned event per command.

#### Scenario: Control characters from the server
- **WHEN** a stored workflow name returned by the server contains an escape sequence
- **THEN** text output shows it escaped rather than writing it to the terminal

#### Scenario: Server error
- **WHEN** the server responds `503` with a body
- **THEN** the command reports HTTP 503 without the body and exits 1

#### Scenario: JSON list
- **WHEN** the user runs `goodflows history list --json`
- **THEN** stdout has exactly one `history_runs` event with `schema_version`, `runs`, and `next_cursor`
