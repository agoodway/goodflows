---
format: goodflows-change/1
id: add-change-request-head-attribution
status: proposed
size: small
door: two_way
capabilities: [change-requests]
depends_on: []
goals:
  - id: G1
    text: Every head an agent pushes through the CLI names the agent, its model when known, and the threads and tickets behind it.
  - id: G2
    text: Attribution adds no flags and no new way for a command to fail.
non_goals:
  - id: N1
    text: Showing attribution in CLI output, including cr status and cr verdict; readers use the server's Git facts read.
  - id: N2
    text: Detecting agents other than Amp; other harnesses set GOODFLOWS_AGENT.
  - id: N3
    text: Attributing claims, scenario sets, verify runs or review answers.
  - id: N4
    text: Reporting usage, tokens or cost.
anti_goals:
  - id: A1
    text: Printing an environment value that failed validation.
  - id: A2
    text: Changing the output of cr open, cr claims or cr push, which scripts parse.
decisions:
  - id: D1
    question: How are the head's links collected, and which commits does the trailer scan read?
    answer: Reuse the run records' link collection, with the scan base as a parameter. Change request commands pass the target commit the Git facts resolved (origin/<target>, else <target>); run records keep their GITHUB_BASE_REF / origin/HEAD base.
    by: agent
    why: The scan then reads the same range the Git facts describe, so a trailer already on the target is not attributed to this head, and a stacked change request gets only its own commits.
    rejected:
      - Reading trailers from the Git facts commits directly; the document carries no messages, and a second scan path would duplicate the 200-commit cap, timeout and key-variable scrubbing the existing scan has.
  - id: D2
    question: Do cr open, cr claims and cr push accept --ticket and --thread-url?
    answer: No. The flags stay run-only; GOODFLOWS_TICKETS and GOODFLOWS_THREAD_URLS supply explicit values.
    by: agent
    why: cr commands reject these flags today and cr defect --ticket already gives the id another meaning, so allowing them would mean a second meaning per command. Harnesses set environment variables more easily than flags.
  - id: D3
    question: Where does agent come from, and what happens to a bad value?
    answer: GOODFLOWS_AGENT wins; otherwise a valid Amp session thread (AMP_CURRENT_THREAD_ID or AMP_THREAD_ID) gives amp. GOODFLOWS_MODEL is attached to whichever name results. Both are validated locally with the server's rules, and a bad value is dropped with a warning.
    by: agent
    why: Dropping locally keeps a bad value from failing the push with 422. Amp does not expose its model in the environment, so an Amp session sends no model unless GOODFLOWS_MODEL is set.
  - id: D4
    question: When is attribution collected?
    answer: Lazily, at most once per command, the first time the command creates a change request or sends a Git facts report. cr receives the root [settings] ticket_prefixes so the scan can find tickets.
    by: agent
    why: cr open with a claims file then sends the same links in both bodies, and commands that send neither never run the git scan.
  - id: D5
    question: What changes besides the posted body?
    answer: Nothing. The Git facts document used for the git_facts summary and the 409 message stays the bare document; only the posted body gains links and agent.
    by: agent
    why: Output stays byte for byte the same, so scripts that parse git_facts are unaffected.
  - id: D6
    question: What does the CLI do when a server without head attribution answers 422 on links or agent?
    answer: Nothing special; it fails as any 422 does. The release notes name the server change as a prerequisite.
    by: agent
    why: Retrying without the keys adds a second request path for a window that lasts only until the hosted server deploys.
    rejected:
      - Retrying the report without links and agent after a 422.
left_to_builder:
  - Names and signatures of the base parameter on the existing link collection and of the new agent reader, and how the per-command cache is held (the design suggested a OnceCell<Attribution> on Ctx).
  - How the posted body is built from the document (a copy with the keys added), as long as the document used for the summary and the 409 message is unchanged.
  - How the root [settings] ticket_prefixes reaches the cr commands.
  - Exact wording of the invalid-variable warnings; each must name the variable and must not contain its value.
  - Document GOODFLOWS_AGENT and GOODFLOWS_MODEL, and what cr open, cr claims and cr push send, in README, src/agents_guide.md and the bundled change request skill.
  - Which test files hold the new unit and mock-server tests; the existing link scan tests must pass unchanged.
  - Release only after goodflows_web add-change-request-head-attribution is deployed, and name it as a prerequisite in the release notes.
---

## Why

The goodflows_web change `add-change-request-head-attribution` lets a Git facts report name the agent, model, threads and tickets behind a head, so the server can group fix iterations, reason spans, bypass merges and defects by agent. The server only learns what the CLI sends, and today the CLI sends neither: Git facts reports post the bare `git-facts/1` document, and `cr open` creates a change request without `links`, although the server already accepts them. The CLI already finds threads for run records, from the Amp session and `Amp-Thread-ID` trailers.

## Context

Server contract (goodflows_web `add-change-request-head-attribution`): a Git facts report body may carry, beside the document's keys, optional `links` (the run record `links` shape) and `agent` (`name`, a slug, and optional `model`). The digest covers only the document, and the first report of a head wins. The create body of `POST .../change-requests` already accepts `links`. A server without the change answers `422` on `links` or `agent` in a Git facts report, which would break every push, so this CLI must ship after that server deploy. Rollback is a CLI downgrade; heads reported meanwhile keep their stored attribution.

Run records collect `links` from flags, `GOODFLOWS_TICKETS` and `GOODFLOWS_THREAD_URLS`, the Amp session, and a git scan of up to 200 commit messages from HEAD back to `refs/remotes/origin/$GITHUB_BASE_REF` or `refs/remotes/origin/HEAD`. Collection warns on invalid environment entries and never fails. The Git facts target commit is `refs/remotes/origin/<target>`, else `refs/heads/<target>`. The `cr` commands currently receive only the `[reporting]` settings, not `[settings] ticket_prefixes`.

Attribution is self-reported and used for metrics only; nothing is authorized by it.

## Approaches

Collection reuses the run record path with a different scan base (D1) rather than a second scanner, and stays on environment variables rather than flags (D2, D3). A branch-wide scan adds the threads of every commit on the branch, not only the pushing one; `links` means "threads behind this head", and `agent` plus the session thread, always first, are the stronger signal. A fallback for old servers was considered and left out (D6) in favor of release ordering.
