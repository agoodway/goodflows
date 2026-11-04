# Design

## Context

See proposal.md for motivation. Current state:

- `src/reporting/contract.rs` is a copy of the server's `exunit-timings` vocabulary: per-event key allowlists, ExUnit outcomes, `times_us` and `files` bucket shapes, and length tables.
- `collect.rs::validate_records` checks keys, identities, order, offsets, string lengths and nested allowlists. It explicitly leaves "full type validation" to the server, so a snapshot with a wrong-typed field can be frozen and then get a permanent 422 (recorded as `rejected_by_server`).
- The record key `run_id` feeds the envelope's `suite_id`.
- Goodflows rewrites `partition`/`partition_count` in the envelope and the first record for instances (`assign_partition`). That behavior is unchanged.
- The ExUnit producer is `GC.TestTimingFormatter` in goodcases (386 lines, Jason-based). It is copied into this repo, not moved. goodcases changes separately.
- CI (`ci.yml`) already installs Node 24. Elixir is not installed anywhere. `release.yml` builds tarballs per target and then runs `gh release create`.

## Goals / Non-Goals

**Goals:**
- One contract definition that a server engineer can implement from this document and `specs/test-timing-contract/spec.md` alone.
- Any snapshot that collection freezes is one a conforming server accepts. No more deferred type errors.
- The reporters cannot drift from the contract without CI failing.

**Non-Goals:**
- Server changes, purging stored rows, goodcases migration (separate changes).
- Reading stored `exunit-timings` data, or translating it.
- Registry publishing (hex.pm or npmjs), and reporters for other runners. The model is meant to admit them, but none are built.
- Changes to publishing, retries, credentials, manifests or the partition rewrite.

## Decisions

### D1. goodci owns the contract, and `contract.rs` becomes a typed schema

`contract.rs` stops being a mirror and becomes the definition. `CONTRACT_VERSION` becomes `"generalize-test-timings"`, the name of this change, and `REPORT_FORMAT` becomes `"test-timings/1"`. The module doc is inverted: the server mirrors this module and its spec. The key allowlists (`&[&str]`) are replaced by a per-event table of `(key, FieldType)`, where `FieldType` covers the handful of shapes the contract uses:

```
Text            1..=4096 bytes          Identifier      identifier grammar
OptText         Text | null             Partition       null | 1..=64 [A-Za-z0-9._-]
Timestamp       RFC 3339 UTC, <=64 B    PartitionCount  null | 1..=2^53-1
UInt            0..=2^53-1              ExecutionId     1..=2^53-1
OptUInt         UInt | null             Line            null | 1..=2^53-1
OptInt          null | |n|<=2^53-1      Outcome         passed|failed|skipped|errored
Bool            true | false            Counts          {outcome: UInt} subset
Path            [Text; 0..=32]          Runner          {name: Identifier, version: null | <=128 B}
Filters         null | {include, exclude}  (existing grammar)
Format          == "test-timings/1"
```

The same table drives three things: required keys (every core key must be present), unknown-key rejection, and typing. The server can transcribe it directly.

*Alternative:* keep the allowlists and add ad hoc type checks in `validate_nested`. Rejected because it leaves the contract spread across two files and invites a third partial copy in the server.

### D2. Full local validation, including pairing and unfinished counts

`validate_records` becomes a single pass over the records with two maps (open groups and open tests, each keyed by `execution_id` to the core values of the started event). It:

1. Types every key against D1.
2. Checks `meta` (D4).
3. Enforces order: `suite_started` first and only first, `suite_finished` last.
4. Pairs each finished event with an open started event that has identical core values, otherwise `unpaired_event`.
5. Checks that `suite_finished.unfinished_*` equals the sizes of the open maps, otherwise `unfinished_mismatch`.

Memory is bounded by the 100,000-record limit that already exists. Duplicate `execution_id` among started events of one kind is `unpaired_event`.

New codes: `invalid_meta`, `unpaired_event`, `unfinished_mismatch`. Existing codes keep their meanings. Type errors on known keys are `invalid_field`, and absent core keys are `missing_field`.

*Alternative:* leave pairing to the server. Rejected because a mismatch would then become an unreplayable `rejected_by_server`, which is exactly the class of failure D1 removes.

### D3. The core field set

Common keys on every record: `event`, `suite_id`, `report_id`, `received_offset_us` (UInt), plus optional `meta`.

