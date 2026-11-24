# Proposal

## Why

The goodflows_web change `add-change-request-head-attribution` lets a Git facts report name the agent, model, threads and tickets behind a head, so the server can group fix iterations, reason spans, bypass merges and defects by agent. The server only learns what the CLI sends. Today the CLI sends neither: `report_facts` posts the bare `git-facts/1` document, and `cr open` creates a change request with `title`, `source_branch`, `openspec_change` and `target_branch` but no `links`, although the server already accepts them. The CLI already knows how to find the threads: `links::collect` reads the Amp session and `Amp-Thread-ID` trailers for run records.

## What Changes

- Every Git facts report (`cr open` with a claims file, `cr claims`, `cr push`) adds two optional keys to the request body next to the document: `links`, collected the way run records collect them, and `agent`, `{name, model}`. Each key is left out when there is nothing to send.
- `agent` comes from the caller's environment: `GOODFLOWS_AGENT` and `GOODFLOWS_MODEL`. Without `GOODFLOWS_AGENT`, an Amp session (`AMP_CURRENT_THREAD_ID` or `AMP_THREAD_ID` that names a valid thread) gives `name` `amp`. Invalid values are dropped with a stderr warning that names the variable but not its value; attribution never fails a command.
- For change requests, the `Amp-Thread-ID` trailer scan covers the commits from the target branch to HEAD (the commits the Git facts describe), not the run scan's `GITHUB_BASE_REF` or `origin/HEAD` base.
- `cr open` sends the same `links` in the create body when it creates a change request. A reused change request is not updated.
- Links and agent are collected at most once per command, and only by a command that sends them.
- No new flags. `--ticket` and `--thread-url` stay run-only; `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS` supply explicit values.
- The command output (`git_facts` summary, events, text lines) does not change.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `change-requests`: the Git facts report body and the `cr open` create body carry attribution, with its sources, its order and how invalid values are handled.

## Impact

- `src/links.rs`: `collect` takes the base ref for the trailer scan (run records keep their current base); a new `agent` function reads the agent variables.
- `src/change_requests/mod.rs`: `Ctx` collects attribution lazily once per command; `report_facts` adds it to the posted body (the local `facts` value and the summary stay the document only); `open` adds `links` to the create body. `cr` gains the root `[settings] ticket_prefixes` from `main.rs`.
- `src/agents_guide.md` and the bundled skills document `GOODFLOWS_AGENT` and `GOODFLOWS_MODEL` for agent harnesses.
- Compatibility: a server without `add-change-request-head-attribution` answers `422` on `links` or `agent` in a Git facts report, which would break every push. Release this only after that server change is deployed, and say so in the release notes.
- Out of scope: attributing claims, scenario sets, verify runs or review answers; usage, tokens and cost; showing attribution in `cr status` or `cr verdict`.
