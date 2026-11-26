# change-requests Specification

## Purpose

Defines the rules every change request, policy, feature index and events command shares (reporting config, keys, picking the change request, JSON events, error messages and exit codes), the claims file and Git facts the CLI reports, and the `cr` commands that open, update, read, wait on and move a change request, including `cr validate`.

## Requirements

### Requirement: Commands use the reporting project, URL and key
Every `cr`, `policy`, `features` and `events` command that talks to the server SHALL use the config's top-level `[reporting]` block, as `history` does: its `project`, the URL in the variable named by `url_env` and the key in the variable named by `token_env` (`GOODFLOWS_URL` and `GOODFLOWS_API_KEY` by default). Each request SHALL go to `/api/v1/projects/<project>/...` with each path segment escaped and the key sent as `Authorization: Bearer <key>`.

#### Scenario: Key and path
- **WHEN** `[reporting]` names project `goodcases`, `GOODFLOWS_API_KEY` is `sk_test` and `goodflows cr ready -n 4` runs
- **THEN** it sends `POST /api/v1/projects/goodcases/change-requests/4/ready` with header `authorization: Bearer sk_test`

#### Scenario: A branch name is escaped
- **WHEN** the current branch is `feat/login` and a command looks up its change request
- **THEN** the query string carries `source_branch=feat%2Flogin`

### Requirement: Missing reporting settings fail before any request
A command that talks to the server MUST fail with exit code 1 and send nothing when the config has no `[reporting]` block (`cr <cmd> requires a top-level [reporting] block naming the project; ...`), when the URL or key variable is unset or empty (`cr <cmd> requires GOODFLOWS_URL and GOODFLOWS_API_KEY to be set (reporting url_env and token_env)`), or when the URL is not origin-only `https` (`http` only for loopback).

#### Scenario: No key
- **WHEN** `[reporting]` is set but `GOODFLOWS_URL` and `GOODFLOWS_API_KEY` are unset and `goodflows cr status -n 4` runs
- **THEN** it exits 1 and stderr names `GOODFLOWS_URL and GOODFLOWS_API_KEY`

#### Scenario: Plain http to a remote host
- **WHEN** `GOODFLOWS_URL` is `http://x.example`
- **THEN** the command exits 1 with `GOODFLOWS_URL must be an origin-only https URL (http only for loopback)`

#### Scenario: No reporting block
- **WHEN** the config has jobs but no `[reporting]` block and `goodflows cr status -n 1` runs
- **THEN** it exits 1 with `cr status requires a top-level [reporting] block naming the project; add [reporting] with project = "<slug>" to the config`

### Requirement: Writes need an sk_ key and the CLI leaves key checks to the server
The CLI SHALL send whatever key the variable holds and SHALL NOT check its prefix; writes need an `sk_` key, and the server refuses other keys. A refusal SHALL surface as an ordinary server error, such as `cr ready failed: forbidden (HTTP 403)` followed by the server's body.

#### Scenario: A pk_ key writes
- **WHEN** `GOODFLOWS_API_KEY` holds a `pk_` key and `goodflows cr ready -n 4` runs and the server answers `403` with an empty body
- **THEN** the request was sent, and the command exits 1 with `cr ready failed: forbidden (HTTP 403)`

### Requirement: The API key never reaches git
Every git process a change request command starts, including `git push` from `cr push` and `git fetch`, SHALL run with the key variable (`token_env`) removed from its environment, so hooks and credential helpers never see it.

#### Scenario: A pre-push hook reads the environment
- **WHEN** `GOODFLOWS_API_KEY` is `sk_test` and `goodflows cr push` runs a repository whose pre-push hook prints `$GOODFLOWS_API_KEY`
- **THEN** the hook prints an empty value

### Requirement: Commands pick the current branch's draft or open change request
A command taking `-n/--number <N>` SHALL use that number, which MUST be at least 1. Without it, the command SHALL list change requests with `state=draft`, then `state=open`, `source_branch=<current branch>` and `limit=200`, following `next_cursor` until it is null, and use the only one found, ordered by number.