| event | core keys (all required) |
|---|---|
| `suite_started` | `format` Format, `started_at` Timestamp, `runner` Runner, `partition` Partition, `partition_count` PartitionCount, `seed` OptInt, `filters` Filters |
| `group_started` | `group` Text, `file` OptText, `execution_id` ExecutionId, `started_offset_us` UInt |
| `group_finished` | group_started keys + `duration_us` OptUInt, `outcome` Outcome, `counts` Counts |
| `test_started` | `group` Text, `path` Path, `name` Text, `file` OptText, `line` Line, `execution_id` ExecutionId, `started_offset_us` UInt |
| `test_finished` | test_started keys + `duration_us` OptUInt, `outcome` Outcome |
| `suite_finished` | `aborted` Bool, `unfinished_tests` UInt, `unfinished_groups` UInt, `counts` Counts, `duration_us` OptUInt |
| `interrupted`, `max_failures_reached` | (common keys only) |

Envelope and `report` keys are unchanged from today, with `format: "test-timings/1"`. The envelope `suite_id` now comes from record `suite_id` instead of `run_id`.

Notes on what was dropped or renamed, and why:
- `module` → `group`, because it names the scheduling unit (ExUnit module, Jest file).
- `describe` → `path[]`, because Jest nests describes to any depth.
- `description` is dropped, because it duplicates `name`.
- `async`, `parameter_id`, `setup_all`, `max_cases`, `schedulers`, `elixir_version` and `otp_release` move to `meta`.
- `observed_duration_us` → `duration_us` on groups.
- `times_us` moves to `meta`.
- `files` buckets are dropped, because the server can derive them from test events.
- `unfinished_modules` → `unfinished_groups`.
- Outcome `excluded` becomes `skipped` with `meta.skip_reason: "excluded"`. Outcome `invalid` becomes `errored`.

*Alternative:* make `test_started` optional, since some runners only report results. Rejected because `unfinished_tests` and the killed-mid-run case depend on it. The Jest reporter synthesizes a start when Jest skips the start hook (D7).

### D4. `meta` is flat and bounded

`meta` is an object of at most 32 keys in the identifier grammar. Values are null, Bool, OptInt-range integers, or strings of at most 256 bytes. It is allowed on every event and is not compared during pairing. Any violation is `invalid_meta`. The record depth bound (16) is unaffected because `meta` adds only one level.

Why flat: a single rule is easy for the server to copy, and structured payloads (logs, exceptions, parameter maps) can't be smuggled in through nesting. Test names were always free-form text, so short strings don't weaken the privacy rule. The rule is about what kind of payload is sent, which the reporters spec states.

*Alternatives:* bounded nested values, which need depth and size rules per value; or per-runner server profiles, which need a contract bump for each runner. Both were rejected during exploration.

### D5. `filters` stays core and nullable

The existing tag grammar and the 256-entry limit carry over unchanged, and so does the `unsupported_filter` code. Jest sends `null`. Runners with boolean tags (ExUnit today; pytest markers or RSpec later) use it.

### D6. Configuration names the family, data names the version

`reports = ["test-timings"]` is a request to collect timing reports. The supported version is whatever `contract.rs` defines (only `/1` today). Today `config.rs::REPORT_FORMAT` is an alias of `contract::REPORT_FORMAT`. Replace that alias with its own constant `"test-timings"`, and leave `contract::REPORT_FORMAT` as the wire value `"test-timings/1"`, so a future `/2` needs no config change. The unsupported-format error interpolates the config constant, so `exunit-timings` fails naming `test-timings`.

### D7. Reporter mappings

**ExUnit** (`reporters/exunit/`, Mix app `:goodflows_exunit_reporter`, module `Goodflows.ExUnitReporter`):
- Requires Elixir `~> 1.18` and uses the built-in `JSON` module, so it has zero dependencies. goodcases is on `~> 1.20`.
- Port `GC.TestTimingFormatter` from goodcases `app/test/support/test_timing_formatter.ex`. That file is not in this repo. Keep the GenServer formatter, `configure/0`, and the directory option `:test_timing_directory`. Rename `:test_timing_run_id` to `:test_timing_suite_id` and `TEST_TIMING_RUN_ID` to `TEST_TIMING_SUITE_ID`; when neither is a valid identifier, `suite_id` is the `report_id`. Do not keep `:suite_id` or `TEST_TIMING_RUN_ID`. Do not copy `@identifier` as a module attribute. Match the grammar at the call site, so a later OTP 28 toolchain does not reject a compiled regex stored in an attribute.
- Fields:
  - `group` is `inspect(module)` and `path` is `[describe]` or `[]`.
  - `name` is `Atom.to_string(test.name)`.
  - `runner` is `{name: "exunit", version: System.version()}`.
