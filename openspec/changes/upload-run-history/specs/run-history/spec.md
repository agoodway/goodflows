# Spec Delta

## MODIFIED Requirements

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

## ADDED Requirements

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