#### Scenario: Lookup requests
- **WHEN** `goodflows cr status` runs on branch `feat/login`
- **THEN** it first sends `GET .../change-requests?state=draft&source_branch=feat%2Flogin&limit=200`, then the same with `state=open`, then reads the one change request found

#### Scenario: Number zero
- **WHEN** `goodflows cr status -n 0` runs
- **THEN** it exits 2 with `invalid value '0' for '--number <N>'` and sends nothing

### Requirement: Picking fails clearly when the branch does not name one change request
Without `--number`, a command MUST exit 1 with `branch <b> has no draft or open change request; open one with goodflows cr open, or pass --number` when none is found, `branch <b> has several draft or open change requests (#3, #5); pass --number` when several are, and `HEAD is detached; check out the change request's branch or pass --number` on a detached HEAD.

#### Scenario: Two change requests on the branch
- **WHEN** branch `feat/login` has draft `#3` and open `#5` and `goodflows cr verdict` runs
- **THEN** it exits 1 and the message contains `(#3, #5)`

### Requirement: JSON output is one event
With `--json`, a command SHALL print exactly one JSON object on one stdout line: its result event with `type` and `schema_version` `1`, or on failure an `error` event with `code` and `message`. Text output SHALL go to stdout for results and to stderr, prefixed `Error: `, for failures. Values from the server or a file SHALL be printed with control characters escaped.

#### Scenario: Failure as JSON
- **WHEN** `goodflows --json cr status -n 3` cannot reach the server
- **THEN** stdout is `{"code":"runtime","message":"cr status failed: could not reach the server","schema_version":1,"type":"error"}` and it exits 1

### Requirement: Server errors name the command, status and bounded body
A non-`2xx` response MUST fail with `<command> failed: <reason> (HTTP <code>)`, where the reason is `redirect not followed` (3xx), `bad request`, `unauthorized`, `forbidden`, `not found`, `not acceptable`, `conflict`, `gone`, `body too large`, `unsupported media type`, `request rejected` (422), `server error` (5xx) or `unexpected response`. Redirects SHALL NOT be followed. The URL and key MUST never appear.

#### Scenario: A conflict
- **WHEN** `goodflows cr ready -n 4 --json` gets `409` with body `{"errors":{"detail":"invalid transition"}}`
- **THEN** it exits 1 and the event's `message` is `cr ready failed: conflict (HTTP 409): {"errors":{"detail":"invalid transition"}}`

#### Scenario: Empty body
- **WHEN** `cr ready` gets `404` with an empty body
- **THEN** the message is `cr ready failed: not found (HTTP 404)` with nothing after it

### Requirement: Quoted server bodies are escaped and bounded
When a failed response has a JSON body, the message SHALL append `: ` and the body re-serialized as compact JSON, cut to at most 2048 bytes on a character boundary with `…` added when cut, and with control characters escaped. A non-JSON body SHALL be left out. A single server value quoted in a rewritten message SHALL be cut to 256 bytes and escaped, and a null value SHALL read `unknown`.

#### Scenario: Escape sequence in the body
- **WHEN** `cr claims` gets `422` with `{"errors":{"head":["does not match\u001b[2J"]}}`
- **THEN** the message is `cr claims failed: request rejected (HTTP 422): {"errors":{"head":["does not match\u001b[2J"]}}`, with the escape printed as `\u001b`, not sent raw

#### Scenario: Long body
- **WHEN** a `409` body is a JSON string of 3000 `é` characters
- **THEN** the message ends with `…` and is shorter than 2148 bytes

### Requirement: Transport failures have fixed messages
A command MUST fail with exit code 1 and `<command> failed: timed out` after 3 seconds to connect or 120 seconds for the exchange, `<command> failed: could not reach the server`, `<command> failed: the response exceeds 16 MiB`, `<command> failed: the server's response is not JSON` for a non-JSON `2xx` body, or `failed: the server's response has no data` when a `2xx` body has no non-null `data`. No request SHALL be retried.

#### Scenario: Unreachable server
- **WHEN** `GOODFLOWS_URL` is `https://127.0.0.1:1` and `goodflows cr status -n 3` runs
- **THEN** it exits 1 with `cr status failed: could not reach the server`

