---
format: qa-scenarios/2
source: intent
change: add-change-request-head-attribution
specs: uncommitted
scenarios:
  - id: 1
    name: Push in an Amp session attributes the head without changing output
    priority: must
    covers:
      - {capability: change-requests, requirement: Git facts describe HEAD against the target, scenario: Attribution travels beside the document}
      - {capability: change-requests, requirement: Git facts reports carry the head's links, scenario: Session and trailer}
      - {capability: change-requests, requirement: Git facts reports name the agent, scenario: Amp session without the variables}
  - id: 2
    name: cr open creates with links and reports the same links
    priority: must
    covers:
      - {capability: change-requests, requirement: cr open creates or reuses the branch's change request, scenario: Create in an Amp session}
      - {capability: change-requests, requirement: "Attribution is collected once, only when sent", scenario: Open with a claims file}
  - id: 3
    name: Reusing a change request collects nothing and updates nothing
    priority: must
    covers:
      - {capability: change-requests, requirement: cr open creates or reuses the branch's change request, scenario: Reuse}
      - {capability: change-requests, requirement: "Attribution is collected once, only when sent", scenario: Reuse without a claims file}
  - id: 4
    name: No attribution source sends neither key
    priority: must
    covers:
      - {capability: change-requests, requirement: Git facts reports carry the head's links, scenario: No link source}
      - {capability: change-requests, requirement: Git facts reports name the agent, scenario: Model without a name}
      - {capability: change-requests, requirement: cr open creates or reuses the branch's change request, scenario: Create from a claims file}
  - id: 5
    name: Trailers already on the target are not attributed
    priority: must
    covers:
      - {capability: change-requests, requirement: The trailer scan follows the change request's target, scenario: Trailer on the target branch}
  - id: 6
    name: A harness names itself
    priority: must
    covers:
      - {capability: change-requests, requirement: Git facts reports name the agent, scenario: Harness names itself}
  - id: 7
    name: Invalid variables are dropped with a warning
    priority: must
    covers:
      - {capability: change-requests, requirement: Invalid attribution never fails a command, scenario: Uppercase agent name}
      - {capability: change-requests, requirement: Invalid attribution never fails a command, scenario: Invalid model}
  - id: 8
    name: Link flags stay run-only
    priority: should
    covers:
      - {capability: change-requests, requirement: Git facts reports carry the head's links, implied: true}
  - id: 9
    name: A real server stores the attribution
    priority: should
    covers:
      - {capability: change-requests, requirement: Git facts describe HEAD against the target, implied: true}
---

# QA: add-change-request-head-attribution

Run the CLI against a mock or test GoodFlows server that records request bodies, in a scratch git repository with `main` pushed as `origin/main` and a branch `feat/login`. Unset `GOODFLOWS_AGENT`, `GOODFLOWS_MODEL`, `GOODFLOWS_TICKETS`, `GOODFLOWS_THREAD_URLS`, `AMP_CURRENT_THREAD_ID`, `AMP_THREAD_ID` and `GITHUB_BASE_REF` unless a step sets them.

## Scenarios

### 1. Push in an Amp session attributes the head without changing output (Must)

1. On `feat/login` with change request `7` targeting `main`, commit with the trailer `Amp-Thread-ID: T-11111111-1111-4111-8111-111111111111`.
2. Run `cr push --json` once with no variables set and record the event.
3. Run it again with `AMP_CURRENT_THREAD_ID=T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b`.

Expected: the second `POST .../7/git-facts` body has every document key plus `links.threads` = the URL of `T-3bb6cad7-...` then the URL of `T-11111111-...`, and `agent` = `{"name": "amp"}`. The event's `git_facts` object and the text output are identical to step 2's apart from values that depend on the head. Exit code 0.

### 2. cr open creates with links and reports the same links (Must)

1. On `feat/login` with no change request, write the default claims file with `# Add login`.
2. Run `cr open --json` with `AMP_CURRENT_THREAD_ID=T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b`.

Expected: the `POST .../change-requests` body has `links` `{"threads": ["https://ampcode.com/threads/T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b"]}`; the following `POST .../git-facts` body has the same `links`; the git log scan for links runs once (check with `GIT_TRACE=1`).

### 3. Reusing a change request collects nothing and updates nothing (Must)

1. The branch has draft `#3` linked to `add-login`; there is no claims file.
2. Run `cr open --openspec-change add-login --json` with `AMP_CURRENT_THREAD_ID` set and `GIT_TRACE=1`.

Expected: only `GET` requests reach the server, the event has `created` false and number `3`, and the trace shows no git log scan for links.

### 4. No attribution source sends neither key (Must)

1. With no link variable, no agent session and no trailers, but `GOODFLOWS_MODEL=gpt-6-astra`, run `cr open --json` on `feat/login` with a claims file `# Add login`, then `cr claims --json`.

Expected: the create body is exactly `{"title": "Add login", "source_branch": "feat/login"}`, and neither Git facts body has a `links` or `agent` key.

### 5. Trailers already on the target are not attributed (Must)

1. Commit on `main` with `Amp-Thread-ID: T-<old>` and push it to `origin/main`; branch `feat/login` from it and commit with `Amp-Thread-ID: T-<new>`.
2. Run `cr push --json` for a change request targeting `main`, with `GITHUB_BASE_REF=release` set to show it is ignored.

Expected: `links.threads` holds the URL of `T-<new>` and not `T-<old>`. A `goodflows run` on the same checkout without `GITHUB_BASE_REF` still scans back to `origin/HEAD` as before.

### 6. A harness names itself (Must)

1. Run `cr push --json` with `GOODFLOWS_AGENT=claude-code` and `GOODFLOWS_MODEL=claude-opus-5-5`, no Amp session.

Expected: the Git facts body's `agent` is `{"name": "claude-code", "model": "claude-opus-5-5"}`.

### 7. Invalid variables are dropped with a warning (Must)

1. Run `cr push` with `GOODFLOWS_AGENT=Claude` and `AMP_CURRENT_THREAD_ID=T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b`.
2. Run `cr push` with `GOODFLOWS_AGENT=codex` and `GOODFLOWS_MODEL='gpt 6'`.

Expected: step 1 warns on stderr naming `GOODFLOWS_AGENT`, stderr does not contain `Claude`, `agent` is `{"name": "amp"}`, exit 0. Step 2 warns naming `GOODFLOWS_MODEL` without its value, `agent` is `{"name": "codex"}`, exit 0.

### 8. Link flags stay run-only (Should)

1. Run `cr push --ticket ENG-1` and `cr open --thread-url https://ampcode.com/threads/T-3bb6cad7-8c04-413a-b23c-5fca67cd4f7b`.

Expected: both are rejected as before this change, and nothing is posted.

### 9. A real server stores the attribution (Should)

1. Against a goodflows_web build with `add-change-request-head-attribution`, run `cr open` with a claims file and then `cr push` in an Amp session.
2. Read the head's Git facts back from the server.

Expected: the stored Git facts carry the `links` and `agent` the CLI sent.
