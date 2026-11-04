# Proposal

## Why

The test timing contract is ExUnit's event model under another name: the `exunit-timings` format, `module`/`describe` identity, `async`/`schedulers`/`otp_release` fields and ExUnit-shaped aggregates are all hard-coded in `src/reporting/contract.rs`, so no other test runner can publish timings. Jest is the next runner we need, and because goodflows is still greenfield, the contract can be replaced outright rather than extended alongside the old one.

## What Changes

- **BREAKING** Replace the `exunit-timings` format with `test-timings/1`. This is a core event model any runner can produce: `suite`, `group` and `test` started/finished events, plus `interrupted` and `max_failures_reached`. `group` replaces `module`, `path[]` replaces `describe`, and outcomes are `passed`, `failed`, `skipped` and `errored`. Record `run_id` becomes `suite_id`. `exunit-timings` is removed with no compatibility window, and saved snapshots in the old format can no longer be replayed.
- Add an optional flat `meta` object to every event for runner-specific data. Keys follow the identifier grammar. Values are null, booleans, safe integers or strings of at most 256 bytes, with at most 32 keys per event. ExUnit-only fields (`async`, `max_cases`, `schedulers`, versions, `setup_all`, `parameter_id`, ExUnit time buckets) move into it. The per-file `files` aggregates are dropped, because they can be derived from test events.
- Keep suite `filters` (boolean identifier tags) as an optional core field, and add a core `runner {name, version}` to `suite_started`.
- **BREAKING** Job configuration changes from `reports = ["exunit-timings"]` to `reports = ["test-timings"]`.
- goodci becomes the authoritative definition of the contract. The server mirrors it in a later goodflows_web change. Local collection moves from structural allowlists to full type validation of core fields and `meta`, so every frozen snapshot is one the server must accept.
- Add `reporters/exunit/`, a zero-dependency Mix package extracted from goodcases' `GC.TestTimingFormatter` and rewritten to emit `test-timings/1`.
- Add `reporters/jest/`, a zero-dependency CommonJS Jest reporter with TypeScript declarations that emits `test-timings/1`.
- Add a CI drift guard: each reporter runs a fixture suite, and the output is collected by the built CLI. Any rejected report fails the build.
- On each release tag, split each reporter with `git subtree` and push it, with the same tag, to the read-only mirrors `agoodway/goodflows-exunit-reporter` and `agoodway/goodflows-jest-reporter`. Consumers install them as git dependencies. Reporter versions are kept in lockstep with the CLI version.

Out of scope: goodflows_web (server validator, storage, purging stored rows) and goodcases (switching to the ExUnit mirror). Each is a separate change. Publishing to hex.pm or npmjs is also out of scope.

## Capabilities

### New Capabilities
- `test-timing-contract`: The `test-timings/1` wire format: the envelope, event vocabulary, core field types, the `meta` rules, filter grammar, limits and the completion rule the server derives.
- `test-reporters`: The goodflows-provided ExUnit and Jest reporters: where they write, how they map runner events onto the contract, crash-safe output, and how they are versioned and distributed through mirror repos.

### Modified Capabilities
- `test-timing-publishing`: The configured format becomes `test-timings`. Collection validates against `test-timing-contract`, and its type checks are no longer deferred to the server. The filter-tag requirement moves into the contract capability. Snapshots in the old format are refused on replay.

## Impact

- Code: `src/reporting/contract.rs` (rewritten as the authoritative schema), `src/reporting/collect.rs` (validation and envelope), `src/config.rs` (`REPORT_FORMAT` and its error text), `src/reporting/replay.rs` (old snapshots are refused through validation).
- Tests: `tests/common/mod.rs` synthetic report, `tests/reporting.rs`, `tests/inspection.rs`, the unit tests in `collect.rs` and `contract.rs`.
- New trees: `reporters/exunit/`, `reporters/jest/`, `reporters/fixtures/`.
- CI: `.github/workflows/ci.yml` gains a reporters job (Elixir and Node). `.github/workflows/release.yml` gains subtree mirror pushes, using one deploy-key secret per mirror.
- Docs: README "Test timing reports", plus a README in each reporter (each mirror's README states that it is read-only).
- Downstream: goodflows_web must adopt `test-timings/1` before a release containing this change can publish successfully. goodcases must switch to the ExUnit mirror. Both are separate changes.