### Requirement: Known 409 conflicts are rewritten into next steps
A `409` whose body's `errors` the command recognizes SHALL replace the generic message with one saying what to do; any other `409` SHALL keep the generic message. Git facts with `errors.detail` `Different Git facts are stored for this head and target` and claims with `Different claims are stored for this head` SHALL be rewritten, naming the first 12 characters of HEAD.

#### Scenario: Moved merge base
- **WHEN** `cr claims --no-fetch` posts Git facts and gets `409` with `{"errors":{"detail":"Different Git facts are stored for this head and target"}}`
- **THEN** it exits 1 with `cr claims failed: Git facts for HEAD <12 chars> against main were already reported with a different merge base; make a new commit (or retarget) and report again`

#### Scenario: Different claims for the head
- **WHEN** the claims upload gets `409` with `errors.detail` `Different claims are stored for this head`
- **THEN** the message is `<command> failed: different claims are already stored for HEAD <12 chars>; claims are fixed per head, so commit the change, update the file's head to the new HEAD, and upload again`

### Requirement: Exit codes
A command SHALL exit 0 on success, 2 for a usage or config error (bad arguments, no `goodflows.toml`, a run-only flag such as `--env`), 1 for any other failure, `cr wait` SHALL exit 75 when its timeout passes, and a command interrupted by a signal SHALL exit 128 plus the signal number.

#### Scenario: A run flag
- **WHEN** `goodflows cr status --env x` runs
- **THEN** it exits 2 with `--env cannot be used with cr commands`

#### Scenario: Missing required argument
- **WHEN** `goodflows cr wait` runs without `--until`
- **THEN** it exits 2 and sends nothing

### Requirement: Git facts fetch the target first
Before computing Git facts, `cr open`, `cr claims` and `cr push` SHALL run `git fetch --quiet origin +refs/heads/<target>:refs/remotes/origin/<target>` with `GIT_TERMINAL_PROMPT=0` when the checkout has an `origin` remote, and skip it with `--no-fetch` or without `origin`. A failed fetch MUST exit 1 with `git fetch origin <target> failed: <stderr>; pass --no-fetch to use the refs this checkout has`, before any facts are sent.

#### Scenario: Unreachable origin
- **WHEN** `origin` is `/nonexistent/goodflows-origin` and `goodflows cr claims cr.md` runs
- **THEN** it exits 1 naming `git fetch origin main failed` and `pass --no-fetch`, and sent only `GET` requests; with `--no-fetch` the same command succeeds

#### Scenario: Single-branch clone
- **WHEN** the clone's fetch refspec covers only `feat/login` and there is no local `main`
- **THEN** `cr claims` still fetches `main` into `origin/main` and reports the merge base with it

### Requirement: Git facts describe HEAD against the target
The CLI SHALL report a `git-facts/1` document to `POST .../change-requests/<N>/git-facts` with `head`, `target` (`branch`, `merge_base`), `commits` from the merge base to HEAD oldest first (`sha`, `parents`, `changed_files` against the first parent) and `diff.files` from the merge base to HEAD. The target commit SHALL be `origin/<target>`, else the local `<target>`.

#### Scenario: Two commits with a rename
- **WHEN** `feat/login` adds `lib/login.ex` (2 lines) in one commit and renames `old.txt` to `docs/new.txt` in the next
- **THEN** `diff.files` is `docs/new.txt` `renamed` with `old_path` `old.txt` and 0/0 counts, then `lib/login.ex` `added` with `additions` 2, and `commits` lists both commits with their parents

### Requirement: Git facts file entries
Each file entry SHALL have `path`, `status` (`added`, `modified`, `deleted`, `renamed`, `copied` or `type_changed`), `additions` and `deletions`, and `old_path` for renames and copies, from `git diff -M` without external diff or textconv. A binary file SHALL count 0 additions and 0 deletions.

#### Scenario: Binary image
- **WHEN** a commit adds `img.png`
- **THEN** its entry is `{"path": "img.png", "status": "added", "additions": 0, "deletions": 0}`

