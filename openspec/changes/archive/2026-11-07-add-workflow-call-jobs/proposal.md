# Proposal

## Why

Named workflows currently isolate their job graphs, so sharing a suite between CI, release, and other workflows requires duplicating jobs or launching nested goodflows shell commands. A workflow-call job makes an existing workflow reusable at an explicit point in another workflow's dependency graph while keeping scheduling, failures, and diagnostics under goodflows' control.

## What Changes

- Add `workflow = "name"` as a third job execution form, mutually exclusive with `command` and `steps`, referencing a workflow in the same config file.
- Wait for the called workflow to finish before completing the call job; use existing job dependencies and conditions for orchestration. Definition order alone introduces no ordering.
- Execute each call independently, including repeated calls to the same workflow. Do not introduce workflow-level prerequisites, automatic deduplication, or cross-run caching.
- Validate workflow references and recursive calls across the entire document before executing anything; apply root `--jobs` selection only to the root workflow.
- Preserve called-workflow settings and profile resolution, including per-key environment inheritance and the existing global job-name partition map. Enforce a shared concurrency ceiling without holding slots for waiting calls, and propagate failures and cancellation through the invocation tree.
- Identify child executions in events, logs, reports, and optional history; include called graphs in graph fingerprints without changing fingerprints for existing command/step-only selections. Event identity is additive and does not replace the workflow name on live text lines.
- Keep the first version same-file and parameter-free: no workflow inputs/outputs, cross-workflow `needs` references, call-specific environment overrides, call partitions, or call-level timeout.

## Capabilities

### New Capabilities

- `workflow-calls`: Definition, validation, selection, settings isolation, scheduling, completion, and observable identity of synchronous workflow-call jobs. A workflow calling itself is a direct cycle and is rejected.

### Modified Capabilities

- `run-history`: Fingerprint reachable called graphs and identify child invocations in immutable history records without storing environment values or output.
- `run-report`: Show workflow-call results and child failures with invocation context, retaining the root report as the final output and providing root-context rerun hints.

## Impact

Changes affect `src/config.rs`, `src/main.rs`, `src/runner.rs`, `src/output.rs`, `src/output/report.rs`, and `src/history.rs`, plus the in-repo Amp plugin `.amp/plugins/goodflows.ts`, which must accept one summary per called workflow run and judge the root summary, with integration through existing reporting sessions, log retention, and the shared RSS sampler. Extend existing workflow, scheduling, process, inspection, history, reporting, and output tests and document the syntax and boundaries in README.md. No new external service, dependency, report wire format, or automatic config migration is intended. Existing configurations retain their execution semantics and output shapes when they do not invoke workflows.
