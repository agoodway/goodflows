# Design

## Context

`Ctx::report_facts` (`src/change_requests/mod.rs`) builds the `git-facts/1` document with `self.git.facts(target)`, posts it as the whole body, and returns a summary built from it. `open` posts the create body itself. `cr` receives only `[reporting]`, so it cannot see `[settings] ticket_prefixes`.

`links::collect` merges flags, `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS`, the Amp session and a git scan of up to 200 commit messages from HEAD back to `refs/remotes/origin/$GITHUB_BASE_REF` or `refs/remotes/origin/HEAD`. It warns on invalid environment entries and never fails. Run records call it once per run through a `OnceCell`.

The server contract is goodflows_web `add-change-request-head-attribution`: optional `links` (the run `links` shape) and `agent` (`name` slug, optional `model`) beside the document; the digest covers only the document; the first report of a head wins.

## Goals / Non-Goals

**Goals:**
- Every head pushed through the CLI by an agent names the agent and its threads, with no new flags and no new way to fail.

**Non-Goals:**
- Showing attribution in CLI output. Readers use the Git facts read.
- Detecting agents other than Amp. Other harnesses set `GOODFLOWS_AGENT`.

## Decisions

### D1. Reuse `links::collect`, with the base as a parameter

`collect` gains a `base: Option<&str>` ref. Run records pass `None` and keep today's `GITHUB_BASE_REF`/`origin/HEAD` lookup; change request commands pass the target commit `Git::facts` resolved (`refs/remotes/origin/<target>`, else `refs/heads/<target>`). The scan then reads the same range the Git facts describe, so a trailer that is already on the target is not attributed to this head, and a stacked change request targeting its parent's branch only gets its own commits.

Alternative: read trailers from the Git facts commits directly. Rejected: the document carries no messages, and a second scan path would duplicate the 200-commit cap, timeout and key-variable scrubbing that `scan` already has.

### D2. Flags stay run-only

`--ticket` and `--thread-url` are rejected on `cr` commands today (`run_only_flag_besides`), and `cr defect --ticket` already reuses the id with another meaning. Allowing them on `cr open`, `cr claims` and `cr push` would mean a second meaning per command. The environment variables cover explicit values, and harnesses set environment variables more easily than flags.

### D3. `agent` from the environment, `amp` from the session

`GOODFLOWS_AGENT` wins; else a session thread that passes the session checks (`AMP_CURRENT_THREAD_ID` or `AMP_THREAD_ID`, valid ID and origin) gives `amp`. `GOODFLOWS_MODEL` is attached to whichever name results. The CLI validates both with the server's rules so a bad value is dropped locally with a warning instead of failing the push with `422`. Amp does not expose its model in the environment today, so an Amp session sends no model unless `GOODFLOWS_MODEL` is set.

### D4. Collected lazily once per command, held on `Ctx`

`Ctx` gets a `OnceCell<Attribution>` (links and agent). `open` (create) and `report_facts` call `ctx.attribution().await`, so `cr open` with a claims file collects once and sends the same `links` in both bodies; commands that send neither never run the git scan. `cr` gains `ticket_prefixes` from the root settings, passed by `main.rs` alongside `reporting`.

### D5. Only the posted body changes

`report_facts` keeps `facts` as the document for the summary and the `409` message, and posts a copy with the two keys added. Output stays byte-for-byte the same, so scripts that parse `git_facts` are unaffected.

### D6. No fallback for an old server

A `422` on `links` or `agent` could be retried without them, but that adds a second request path for a window that only lasts until the hosted server deploys. The release notes name the server change as a prerequisite instead.

## Risks / Trade-offs

- [A CLI released before the server change breaks every push with `422`] → release only after the server deploy; the server change's tasks list this ordering.
- [A branch-wide scan adds threads of every commit, not only the pushing one] → `links` means "threads behind this head"; `agent` and the session thread (always first) are the stronger signal.
- [Self-reported values] → attribution is for metrics; nothing is authorized by it.

## Migration Plan

Additive CLI behavior. Ship after goodflows_web `add-change-request-head-attribution` is deployed. Rollback is a CLI downgrade; heads reported meanwhile keep their stored attribution.