### Requirement: Git facts limits and local failures
The CLI MUST refuse to report more than 1000 commits after the merge base (`HEAD is <n> commits ahead of its merge base with <target>; git-facts/1 allows at most 1000`) or more than 10000 changed files, and MUST fail with `the target branch <t> is not in this checkout as origin/<t> or <t>; fetch it first` or `HEAD has no merge base with <t>`, sending nothing.

#### Scenario: Target missing
- **WHEN** neither `origin/release` nor `release` exists and `cr claims --no-fetch` runs for a change request targeting `release`
- **THEN** it exits 1 with `the target branch release is not in this checkout as origin/release or release; fetch it first` and posts no facts

### Requirement: Git facts report reported or unchanged
A Git facts report SHALL be summarized as `git_facts` with `head`, `target`, the number of `commits` and `files`, and `status` `reported` when the server answered `201` or `unchanged` otherwise. Text output SHALL print `Git facts: <status> for <12-char head> (<n> commits, <n> files against <target>)`.

#### Scenario: Reported again
- **WHEN** `cr open` reports facts the server already has and it answers `200`
- **THEN** `git_facts.status` is `unchanged` and the text line starts `Git facts: unchanged`

### Requirement: The default claims file follows the branch
The default claims file SHALL be `.goodflows/change-requests/<branch>/cr.md` under the checkout root, each `/` in the branch a directory, or `.goodflows/change-requests/.detached/<short SHA>/cr.md` on a detached HEAD, and the default diff set SHALL be `qa-scenarios.md` in the same directory.

#### Scenario: Branch with a slash
- **WHEN** the branch is `feat/login`
- **THEN** the default claims file is `.goodflows/change-requests/feat/login/cr.md` and the diff set `.goodflows/change-requests/feat/login/qa-scenarios.md`

#### Scenario: Detached HEAD
- **WHEN** HEAD is detached at short SHA `a1b2c3d`
- **THEN** the default claims file is `.goodflows/change-requests/.detached/a1b2c3d/cr.md`

### Requirement: cr path prints the default paths without a server
`cr path` SHALL send no request and need no API key. Text output SHALL print the absolute claims path. `--json` SHALL print `change_request_path` with `path` (relative), `absolute_path`, `branch` (null when detached), `exists`, `scenarios_path`, `scenarios_absolute_path` and `scenarios_exists`. It still loads the config, so without one it exits 2.

#### Scenario: JSON before the file exists
- **WHEN** `goodflows cr path --json` runs on `feat/verify` with no claims file
- **THEN** `.path` is `.goodflows/change-requests/feat/verify/cr.md`, `.exists` false and `.scenarios_path` `.goodflows/change-requests/feat/verify/qa-scenarios.md`

### Requirement: Claims files are change-claims/2 with cr and head
The CLI SHALL read a claims file as YAML front matter between a first line `---` and the next `---` line, then a body. It MUST refuse a file without front matter, invalid YAML, YAML anchors, aliases or tags, `format` other than `change-claims/2`, `cr` other than null or a positive integer, or a missing string `head`, listing every such problem at once. Other fields are left to the server.

#### Scenario: Three problems at once
- **WHEN** `cr.md` has front matter `format: change-claims/1` and `cr: 0` and no `head`
- **THEN** the problems are `<file>: format must be change-claims/2`, `<file>: cr must be a change request number or null` and `<file>: head must be the commit the claims describe`

#### Scenario: Ampersand in plain text
- **WHEN** `cr.md` has `reach: Login & signup` and `door_reason: |` followed by the line `  *Not* reversible`
- **THEN** no anchor or alias problem is reported, because `&` and `*` count as an anchor or alias only where a node starts and a name follows

#### Scenario: Alias
- **WHEN** `cr.md` has `reach: *r`
- **THEN** the problems include `<file>: YAML anchors and aliases are not supported`

### Requirement: The claims title is the first level-1 heading
A new change request's title SHALL come from the claims body's first line starting `# `, trimmed, and an empty heading SHALL count as none. Lines starting `## ` SHALL NOT count.

#### Scenario: Heading after a subheading
- **WHEN** the body is `## Notes` then `# Add login`
- **THEN** the title is `Add login`