- `meta`:
  - suite_started: `elixir_version`, `otp_release`, `max_cases`, `schedulers`.
  - group events: `async`, `setup_all`, `parameter_id`.
  - test events: `async`, `parameter_id`, and `skip_reason` on finish.
  - suite_finished: `run_us`, `async_us`, `sync_us`, `load_us`, `total_us`.
- An unwritable directory logs one warning and turns the formatter into a no-op. `File.open!` crashing the formatter is not acceptable.
- Partition precedence: `GOODFLOWS_PARTITION`/`_COUNT`, then `MIX_TEST_PARTITION` with `TEST_TIMING_PARTITIONS`, then `null`/`1`.

**Jest** (`reporters/jest/`, package `goodflows-jest-reporter`):
- CommonJS `index.js` exporting the reporter class, plus `index.d.ts` for the options (`outputDirectory`, `suiteId`).
- `engines.node >= 18`. `peerDependencies.jest` is set to the earliest version that has `onTestCaseStart` and `onTestCaseResult` (to be verified in tasks).
- Writes use `fs.openSync(path, "wx")` and `fs.writeSync` per record, which gives crash-safety without buffering.
- Hook mapping:
  - `onRunStart` → `suite_started`. `runner.version` is read from `jest/package.json` resolved against `rootDir`, or null. `seed` comes from `globalConfig.seed`. `meta` has `node_version`, `max_workers` and `duration_resolution_us: 1000`. The partition comes from goodflows variables, then `globalConfig.shard`.
  - `onTestFileStart` → `group_started`. `group` and `file` are the path relative to `rootDir`. `meta.project` is the project's `displayName.name`.
  - `onTestCaseStart` → `test_started`. `started_offset_us` comes from `startedAt` relative to the run start, and `line` is null, because Jest's start info has no location.
  - `onTestCaseResult` → `test_finished`. If no start was recorded for that case, the reporter writes a `test_started` immediately before it. `meta` has `skip_reason`, `invocations` (when greater than 1) and `line` (from `location`, when enabled).
  - `onTestFileResult` → `group_finished`. The outcome is `errored` when `testExecError` is set, `failed` when any test failed, `skipped` when every test was skipped, otherwise `passed`. `meta` has `load_test_environment_us`, `setup_files_us` and `slow`.
  - `onRunComplete` → `interrupted` when `wasInterrupted`, then `suite_finished`. `max_failures_reached` is written once, when `bail` is set and the failed-suite count reaches it.
- Each `onRunStart` opens a new report, which handles watch mode.
- The reporter never throws out of a hook: errors are caught, warned once, and writing stops.

### D8. Drift guard runs the real CLI over the real reporters

There is a new CI job, `reporters`, separate from the `check` matrix, on `ubuntu-24.04` with Rust, Node 24 and Elixir. `erlef/setup-beam` requires both an Elixir version and an OTP version. Pin `elixir-version` to `1.18.6` and `otp-version` to `27`. That pair is the reporter's `~> 1.18` floor on an OTP release Elixir 1.18 supports (25 through 27). `1.18.6` is the first 1.18 release whose built-in JSON decoder includes the quantity-limit fix, so the pin is not `1.18.4`. Do not use goodcases' `~> 1.20` or OTP 28 for this job. It runs:

1. Each reporter's own unit tests: `mix test` in `reporters/exunit`, and `node --test` in `reporters/jest`.
2. `scripts/check-reporters.sh`, which builds the CLI and runs `reporters/fixtures/goodflows.toml` with `--json`. That config has a `[reporting]` block and two jobs, `exunit` and `jest`, each declaring `reports = ["test-timings"]` and running the fixture suites. With no API key set, the run stays local, and collection still validates every report.
3. The script then asserts, from `reports_collected` events, that each job collected at least one report and that `rejected` is empty. The workflow's own exit status is ignored, because the fixtures fail on purpose.

