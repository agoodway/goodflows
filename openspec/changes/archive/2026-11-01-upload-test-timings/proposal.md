# Proposal

## Why

GoodCases now produces per-test, module and file timings, but Goodflows only emits local job output. Publishing those reports from the CLI will retain timing history in GoodFlows without adding network requests to test execution or building project-specific upload scripts.

## What Changes

- Add opt-in reporting configuration and per-job `exunit-timings` report collection.
- Give each CLI invocation a unique identity and each report-producing job an isolated persistent directory, exposed to its child processes.
- Publish reports after jobs finish, including failed jobs, using environment-provided endpoint and API credentials; retain local data when publishing is unavailable.
- Add bounded retries, idempotent requests and a command to retry a saved invocation without rerunning tests.
- Preserve test statuses, exit codes, scheduling, text output and JSON event validity. Add explicit reporting status events.
- Store timings only: no dashboards, grouping recommendations, scheduling changes or general log uploads.

## Capabilities

### New Capabilities

- `test-timing-publishing`: reporting configuration, invocation-scoped collection, authenticated delivery, persistence and replay.

### Modified Capabilities

None. This repository has no durable OpenSpec capabilities yet.

## Impact

- CLI: `src/config.rs`, `src/main.rs`, `src/runner.rs`, `src/output.rs`, a reporting module, Cargo dependencies/lockfile, integration tests and README.
- Companion server change: `../goodflows_web/openspec/changes/archive/2026-11-01-store-test-timings/` owns the ingestion endpoint (`POST /api/v1/projects/:project/test-reports`), validation, authentication and Postgres storage. It depends on `../goodflows_web/openspec/changes/archive/2026-11-01-scope-recipes-by-project/`, so the configured project must already exist on the API key's account before the first upload.
- Companion producer integration: `../goodcases/openspec/changes/publish-test-timings/` connects the existing formatter to the CLI-owned report directory.
- Each change owns implementation only in its own repository. The server design is the authoritative shared wire contract.