### Requirement: Claims must describe HEAD
`cr open` with a claims file and `cr claims` MUST refuse a file whose `head` is not the full SHA of HEAD, with `<file> describes <head> but HEAD is <sha>; update the claims for HEAD first`, before sending any request.

#### Scenario: Stale head
- **WHEN** `cr.md` has `cr: 7` and `head` is 40 `a` characters and `goodflows cr claims cr.md` runs
- **THEN** it exits 1, stderr contains `but HEAD is`, and the server received no request

### Requirement: The change request number is written into cr
When a claims file's `cr` is null, the CLI SHALL replace the first front matter line starting `cr:` with `cr: <N>`, keeping its line ending and every other byte, write the file before uploading, and upload the file as written. A file whose `cr` names another number MUST be refused with `<file> names change request <n>, not <N>`.

#### Scenario: Number written before upload
- **WHEN** `cr open` uploads a file with `cr: null` for new change request `7`
- **THEN** the file afterwards equals the original with `cr: null` replaced by `cr: 7`, and the claims request body is exactly the new file

### Requirement: Claims are uploaded as Markdown
The claims upload SHALL be `POST .../change-requests/<N>/claims` with the file's bytes and `Content-Type: text/markdown; charset=utf-8`. Its summary `claims` SHALL have `file`, `head`, `status` `uploaded` on `201` or `unchanged` otherwise, and `wrote_cr`. Text SHALL print `Claims: <status> from <file>`, adding ` (wrote cr: <N> into it)` when it wrote the number.

#### Scenario: Header and status
- **WHEN** the server answers the claims upload with `201`
- **THEN** the request had `content-type` `text/markdown; charset=utf-8` and `claims.status` is `uploaded`

### Requirement: cr open creates or reuses the branch's change request
`cr open` SHALL need a checked-out branch (`HEAD is detached; cr open needs the change request's branch checked out`) of 1-256 bytes without control characters. It SHALL reuse the branch's one draft or open change request, or create a draft with `POST .../change-requests` and body `title`, `source_branch`, and `openspec_change` and `target_branch` when given.

#### Scenario: Create from a claims file
- **WHEN** branch `feat/login` has no change request and the default claims file has `# Add login`
- **THEN** the create body is `{"title": "Add login", "source_branch": "feat/login"}` and the event has `created` true

#### Scenario: Reuse
- **WHEN** the branch has draft `#3` linked to `add-login` and `cr open --openspec-change add-login --json` runs
- **THEN** only `GET` requests are sent and the event has `created` false and number `3`

### Requirement: cr open with a claims file reports facts and uploads claims
When the claims file exists (`--claims <file>`, which must exist, or the default file when it exists), `cr open` SHALL check its head before any request, then report Git facts, write `cr`, and upload the claims. The event SHALL carry `git_facts`, `claims`, and `verdict` and `change_request` from the claims response.

#### Scenario: Request order
- **WHEN** `cr open --json` creates change request `7` from the default claims file
- **THEN** the requests are the draft and open lookups, `POST .../change-requests`, `POST .../7/git-facts` and `POST .../7/claims`, and the event's `verdict.state` is the one the claims response returned

#### Scenario: Missing named file
- **WHEN** `cr open --claims nope.md` runs and `nope.md` does not exist
- **THEN** it exits 1 with `the claims file nope.md does not exist`

### Requirement: cr open without a claims file needs a title
Without a claims file, `cr open` SHALL create the change request from `--title` and send no facts or claims; the event's `claims` SHALL be `{"file": <path>, "status": "missing"}` and text SHALL print `Claims: no file at <path>; when it exists, goodflows cr claims uploads it`. Without `--title` it MUST exit 1 with `there is no claims file at <path>; pass --title to open a change request without one`.

#### Scenario: Before code
- **WHEN** `cr open --openspec-change add-login --title "Add login" --json` runs with no claims file
- **THEN** exactly three requests are sent, the create body is `{"title": "Add login", "source_branch": "feat/login", "openspec_change": "add-login"}`, and `claims.status` is `missing`

### Requirement: cr open refuses ambiguous or mismatched change requests
`cr open` MUST exit 1 when the branch has several draft or open change requests (`... (#3, #5); close the extra ones or use cr claims with the file's cr`), when the reused one links another OpenSpec change (`change request #3 of this branch links OpenSpec change add-login, not add-export; the link cannot be changed`), or when the file's `cr` names a number the branch does not have.

