# Amp GoodFlows Command Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox syntax for tracking.

**Goal:** Explicitly run GoodFlows from Amp and hand diagnostic results to the invoking agent for repair and verification.

**Architecture:** A single project plugin registers GoodFlows: Run. It executes the existing CLI, discovers workflows, and appends a bounded diagnostic message to the captured invoking thread. No lifecycle hook or automatic plugin retry is involved.

**Tech Stack:** TypeScript, stable Amp Plugin API, Node's built-in test runner, existing Rust CLI.

**Spec:** `docs/superpowers/specs/2026-10-31-amp-command-design.md`

## Global Constraints

- Explicit command invocation only; no agent.end handler.
- Configuration precedence: goodflows.toml, then .goodflows.toml.
- Use default workflow when present; otherwise select a named workflow.
- Require a successful process exit and successful summary to report success.
- Deliver results to the invoking thread, including failure tails and repair/rerun instructions.
- No changes to Rust runner behavior or TOML format; no installation or initialization side effects.

## Review Focus

- A thread switch during execution must not redirect diagnostics.
- Malformed output and nonzero exits must never become passing checks.
- Workflow names and paths containing shell punctuation must remain literal.
- A cancelled workflow picker must not run checks or start repairs.
- Missing configuration/binary and duplicate invocation must have clear outcomes.

### Task 1: Command, diagnostic handoff, and behavioral tests

**Files:** Create `.amp/plugins/goodflows.ts` and `tests/amp-plugin.test.mjs`.

**Interfaces:** Default export accepts Amp's `PluginAPI`; registers `goodflows-run` with category `GoodFlows` and title `Run`. The test host supplies command registration, shell execution, UI selection/notification, and a thread append recorder.

- [x] Write tests for explicit invocation, missing thread/config/binary, default/named workflows, picker cancellation, failure details, malformed output, nonzero exit, thread switch, and duplicate invocation.
- [x] Run `node --test tests/amp-plugin.test.mjs`; observe failures for the missing command implementation.
- [x] Implement the command using documented stable APIs and bounded output handling. Capture thread before asynchronous work. Pass workflow/config arguments as literal spawn arguments.
- [x] Run the tests, including real CLI fixture checks, and verify the failure-to-agent message contains the exact rerun command and bounded diagnostics.

### Task 2: Documentation, CI, and validation

**Files:** Modify `README.md` and `.github/workflows/ci.yml`.

**Interfaces:** Plugin installs by copying the single file into project or system plugin directories. Tests require Node 22.18+ for built-in TypeScript stripping.

- [x] Document command invocation, configuration/workflow discovery, agent repair handoff, installation/reload, and cancellation limits.
- [x] Configure plugin tests in CI on current Node LTS.
- [x] Verify local Amp can load the plugin, run plugin tests and existing Rust checks, and run `git diff --check`.
- [x] Review the implementation against the spec and report any live Amp UI testing limitation.

## Execution notes

The user approved implementation. Execute inline in this session and leave changes uncommitted for review.

- Ruling: Work on `feat/amp-goodflows-command` in the user's checkout; no external worktree or commits are needed for this reviewable change.
- Ruling: Use Node-compatible child_process streaming instead of the shell runner for GoodFlows, since the latter returns all stdout in memory. Use the command context shell only to resolve the executor working directory.
- Ruling: Add GoodFlows: Cancel and abort running checks on plugin disposal. Amp command contexts have no cancellation signal, so this supplies an explicit stop action. After handoff, normal Amp Stop controls agent repairs.
- Verification: 20 plugin tests passed; `amp plugins list` loads both commands successfully. The real CLI fixtures exercise config discovery, failure handoff, noisy output, cancellation, and option-like workflow names.
- Existing `./target/debug/goodflows` checks passed: fmt, Clippy, all 59 Rust integration tests, and documentation.
- Independent review found option-like workflow handling and later failure omission under truncation; both reproduced with failing tests and fixed. Follow-up review found no remaining material issues.
- Limitation: live Amp UI invocation and a real model's repair turn were not exercised. Command loading is verified with the installed Amp CLI; thread append payloads are verified against a simulated host using the documented API.
