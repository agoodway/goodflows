# Spec Delta

## ADDED Requirements

### Requirement: Runs upload reads a run folder
`goodflows cr runs upload <FOLDER>` SHALL read FOLDER's `scenarios.md`, the scenario set the run graded, and `run.md`. A missing `scenarios.md` MUST be refused with `<folder> has no scenarios.md, the copy of the scenario set the run graded`, and a missing `run.md` with `<folder> has no run.md`. `scenarios.md` MUST be UTF-8 and pass the qa-scenario-sets capability's front matter checks; its section headings are not checked.

#### Scenario: No scenarios.md
- **WHEN** `.verify/web/run-1` has `run.md` but no `scenarios.md`
- **THEN** the command exits 1 with `.verify/web/run-1 has no scenarios.md, the copy of the scenario set the run graded` and sends no request

### Requirement: The graded set's digest is the SHA-256 of scenarios.md
The document's `scenario_set` SHALL be `{digest, source}`, where `digest` is the lowercase hex SHA-256 of `scenarios.md`'s bytes and `source` its front matter's `source`. A digest stated in `run.md` MUST NOT replace it: when `set.sha256` is present and differs, the CLI MUST refuse the folder with `<folder>/run.md: set.sha256 is <stated>, but scenarios.md has SHA-256 <digest>; the run folder's copy of the set changed`.

#### Scenario: Digest from the file
- **WHEN** `scenarios.md` is an intent set and `run.md` has no `set`
- **THEN** the posted `scenario_set` is `{"digest": "<sha256 of scenarios.md>", "source": "intent"}`

#### Scenario: Copy changed after the run
- **WHEN** `run.md` has `set: {path: qa.md, sha256: 000…0}` (64 zeros)
- **THEN** the command exits 1 naming `copy of the set changed`, and no run is posted

### Requirement: run.md front matter is verify-run-notes/1
`run.md` MUST start with YAML front matter between `---` lines, else `<folder>/run.md must start with YAML front matter between --- lines`. The front matter MUST be a YAML mapping with `format: "verify-run-notes/1"`, else `<folder>/run.md: the front matter needs format: "verify-run-notes/1"`. YAML anchors and aliases SHALL be refused with `<folder>/run.md: YAML anchors and aliases are not supported`.

#### Scenario: Alias in drift
- **WHEN** the front matter has `drift: [&d "R&D", *d]`
- **THEN** the command exits 1 with `run.md: YAML anchors and aliases are not supported`

### Requirement: run.md needs commit, dirty, started_at, finished_at and results
The front matter MUST have non-null `commit`, `dirty`, `started_at`, `finished_at` and `results`, each missing one refused with `<folder>/run.md: the front matter needs <key>`. `branch` is optional: absent, null or empty it is sent as null; otherwise it MUST be at most 256 bytes without control characters.

#### Scenario: No branch
- **WHEN** `run.md` has every required key and no `branch`
- **THEN** the posted `source.branch` is null and the upload proceeds

#### Scenario: No dirty
- **WHEN** `run.md` has no `dirty`
- **THEN** the command exits 1 with `run.md: the front matter needs dirty`

### Requirement: Commit, dirty and times have exact shapes
`commit` MUST be 40 or 64 lowercase hex characters (`commit must be the full commit SHA in lowercase hex`), `dirty` MUST be `true` or `false` (`dirty must be true or false`), and `started_at` and `finished_at` MUST be `YYYY-MM-DDTHH:MM:SS`, optionally `.` and 1 to 9 digits, then `Z` or `±HH:MM` (`<key> must be an RFC 3339 time such as 2026-11-19T14:30:00Z`). Times SHALL be sent as written.

#### Scenario: Short commit
- **WHEN** `commit` is `a1b2c3d`
- **THEN** the command exits 1 with `commit must be the full commit SHA in lowercase hex`

#### Scenario: Times
- **WHEN** `started_at` is `2026-11-19T14:30:00.123+02:00`
- **THEN** it is accepted, and `2026-11-19T14:30:00`, `2026-11-19T14:30:00.Z`, `2026-11-19 14:30:00Z` and `2026-11-19T14:30:00+0200` are refused

### Requirement: Results hold exactly the scenarios of the set
`results` MUST be a list with one entry per scenario of `scenarios.md`, IDs compared as strings, at most 200 entries. An entry without an ID, an ID listed twice, or a set whose IDs differ SHALL be refused, the last with `<folder>/run.md: results must hold exactly the scenarios of scenarios.md (missing: <ids or none>; not in the set: <ids or none>)`.

#### Scenario: A result missing
- **WHEN** the set has scenarios 1 and 2 and `results` has only `{id: 1, result: passed}`
- **THEN** the command exits 1 and its error contains `missing: 2`, before any request

#### Scenario: Integer and string IDs match
- **WHEN** the set's scenario has `id: 1` and the result has `id: "1"`
- **THEN** the result is accepted and sent with `id` `"1"`

### Requirement: Each result has an outcome and bounded texts
Each result's `result` MUST be `passed`, `failed` or `not_run`. `failed_step` and `reason` SHALL be sent as given when nonempty strings and as null otherwise, and each MUST be at most 4096 bytes. Each result SHALL be sent as `{id, result, failed_step, reason, evidence}`, `evidence` being artifact IDs.

#### Scenario: Not run with a reason
- **WHEN** a result is `{id: 2, result: not_run, reason: No seed data}`
- **THEN** it is sent as `{"id": "2", "result": "not_run", "failed_step": null, "reason": "No seed data", "evidence": []}`

#### Scenario: Skipped is not an outcome
- **WHEN** a result has `result: skipped`
- **THEN** the command exits 1 with `scenario <id>: result must be passed, failed or not_run`

### Requirement: Evidence paths stay inside the run folder
Each `evidence` entry MUST be a string path. A path starting `.verify/<app>/<run-id>/` SHALL be read relative to the run folder after those three parts; any other path is already relative to it. The relative path MUST be nonempty with only normal parts, else `evidence <path> must be a path inside the run folder`.

#### Scenario: Prefixed path
- **WHEN** evidence is `.verify/web/20261119T143000Z-a1b2c3d/shots/01.png`
- **THEN** the artifact's `path` is `shots/01.png`

#### Scenario: Escaping paths
- **WHEN** evidence is `../secret`, `/etc/passwd`, `a/../../b`, `""` or `.`
- **THEN** each is refused as not inside the run folder

### Requirement: Evidence files are regular files of 1 byte to 50 MiB
Each evidence file MUST exist (`evidence file <file> does not exist`), be a regular file and not a symbolic link (`evidence <file> is not a regular file`), hold 1 byte to 50 MiB (`evidence file <file> must hold 1 byte to 50 MiB`), and have a relative path of at most 1024 bytes without control characters. A run MUST name at most 200 distinct files.

#### Scenario: Empty evidence
- **WHEN** a result names `shots/empty.png`, a 0-byte file
- **THEN** the command exits 1 with `must hold 1 byte to 50 MiB`

### Requirement: Each evidence file is one artifact
Each distinct evidence path SHALL be one artifact `{id, path, content_type, size, sha256}`, a path named twice reusing its ID. The ID MUST be the path with characters other than ASCII letters, digits, `.`, `_` and `-` replaced by `-`, prefixed `a` unless it starts with a letter or digit, cut to 60 characters, and suffixed `-2`, `-3`… when taken. `content_type` SHALL follow the extension, `application/octet-stream` when unknown.

#### Scenario: Screenshot
- **WHEN** evidence `shots/01.png` holds the 9 bytes `png bytes`
- **THEN** the artifact is `{"id": "shots-01.png", "path": "shots/01.png", "content_type": "image/png", "size": 9, "sha256": "<sha256 of png bytes>"}` and the result's `evidence` is `["shots-01.png"]`

#### Scenario: Colliding IDs
- **WHEN** evidence names `a/b.png` and then `a-b.png`
- **THEN** their IDs are `a-b.png` and `a-b.png-2`

### Requirement: Optional lists are checked against the server's limits
`changed_files` SHALL be relative paths of 1 to 4096 bytes without control characters, at most 10000. `drift` SHALL be strings of at most 1024 bytes, at most 100. `features` SHALL be mappings with a nonblank string `id` of at most 64 bytes, an optional string `entry_point` of at most 1024 bytes and a `result` of `passed`, `failed` or `not_run`, at most 500. Absent lists SHALL be sent empty.

#### Scenario: Absolute changed file
- **WHEN** `changed_files` is `[src/a.rs, /etc/passwd]`
- **THEN** the command exits 1 naming `changed_files[1]`

#### Scenario: Bad feature result
- **WHEN** `features` is `[{id: sign-in, result: skipped}]`
- **THEN** the command exits 1 with `run.md: features[0] (sign-in): result must be passed, failed or not_run`

### Requirement: Features keep only id, entry_point and result
Each feature SHALL be sent as exactly `{id, entry_point, result}`, `entry_point` null when absent, and other keys MUST be dropped.

#### Scenario: Extra keys dropped
- **WHEN** a feature is `{id: sign-in, entry_point: /login, result: failed, notes: flaky, owner: web}`
- **THEN** it is sent as `{"id": "sign-in", "entry_point": "/login", "result": "failed"}`

#### Scenario: No entry point
- **WHEN** a feature is `{id: sign-in, result: passed}`
- **THEN** it is sent with `entry_point` null

### Requirement: The run ID derives from the folder unless run.md names one
Without `run_id`, the CLI SHALL use the UUID v5, in namespace `0b6f1c2e-6a8d-5c3e-9f41-7d2a4e8b9c10`, of the run folder's resolved directory name, a newline and `commit`, so uploading the same folder again names the same run. A given `run_id` MUST be a lowercase canonical UUID, else `<folder>/run.md: run_id must be a lowercase canonical UUID`.

#### Scenario: Same folder, same run
- **WHEN** the same run folder is uploaded twice
- **THEN** both uploads send the same `run_id`

### Requirement: merge_result, links and workflow_run_id pass through
When the front matter has `merge_result`, `links` or `workflow_run_id`, the CLI SHALL copy each value into the document unchecked, and MUST NOT add a key the front matter lacks.

#### Scenario: Links given
- **WHEN** `run.md` has `links: {tickets: [GOO-291]}`
- **THEN** the posted document has `links` `{"tickets": ["GOO-291"]}`

### Requirement: Every problem in a run folder is listed before any request
The CLI SHALL check `scenarios.md` and `run.md` fully and MUST print every problem, one per line, each naming its file, then exit 1 without sending any request.

#### Scenario: Two problems
- **WHEN** `run.md` has `commit: abc` and no `finished_at`
- **THEN** the error lists both `commit must be the full commit SHA in lowercase hex` and `the front matter needs finished_at`, and nothing is posted

### Requirement: Runs upload posts a verify-run/1 document
The CLI SHALL post `{format: "verify-run/1", run_id, started_at, finished_at, source: {commit, branch, dirty, changed_files}, scenario_set, results, features, drift, artifacts}` plus pass-through keys to `POST /api/v1/projects/{project}/verify-runs`, with the reporting config and key the change-requests capability specifies. A run belongs to the project, not a change request.

#### Scenario: Document shape
- **WHEN** `run.md` has `commit: <HEAD>`, `branch: feat/login`, `dirty: false`
- **THEN** the posted `source` is `{"commit": "<HEAD>", "branch": "feat/login", "dirty": false, "changed_files": []}`

### Requirement: Evidence goes to the URL the server returns
When the response's `data.status` is `uploading`, the CLI SHALL, for each artifact, find the entry of `data.uploads` (else `data.artifacts`) with its `artifact_id` and `PUT` the file's bytes to its `upload_url` with its `upload_headers`. It MUST NOT send the API key and MUST NOT follow redirects. A missing entry, a URL not `http` or `https`, or a header that is not a string MUST fail with `runs upload failed: the server returned invalid upload URLs`.

#### Scenario: Presigned upload
- **WHEN** the server returns `upload_url` `<base>/bucket/obj-1?sig=x` and `upload_headers` `{"content-type": "image/png", "x-amz-checksum-sha256": "abc"}`
- **THEN** the CLI sends `PUT /bucket/obj-1?sig=x` with body `png bytes`, header `x-amz-checksum-sha256: abc`, and no `authorization` header

### Requirement: Evidence that changed or was refused stops the upload
Before each `PUT` the CLI SHALL read the file again and MUST fail with `<path> changed while it was uploaded` when its SHA-256 differs from the one posted. A response other than `2xx` MUST fail with `runs upload failed: the object store refused <artifact id> (HTTP <status>)`, and no response with `runs upload failed: could not reach the object store`. The run is not committed.

#### Scenario: Object store refuses
- **WHEN** the `PUT` for `shots-01.png` answers `403`
- **THEN** the command exits 1 with `runs upload failed: the object store refused shots-01.png (HTTP 403)` and sends no commit

### Requirement: The run is committed after its evidence
After every artifact is uploaded, the CLI SHALL `POST /api/v1/projects/{project}/verify-runs/{run_id}/commit` and report the committed run's `status`. When the upload response's `status` is not `uploading`, such as a run already stored as `final`, it MUST upload no evidence and send no commit.

#### Scenario: Upload then commit
- **WHEN** the server answers the upload with `status` `uploading` and the commit with `status` `final`
- **THEN** the requests are the run, the `PUT`, then the commit, and the event's `status` is `final`

#### Scenario: Already final
- **WHEN** the server answers the upload `200` with `status` `final`
- **THEN** no `PUT` or commit is sent, and the event has `duplicate` true

### Requirement: A commit answered 503 is retried up to 5 times
When the commit answers `503`, the CLI SHALL wait the response's `retry-after` seconds, 1 when absent or not an integer, at most 10, and send it again, up to 5 retries. After the fifth retry the `503` MUST be the failure, exit code 1, with a message containing `HTTP 503`. Any other failure MUST NOT be retried.

#### Scenario: Store comes back
- **WHEN** the first two commits answer `503` with `retry-after: 0` and the third answers `200` with `status` `final`
- **THEN** the command exits 0 after 3 commit requests with `status` `final`

#### Scenario: Store stays down
- **WHEN** every commit answers `503` with `retry-after: 0`
- **THEN** the command sends 6 commit requests, one try and five retries, and exits 1 naming `HTTP 503`

#### Scenario: Long retry-after
- **WHEN** a commit answers `503` with `retry-after: 120`
- **THEN** the CLI waits 10 seconds, not 120, before the next try

### Requirement: An interrupt stops the upload
On SIGINT or SIGTERM during an evidence `PUT` or a wait between commit retries, the CLI SHALL stop at once without sending further artifacts or commits, fail with `runs upload interrupted`, and exit with 128 plus the signal number, 130 for SIGINT.

#### Scenario: Ctrl-C during evidence
- **WHEN** the user presses Ctrl-C while the first of three artifacts is being sent
- **THEN** the other two are not sent, no commit is sent, and the command exits 130

### Requirement: Runs upload prints the run and the verdict
On success the CLI SHALL emit a `verify_run` event with `folder`, `run_id`, `status`, `duplicate` (server answered `200`), `scenario_set`, `commit`, `dirty`, `results` counts, `artifacts` count, `number` and `verdict`. It SHALL read the verdict of `--number`, else of the current branch's one draft or open change request; with neither, `number` and `verdict` are null.

#### Scenario: Text output
- **WHEN** a run with 1 passed and 1 failed result and 1 artifact is committed as `final` for change request 1
- **THEN** the text starts `Verify run <run_id>: final  passed 1, failed 1, not run 0; 1 artifacts`, then `  commit <first 12 of commit>  intent set <digest>`, then `Change request #1: ` and the verdict

#### Scenario: Dirty run
- **WHEN** `run.md` has `dirty: true`
- **THEN** the first line ends `; dirty tree, so it never counts`