The fixtures (`reporters/fixtures/exunit/` as a tiny Mix project with a path dependency on `../../exunit`, and `reporters/fixtures/jest/` with a lockfile pinning Jest) cover:
- passing, failing, skipped and errored tests (`setup_all` raising; a Jest file that throws on load)
- nested describes, a todo test and a tag filter

Jest runs twice: once at the minimum peer version and once at the latest.

*Alternative:* committed golden JSONL checked by `cargo test`. Rejected as the main guard because golden files go stale without anyone noticing. It is still used for Rust unit tests through a synthetic report in `tests/common/mod.rs`.

### D9. Releases split reporters into mirrors

In `release.yml`:

- **A new `verify-versions` job**, which `build` depends on. It fails unless `Cargo.toml`, `reporters/exunit/mix.exs` and `reporters/jest/package.json` agree, and, when the workflow ref is a tag, unless that tag's version agrees too.
- **A new `mirror-reporters` job**, which needs `build`; `publish` then needs `mirror-reporters`. For each `(prefix, mirror, secret)` it:
  1. checks out with `fetch-depth: 0`
  2. runs `git subtree split --prefix <prefix> -b split`
  3. fetches the mirror's `main` (it may not exist on the first release)
  4. requires `git merge-base --is-ancestor mirror/main split` when it does exist
  5. pushes `split:main` without force, and `split:refs/tags/<tag>`

  Credentials are per-mirror deploy keys with write access, in the secrets `EXUNIT_REPORTER_DEPLOY_KEY` and `JEST_REPORTER_DEPLOY_KEY`, used through `GIT_SSH_COMMAND` with a temporary key file.
- **Split SHAs are deterministic** for the same history, so fast-forwards hold across releases. Each reporter's `README.md` (which becomes the mirror's README) states that the mirror is read-only and that changes go to `agoodway/goodflows`.

Consumers:
- Mix: `{:goodflows_exunit_reporter, github: "agoodway/goodflows-exunit-reporter", tag: "v0.6.0", only: :test}`.
- npm: `"goodflows-jest-reporter": "github:agoodway/goodflows-jest-reporter#v0.6.0"`.

## Risks / Trade-offs

- [The server still speaks `exunit-timings` when this CLI ships, so uploads get 422 and are recorded as `rejected_by_server`.] → Release only after the goodflows_web change is deployed. The README states the minimum server. Deleting the rejection markers makes those reports replayable once the server catches up.
- [Pending `exunit-timings` snapshots in existing spools become `invalid_snapshot`.] → This is accepted for a greenfield product and called out in the README upgrade note. Raw JSONL stays on disk.
- [Strict pairing and unfinished-count equality reject reports from a buggy or third-party producer.] → These rules guarantee coherent data for completion. Both shipped reporters are held to them by D8, and the rejection codes tell third parties exactly what broke.
- [Jest hook availability and payload shapes vary by version.] → A pinned minimum peer version, the CI matrix at minimum and latest, and the synthesized `test_started` fallback.
- [Jest durations are only millisecond-precise.] → `meta.duration_resolution_us: 1000` records the precision. Consumers should not compare Jest and ExUnit timings at sub-millisecond precision.
- [The Jest reporter runs in the parent process, so its receipt times lag the workers.] → `started_offset_us` and `duration_us` use Jest's own `startedAt`/`duration`. Only `received_offset_us` reflects receipt, as the contract defines it.
- [The goodflows_web change copies this contract incorrectly.] → The contract spec's scenarios are written to be ported as server tests one-for-one, and D1's table is the single source.
- [Mirror deploy keys are a new secret surface.] → One key per mirror, write access only to that mirror, and used only in the tag-triggered job.
- [`git subtree split` rewrites can diverge if reporter history is ever rewritten in the monorepo.] → The ancestry check fails the release instead of force-pushing. A deliberate reset of a mirror is a manual operation.

## Migration Plan

1. Merge this change. `main` emits and accepts only `test-timings/1`.
2. Land and deploy the goodflows_web change that implements `test-timing-contract`.
3. Add the two deploy-key secrets, then tag the release. The mirrors receive their first `main` and the tag.
4. The goodcases change replaces `GC.TestTimingFormatter` with the mirror dependency and `reports = ["test-timings"]`.

Rollback: revert the CLI release. Because the server is switching formats at the same time, rolling back one side without the other leaves uploads rejected but local data intact, which is acceptable while greenfield.
