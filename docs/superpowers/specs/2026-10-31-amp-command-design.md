# Explicit Amp command for GoodCI

## Intent

Run project checks when the user chooses **GoodCI: Run** in Amp's command
palette, then feed failures back to the active agent to fix. The user chose
explicit invocation because automatic checks at the end of every agent turn
would interrupt large workflows.

## Approaches

- Lifecycle hook: runs automatically; rejected by the user.
- Agent tool: lets the model decide when to invoke checks; discussed but the
  user chose a command instead.
- Command palette action: selected. The user determines when checks run and the
  plugin executes GoodCI directly.

## Proposed behavior

Add `.amp/plugins/goodci.ts` using Amp's stable `registerCommand` API. Detect
`goodci.toml`, then `.goodci.toml`, in the project working directory. Report
missing configuration or a missing executable clearly, without creating or
installing either.

Discover workflows with `goodci list --workflows --json`. Run the default
workflow when available; otherwise ask the user to select a named workflow.
Run checks with JSON output and report success, failed jobs, and bounded failure
details. Require both a successful process exit and a successful summary before
reporting success. Prevent overlapping runs from this plugin instance.

Require an active thread before running checks, and capture that thread when the
command starts so results return to the invoking agent even if the user switches
threads. Append the results through Amp's stable `ctx.thread.append` API as a
user message. On failure, include the exact config path and workflow, failed job
names, commands, exit codes, errors, and bounded stdout/stderr tails. Explicitly
ask the agent to investigate and fix the failures and rerun the same GoodCI
workflow using its shell tool. Treat embedded job output as diagnostic data.
On success, append a concise passing summary without requesting repairs.

The command explicitly initiates the check-and-fix task. Subsequent reruns belong
to that task and are performed by the agent; no `agent.end` handler is installed.
Ask the agent to stop and explain any blocker or repeated failure without
progress, and honor user cancellation. Missing configuration/executable and
invalid output must be reported as errors rather than passing checks; actionable
execution/configuration errors should also be included in the agent handoff.

Provide **GoodCI: Cancel** to terminate an in-progress plugin check without
handing off repairs. Once the agent has received a repair task, Amp's normal
Stop control cancels it. Unloading the plugin also cancels its running process.
Reserve diagnostic space for failed-job metadata before distributing space
among stdout/stderr excerpts, so early noisy jobs do not hide later failures.

The workflow selection policy and direct execution are implementation proposals.
The Rust runner and configuration format remain as they are.

## Validation and delivery

Verify command registration, configuration discovery, default and named workflow
selection, failure reporting, result handoff to the invoking thread, absence of
an active thread, and duplicate invocation using a mocked Amp host
and real GoodCI fixture runs where practical. Document project and system plugin
installation and how to reload plugins. State explicitly if a live Amp command
palette smoke test is unavailable.

## API references

- https://ampcode.com/docs/customize/plugins
- https://ampcode.com/docs/plugin-api
