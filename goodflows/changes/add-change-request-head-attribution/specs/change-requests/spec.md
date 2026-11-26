## MODIFIED Requirements

### Requirement: Git facts describe HEAD against the target
Refs: G1, D5, D6
The CLI SHALL report a `git-facts/1` document to `POST .../change-requests/<N>/git-facts` with `head`, `target` (`branch`, `merge_base`), `commits` from the merge base to HEAD oldest first (`sha`, `parents`, `changed_files` against the first parent) and `diff.files` from the merge base to HEAD. The target commit SHALL be `origin/<target>`, else the local `<target>`. The posted body SHALL be the document's keys plus `links` and `agent` when collected; the `git_facts` summary and all command output SHALL come from the document alone. A `422` answer SHALL NOT be retried without `links` and `agent`.

#### Scenario: Two commits with a rename
- **WHEN** `feat/login` adds `lib/login.ex` (2 lines) in one commit and renames `old.txt` to `docs/new.txt` in the next
- **THEN** `diff.files` is `docs/new.txt` `renamed` with `old_path` `old.txt` and 0/0 counts, then `lib/login.ex` `added` with `additions` 2, and `commits` lists both commits with their parents

#### Scenario: Attribution travels beside the document
- **WHEN** `cr push` runs in an Amp session and attribution was collected
- **THEN** the posted body has the document's keys plus `links` and `agent`, and the event's `git_facts` summary is the same as without attribution

### Requirement: cr open creates or reuses the branch's change request
Refs: G1, D4
`cr open` SHALL need a checked-out branch (`HEAD is detached; cr open needs the change request's branch checked out`) of 1-256 bytes without control characters. It SHALL reuse the branch's one draft or open change request without updating it, or create a draft with `POST .../change-requests` and body `title`, `source_branch`, and `openspec_change`, `target_branch` and `links` when given or collected.

#### Scenario: Create from a claims file
- **WHEN** branch `feat/login` has no change request, the default claims file has `# Add login`, and no link source supplies a link
- **THEN** the create body is `{"title": "Add login", "source_branch": "feat/login"}` and the event has `created` true

#### Scenario: Reuse
- **WHEN** the branch has draft `#3` linked to `add-login` and `cr open --openspec-change add-login --json` runs
- **THEN** only `GET` requests are sent and the event has `created` false and number `3`

#### Scenario: Create in an Amp session
- **WHEN** branch `feat/login` has no change request and `cr open --title 'Add login'` runs with `AMP_CURRENT_THREAD_ID=T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b`
- **THEN** the create body has `links` `{"threads": ["https://ampcode.com/threads/T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b"]}`

## ADDED Requirements

### Requirement: Git facts reports carry the head's links
Refs: G1, G2, D1, D2
A Git facts report (`cr open` with a claims file, `cr claims`, `cr push`) SHALL carry `links` collected as run records collect them, with the same validation and caps: `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS`, then the agent session, then `Amp-Thread-ID` trailers and tickets matching the root `[settings] ticket_prefixes` from git. `cr` commands SHALL keep rejecting `--ticket` and `--thread-url`. `links` and each empty list SHALL be omitted.

#### Scenario: Session and trailer
- **WHEN** `cr push` runs with `AMP_CURRENT_THREAD_ID=T-<a>` and a branch commit has the trailer `Amp-Thread-ID: T-<b>`
- **THEN** the report's `links.threads` is the URL of `T-<a>`, then the URL of `T-<b>`

#### Scenario: No link source
- **WHEN** `cr claims` runs with no link variable, no agent session and no trailers
- **THEN** the report body has no `links` key

### Requirement: The trailer scan follows the change request's target
Refs: D1
For a change request command, the git scan SHALL read the messages of at most 200 commits reachable from HEAD but not from the target commit the Git facts use (`origin/<target>`, else `<target>`), newest first, and SHALL ignore `GITHUB_BASE_REF`. Run records SHALL keep their base.

#### Scenario: Trailer on the target branch
- **WHEN** `main` has a commit with `Amp-Thread-ID: T-<old>`, `feat/login` branches from it and adds a commit with `Amp-Thread-ID: T-<new>`, and `cr push` runs for a change request targeting `main`
- **THEN** `links.threads` holds `T-<new>` and not `T-<old>`

### Requirement: Git facts reports name the agent
Refs: G1, D3
A Git facts report SHALL carry `agent` with `name` from `GOODFLOWS_AGENT`, or `amp` when that is unset or invalid and `AMP_CURRENT_THREAD_ID` or `AMP_THREAD_ID` names a valid thread, and `model` from `GOODFLOWS_MODEL` when set. Without a name it SHALL omit `agent`, even when `GOODFLOWS_MODEL` is set. Only the caller's process environment SHALL be read.

#### Scenario: Harness names itself
- **WHEN** `cr push` runs with `GOODFLOWS_AGENT=claude-code` and `GOODFLOWS_MODEL=claude-opus-5-5`
- **THEN** the report's `agent` is `{"name": "claude-code", "model": "claude-opus-5-5"}`

#### Scenario: Amp session without the variables
- **WHEN** `cr push` runs with only `AMP_CURRENT_THREAD_ID=T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b` set
- **THEN** the report's `agent` is `{"name": "amp"}`

#### Scenario: Model without a name
- **WHEN** `cr push` runs outside an agent session with only `GOODFLOWS_MODEL=gpt-6-astra`
- **THEN** the report body has no `agent` key

### Requirement: Invalid attribution never fails a command
Refs: G2, D3
`GOODFLOWS_AGENT` MUST be 1 to 64 bytes matching `[a-z0-9][a-z0-9._-]*` and `GOODFLOWS_MODEL` 1 to 128 bytes of printable ASCII without spaces. An invalid value SHALL be dropped with a stderr warning naming the variable but not its value, and the command's exit code MUST NOT change.

#### Scenario: Uppercase agent name
- **WHEN** `cr push` runs with `GOODFLOWS_AGENT=Claude` in an Amp session
- **THEN** stderr warns that `GOODFLOWS_AGENT` is invalid without printing `Claude`, the report's `agent` is `{"name": "amp"}`, and the command exits 0

#### Scenario: Invalid model
- **WHEN** `cr push` runs with `GOODFLOWS_AGENT=codex` and `GOODFLOWS_MODEL` holding a space
- **THEN** stderr warns about `GOODFLOWS_MODEL`, and the report's `agent` is `{"name": "codex"}`

### Requirement: Attribution is collected once, only when sent
Refs: D4
A change request command SHALL collect links and agent at most once, when it first sends a Git facts report or creates a change request, and a command that sends neither MUST NOT run git to collect them.

#### Scenario: Reuse without a claims file
- **WHEN** `cr open` reuses the branch's draft and there is no claims file
- **THEN** no git command runs to collect links

#### Scenario: Open with a claims file
- **WHEN** `cr open` creates a change request from a claims file
- **THEN** links are collected once, and the create body and the Git facts report carry the same `links`