#### Scenario: Different OpenSpec change
- **WHEN** the branch's draft `#3` links `add-login` and `cr open --openspec-change add-export` runs
- **THEN** it exits 1 with a message containing `links OpenSpec change add-login, not add-export`

#### Scenario: cr names a missing change request
- **WHEN** the branch has no draft or open change request and the claims file has `cr: 9`
- **THEN** it exits 1 with `<file> names change request 9, which is not a draft or open change request of branch feat/login; reopen it, or set cr: null to open a new one` and creates nothing

### Requirement: cr open option values are checked
`--openspec-change` MUST be 1-64 characters of `a-z`, `0-9` and `-` starting with a letter or digit, else exit 2. A title MUST be 1-256 bytes without control characters. A claims file without a `#` heading and no `--title` MUST fail with `<file> has no # heading for the title; pass --title`. `--title` SHALL win over the heading, and `--title`, `--target` are used only when creating.

#### Scenario: Bad change id
- **WHEN** `cr open --openspec-change Add_Login` runs
- **THEN** it exits 2 with `must be 1-64 characters of a-z, 0-9 and -, starting with a letter or digit`

### Requirement: cr claims updates a change request for HEAD
`cr claims [<file>]` SHALL read the file (default: the branch's claims file), which MUST exist (`the claims file <f> does not exist; write it first (gf-change-request)`), check its head, then read the change request, report Git facts against its target branch and upload the claims. The number SHALL be the file's `cr`, else `--number`, else the branch's change request.

#### Scenario: Number conflict
- **WHEN** the file has `cr: 7` and `cr claims -n 8` runs
- **THEN** it exits 1 with `<file> names change request 7, not 8` and sends nothing

#### Scenario: Text output
- **WHEN** `goodflows cr claims` uploads for change request 7 on `feat/login`
- **THEN** stdout contains `#7 draft  feat/login -> main  Add login`, `Claims: uploaded from` and a `Verdict:` line

### Requirement: cr push pushes then reports facts
`cr push` SHALL first find the change request, so nothing is pushed without one, then run `git push` with the arguments after `--`, with git's stdout and stderr both sent to stderr, then report Git facts for the new HEAD. A failed push MUST exit 1 with `git push failed` and report nothing. It SHALL NOT upload claims.

#### Scenario: Push with arguments
- **WHEN** `goodflows cr push --json -- -u origin HEAD` runs
- **THEN** it runs `git push -u origin HEAD`, stdout is one JSON line with `action` `push`, `git_facts` and `verdict`, and git's `[new branch]` line is on stderr

### Requirement: cr status shows the record and its history
`cr status` SHALL read `GET .../change-requests/<N>` and print `change_request` with `action` `status`. Text SHALL print `#<N> <state>  <source> -> <target>  <title>`, a line with `head <12 chars>` and `OpenSpec <id>` and `parent #<n>` when set, the merge line when merged, the verdict, and one `  <inserted_at>  <from> -> <to>  <user_id>` line per history entry.

#### Scenario: History lines
- **WHEN** the change request went draft to open and back to draft
- **THEN** the text lists `draft -> open` and then `open -> draft`

### Requirement: cr verdict shows the verdict and a stale local head
`cr verdict` SHALL read `GET .../change-requests/<N>/verdict` and print `change_request_verdict` with `number`, `verdict` and `local_head`. Text SHALL print `Verdict: <state>  (head <12 chars>, policy v<n>)` and one line per reason, and SHALL add `Local HEAD <12 chars> is not the reported head; run goodflows cr push or goodflows cr claims.` when HEAD differs from the verdict's head.

#### Scenario: New local commit
- **WHEN** the verdict is for head `aaaa…` and local HEAD is `bbbb…`
- **THEN** the text includes `Local HEAD bbbbbbbbbbbb is not the reported head`

### Requirement: cr wait polls until a wanted state
`cr wait --until <states>` (required; `pending`, `blocked`, `needs_review`, `mergeable`, comma-separated or repeated) SHALL read the verdict every `--interval` seconds (1-3600, default 10) and exit 0 once its state is one of them, printing one `change_request_verdict` event with `until` and `timed_out` false.

#### Scenario: Reached
- **WHEN** `cr wait --until mergeable,needs_review` runs and the second read returns `needs_review`
- **THEN** it exits 0 after two reads and prints one event

#### Scenario: Interval out of range
- **WHEN** `cr wait --until mergeable --interval 0` runs
- **THEN** it exits 2 with `invalid value '0' for '--interval <SECS>'`

### Requirement: cr wait exits 75 when the timeout passes
With `--timeout <secs>`, `cr wait` SHALL never sleep past the deadline, and when a read after the deadline has not reached a wanted state it SHALL print the event with `timed_out` true and exit 75; text SHALL add `Timed out before the verdict reached <states>.`. An interrupt SHALL stop it with `cr wait interrupted`.

#### Scenario: Timeout
- **WHEN** `cr wait --until mergeable --timeout 600` runs and the verdict stays `pending`
- **THEN** after about 600 seconds it prints `Timed out before the verdict reached mergeable.` and exits 75

### Requirement: State changes post to the change request
`cr ready`, `cr draft` and `cr close` SHALL send `POST .../change-requests/<N>/<ready|draft|close>` with no body for the picked draft or open change request, and print `change_request` with `action` the command name; the server decides which transitions are allowed.

#### Scenario: Ready
- **WHEN** `goodflows cr ready` runs for draft change request 1
- **THEN** it posts `.../change-requests/1/ready` and the text's first line starts `#1 open`

### Requirement: cr reopen finds the branch's closed change request
`cr reopen` SHALL post `.../change-requests/<N>/reopen` where N is `--number`, else the one change request with `state=closed` of the current branch, failing with `branch <b> has no closed change request; pass --number` or `branch <b> has several closed change requests (#..); pass --number`. Other commands SHALL NOT pick closed change requests.

#### Scenario: Reopen without a number
- **WHEN** branch `feat/login` has only closed change request `#4` and `cr reopen` runs
- **THEN** it posts `.../change-requests/4/reopen`; `cr ready` on the same branch then exits 1

### Requirement: cr merged reports a merge commit
`cr merged --commit <rev>` SHALL post `.../change-requests/<N>/merged` with `merged_commit`, `target_branch` and, with `--lease`, `lease_id`. A 40 or 64 character lowercase hex commit SHALL be sent as given; anything else SHALL be resolved with git, failing with `<rev> does not name a commit`. Without `--target` it SHALL read the change request's target branch.

#### Scenario: Merge of HEAD
- **WHEN** `cr merged -n 42 --commit HEAD --target main` runs at commit `3f2a…`
- **THEN** the body is `{"merged_commit": "<full SHA of HEAD>", "target_branch": "main"}`

### Requirement: cr retarget changes only the server's target
`cr retarget <branch>` SHALL post `.../change-requests/<N>/retarget` with `{"target_branch": <branch>}` and change no Git ref. The branch MUST be 1-256 bytes without control characters, else exit 1 with `the target branch must be 1-256 bytes without control characters` before the change request is looked up.

#### Scenario: Retarget
- **WHEN** `cr retarget release -n 42` runs
- **THEN** it posts `{"target_branch": "release"}` to `.../change-requests/42/retarget` and prints `change_request` with `action` `retarget`

### Requirement: cr parent sets or clears the parent
`cr parent <P>` SHALL send `PUT .../change-requests/<N>/parent` with `{"number": P}`, P at least 1, and `cr parent --clear` SHALL send `DELETE` to the same path. One of the two is required and they MUST NOT be combined (exit 2).

#### Scenario: Both given
- **WHEN** `cr parent 3 --clear` runs
- **THEN** it exits 2 with `the argument '[PARENT]' cannot be used with '--clear'`

#### Scenario: Stack
- **WHEN** `cr parent 41 -n 42` runs
- **THEN** it sends `PUT .../change-requests/42/parent` with body `{"number": 41}`

### Requirement: cr validate checks files without a server or config
`cr validate [<path>...]` SHALL send no request and need neither an API key nor `goodflows.toml`. It SHALL print `cr_validation` with `ok` and one entry per path: `file`, `kind` (`claims`, `scenario_set`, `verify_run` or `unknown`), `ok`, `errors` and `warnings`, and exit 1 when any file has errors, else 0.

#### Scenario: Text summary
- **WHEN** `cr validate n.txt` checks a file without front matter
- **THEN** it prints `failed n.txt (unknown)`, `  error: n.txt: the front matter's format must be change-claims/2, qa-scenarios/2 or verify-run-notes/1`, `1 checked: 0 ok, 1 failed` and exits 1

### Requirement: cr validate finds the checkout's files
Without paths, `cr validate` SHALL check the branch's claims file and diff set when they exist, each `openspec/changes/<change>/qa-scenarios.md` except under `archive`, and each `.verify/<app>/<run-id>/` folder holding `run.md`, in that order with sorted directories, shown relative to the working directory. With none found it MUST exit 1 with `found no change request files (...); pass the files to check`.

#### Scenario: Default discovery
- **WHEN** the branch `feat/login` has a claims file and `openspec/changes/add-login/qa-scenarios.md` exists
- **THEN** `files[0].file` is `.goodflows/change-requests/feat/login/cr.md` and `files[1].file` is `openspec/changes/add-login/qa-scenarios.md`

### Requirement: cr validate picks each file's kind
A directory SHALL be a run folder when it holds `run.md`, else an error `<dir> is a directory without run.md; ...`. A file's kind SHALL come from its front matter `format`, else its name (`cr.md`, a name ending `scenarios.md`, `run.md`). A file in a `verify-*/features` directory SHALL fail with `<file> is a feature file; check it with goodflows features validate`. An unreadable file SHALL report `could not read <file>: ...`.

#### Scenario: Missing file
- **WHEN** `cr validate nothere.md --json` runs
- **THEN** the entry has `kind` `unknown`, `ok` false and an error starting `could not read nothere.md` and it exits 1

### Requirement: cr validate checks claims as cr open and cr claims read them
A claims file SHALL get every problem `cr open` and `cr claims` would refuse, plus `<file>: the front matter needs a top-level cr: null line for cr open to fill in` when `cr` is absent. It SHALL warn `<file>: head is <h> but HEAD is <sha>; cr claims uploads claims only for HEAD` and, when `cr` is null without a title, `<file>: the body has no # heading, so cr open needs --title`. It SHALL also warn, naming the key or entry, about each rule of the claims format the server refuses that the file breaks among: a missing or unknown front matter key, a `door` other than `one_way` or `two_way`, a `proof` other than `stated`, `pointed`, `walked` or `ran`, a `ran` fact with no `evidence`, and a question whose `recommendation` is not one of its `options`. Warnings SHALL NOT fail the file.

#### Scenario: Other head
- **WHEN** `cr.md` has `cr: 4` and `head: abc`
- **THEN** its entry has `ok` true and the head warning, and `cr validate` on it alone exits 0

#### Scenario: No cr line
- **WHEN** `other.md` has `format: change-claims/2` and the current `head` but no `cr`
- **THEN** its error is `<file>: the front matter needs a top-level cr: null line for cr open to fill in`

### Requirement: cr validate checks scenario sets and run folders as their uploads read them
A scenario set SHALL get every problem `cr scenarios upload` would refuse, including scenarios with no or several `### <id>. <name>` headings under `## Scenarios`, and a warning when its `revision` ends `+dirty`. A run folder, or a `run.md` checked through its folder, SHALL get every problem `cr runs upload` would refuse before sending.

#### Scenario: Set with heading problems
- **WHEN** a set lists ids 1 and 2 and has `### 1. A` and `### 1. Again`
- **THEN** its errors are `<file>: scenario 1 has more than one heading` and `<file>: scenario 2 has no "### 2. <name>" heading under ## Scenarios`

#### Scenario: Dirty diff set
- **WHEN** a valid set has `revision: "a1b2c3d+dirty"`
- **THEN** it is ok with warning `<file>: revision ends +dirty, so cr scenarios upload refuses it until the changes are committed and the set is written again`
