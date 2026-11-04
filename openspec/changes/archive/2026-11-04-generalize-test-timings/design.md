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

`reports = ["test-timings"]` is a request to collect timing reports. The supported version is whatever `contract.rs` defines (only `/1` today). Today `config.rs::REPORT_FORMAT` is an alias of `contract::REPORT_FORMAT`. Replace that alias with its own constant `"test-timings"`, named `config::REPORT_FAMILY` (it names the family, not a wire format), and leave `contract::REPORT_FORMAT` as the wire value `"test-timings/1"`, so a future `/2` needs no config change. The unsupported-format error interpolates the config constant, so an unknown format fails naming `test-timings`; `exunit-timings` gets its own error that says it was retired, how to migrate (`reports = ["test-timings"]` and the ExUnit reporter) and links the README upgrade note.

### D7. Reporter mappings

**ExUnit** (`reporters/exunit/`, Mix app `:goodflows_exunit_reporter`, module `Goodflows.ExUnitReporter`):
- Requires Elixir `~> 1.18` and uses the built-in `JSON` module, so it has zero dependencies. goodcases is on `~> 1.20`.
- Port `GC.TestTimingFormatter` from goodcases `app/test/support/test_timing_formatter.ex`. That file is not in this repo. Keep the GenServer formatter, `configure/0`, and the directory option `:test_timing_directory`. Rename `:test_timing_run_id` to `:test_timing_suite_id` and `TEST_TIMING_RUN_ID` to `TEST_TIMING_SUITE_ID`; when neither is a valid identifier, `suite_id` is the `report_id`. Do not keep `:suite_id` or `TEST_TIMING_RUN_ID`. Do not copy `@identifier` as a module attribute. Match the grammar at the call site, so a later OTP 28 toolchain does not reject a compiled regex stored in an attribute.
- Fields:
  - `group` is `inspect(module)` and `path` is `[describe]` or `[]`.
  - `name` is the test's own name: `Atom.to_string(test.name)` without ExUnit's `test ` prefix and, inside a describe, the `<describe> ` prefix after it, as Jest writes the title alone. Doctests and other names are kept whole.
  - `runner` is `{name: "exunit", version: System.version()}`.
- `meta`:
  - suite_started: `reporter`, `reporter_version`, `filtered`, `elixir_version`, `otp_release`, `max_cases`, `schedulers`.
  - group events: `async`, `setup_all`, `parameter_id`.
  - test events: `async` (falling back to the module's, since ExUnit sets no `:async` tag on tests it does not run), `parameter_id`, and `skip_reason` on finish.
  - suite_finished: `run_us`, `async_us`, `sync_us`, `load_us`, `total_us`.
  - `parameter_id` is `:erlang.phash2(parameters, 2^32)`, so the same parameters get the same id in every run and partition.
- Group counts and outcome come from the tests the module ran plus the skipped and excluded tests the reporter saw finish for that module, since `module_finished`'s test list omits the latter.
- Durations: a test's is ExUnit's `test.time` (passed and failed tests; otherwise null), a group's is the time between the reporter receiving `module_started` and `module_finished`, and the suite's is ExUnit's run time.
- An unwritable directory logs one warning and turns the formatter into a no-op. `File.open!` crashing the formatter is not acceptable.
- Partition precedence: `GOODFLOWS_PARTITION`/`_COUNT` when both are valid, then `MIX_TEST_PARTITION` with `TEST_TIMING_PARTITIONS` when both are valid, then `null`/`1`. goodflows always sets the two variables together, so a valid partition with a missing or invalid count uses neither, rather than writing a partition with a null count; the same rule applies to the Mix pair.
- `suite_id` precedence (both reporters): the option, `TEST_TIMING_SUITE_ID`, `GOODFLOWS_RUN_ID` (the workflow run's UUID, which goodflows exports to every report-producing job), then the `report_id`; each source is used only when it follows the identifier grammar. Every report of one goodflows run, across jobs and instances, therefore shares a suite id without configuration.
- An explicit directory option wins over `GOODFLOWS_REPORT_DIR`, as before, but when both are set and differ the reporter warns (once) that goodflows will not collect the report. Making `GOODFLOWS_REPORT_DIR` win instead would silently ignore an explicit option.

**Jest** (`reporters/jest/`, package `goodflows-jest-reporter`):
- CommonJS `index.js` exporting the reporter class, plus `index.d.ts` for the options (`outputDirectory`, `suiteId`).
- `engines.node >= 18`. `peerDependencies.jest` is set to the earliest version that has `onTestCaseStart` and `onTestCaseResult` (to be verified in tasks).
- Writes use `fs.openSync(path, "wx")` and `fs.writeSync` per record, which gives crash-safety without buffering.
- Hook mapping:
  - `onRunStart` → `suite_started`. `runner.version` is read from `jest/package.json` resolved against `rootDir`, or null. `seed` comes from `globalConfig.seed`. `meta` has `reporter`, `reporter_version`, `node_version`, `max_workers`, `duration_resolution_us: 1000` and `filtered` (a name or path pattern, `onlyChanged`, `onlyFailures`, `findRelatedTests`, `lastCommit`, `changedSince` or a `filter` module). The partition comes from goodflows variables (only when both are valid, as for ExUnit), then `globalConfig.shard`. The `report_id` is time, PID, a sequence number and 4 random bytes, since containers often share PID 1.
  - `onTestFileStart` → `group_started`. `group` and `file` are the path relative to the global `rootDir` (`globalConfig.rootDir`), not the project's own `rootDir`, also in a multi-project run: that matches "relative to the Jest root directory" in the reporters spec and keeps same-named files in two projects (`packages/a/__tests__/x.test.js`, `packages/b/__tests__/x.test.js`) distinct groups. `meta.project` is the project's `displayName.name`. Open groups are keyed by project (`config.id`) and path, so one file run by two projects (a project matrix) is two groups that each pair with their own finish.
  - `onTestCaseStart` → `test_started`. `started_offset_us` comes from `startedAt` relative to the run start, and `line` is null, because Jest's start info has no location.
  - `onTestCaseResult` → `test_finished`. If no start was recorded for that case, the reporter writes a `test_started` immediately before it. `meta` has `skip_reason`, `invocations` (when greater than 1) and `line` (from `location`, when enabled).
  - Retries: with `jest.retryTimes`, Jest delivers a start and a result for every attempt, while the file's results hold only the final one. Every attempt is written as its own pair (the timings are real), attempts after the first carry `meta.invocations`, and a later attempt uncounts the failed attempt it replaces, so `suite_finished.counts` equals the sum of group counts. No `meta.retried` flag is written on earlier attempts, because a reporter that streams cannot know yet that an attempt will be retried.
  - Case events carry the context of whichever project created the test runner, not the test's own project (verified in Jest 30.5.2), so cases are keyed by path, titles and title only; delivered results and counted attempts are consumed one per final result when a file finishes, which keeps counts right when two projects run the same file at once.
  - `onTestFileResult` → `group_finished`. The outcome is `errored` when `testExecError` is set, `failed` when any test failed, `skipped` when every test was skipped, otherwise `passed`. `meta` has `load_test_environment_us`, `setup_files_us` and `slow`, each when Jest reports it (`perfStats` of a file that failed to load may lack them).
  - `onRunComplete` → `interrupted` when `wasInterrupted`, then `suite_finished`. `max_failures_reached` is written once, when `bail` is set and the failed-test count (`numFailedTests`) reaches it: Jest's own bail check (`TestScheduler`, in 29.6.0 and 30.5.2) compares `numFailedTests`, not `numFailedTestSuites`, so a file that fails to load does not bail.
  - Durations: a test's is Jest's `duration` for that attempt; a group's is `perfStats.runtime` (or `end - start`); the suite's is the parent's wall time from `onRunStart` to `onRunComplete`.
- Text is made well-formed before it is cut: a lone UTF-16 surrogate would serialize as a bare `\uD800` escape, which collection rejects as `malformed_record`, so it becomes U+FFFD (`String.prototype.toWellFormed`, with a Buffer round trip on Node 18). `fs.writeSync` is looped until each line is fully written.
- Each `onRunStart` opens a new report, which handles watch mode.
- The reporter never throws out of a hook: errors are caught, warned once, and writing stops.

### D8. Drift guard runs the real CLI over the real reporters

There is a new CI job, `reporters`, separate from the `check` matrix, on `ubuntu-24.04` with Rust, Node 24 and Elixir. `erlef/setup-beam` requires both an Elixir version and an OTP version. Pin `elixir-version` to `1.18.6` and `otp-version` to `27`. That pair is the reporter's `~> 1.18` floor on an OTP release Elixir 1.18 supports (25 through 27). `1.18.6` is the first 1.18 release whose built-in JSON decoder includes the quantity-limit fix, so the pin is not `1.18.4`. Do not use goodcases' `~> 1.20` or OTP 28 for this job. It runs:

1. Each reporter's own unit tests: `mix test` in `reporters/exunit`, and `node --test` in `reporters/jest`. Neither loads Jest (the Jest tests replay payloads recorded from Jest 30), so they run once, in the `latest` leg.
2. `scripts/check-reporters.sh`, which builds the CLI and runs `reporters/fixtures/goodflows.toml` with `--json`. That config has a `[reporting]` block and four jobs, each declaring `reports = ["test-timings"]`: `exunit`, `jest`, `jest-projects` (two projects running the same files, one with a retried flaky test) and `jest-bail` (`bail: 1`). With no API key set, the run stays local, and collection still validates every report.
3. The script then asserts, from `reports_collected` events, that each job collected exactly one report and that `rejected` is empty. Because a truncated report is still accepted by collection, it also reads each collected report and requires that the last record is `suite_finished` with the expected `aborted`, zero unfinished tests and groups, the fixture's expected suite counts and group outcomes and counts (and, for `jest-bail`, one `max_failures_reached`), and that no job printed a reporter "stopped writing" warning. The workflow's own exit status is ignored, because the fixtures fail on purpose.

The job lives in `.github/workflows/reporters.yml`, a reusable workflow (`workflow_call`) that both CI and Release call, so a release never publishes reporters the drift guard has not passed.

The fixtures (`reporters/fixtures/exunit/` as a tiny Mix project with a path dependency on `../../exunit`, and `reporters/fixtures/jest/` with a lockfile pinning Jest) cover:
- passing, failing, skipped and errored tests (`setup_all` raising; a Jest file that throws on load)
- nested describes, a todo test and a tag filter

Jest runs twice: once at the minimum peer version and once at the latest. The version matrix is what exercises the fixtures against real Jest; the unit tests do not depend on it. Third-party actions are pinned to a commit (`erlef/setup-beam` at v1.24.1); GitHub's own `actions/*` stay on their major tags.

*Alternative:* committed golden JSONL checked by `cargo test`. Rejected as the main guard because golden files go stale without anyone noticing. It is still used for Rust unit tests through a synthetic report in `tests/common/mod.rs`.

### D9. Releases split reporters into mirrors

In `release.yml`, on a tag push:

- **`verify-versions`** (first) fails unless `Cargo.toml`, `reporters/exunit/mix.exs` and `reporters/jest/package.json` agree, and unless the tag's version agrees too. CI's `release-scripts` job runs the same script without a tag on every commit, so drift is caught before a release.
- **`mirror-check`** (needs `verify-versions`; tag pushes only) runs `scripts/mirror-reporters.sh <tag> --check-keys` and then `--verify`. Together they fail the release before anything is built when a deploy key is missing (naming each missing secret and the README's "Reporter mirror deploy keys" section, also as a workflow `::error` annotation), when the tag does not name a commit on `main`, or when a mirror's `main` is not an ancestor of the new split. Nothing is pushed.
- **`reporters`** calls the reusable drift-guard workflow (D8).
- **`build`** needs `verify-versions` and `mirror-check` (skipped on `workflow_dispatch`); **`publish`** needs `build`, `reporters` and `mirror-check`.
- **`mirror-reporters`** needs `publish`, so the mirrors are pushed only after the CLI release exists; a failed publish leaves no reporter tag without a CLI release. For each `(prefix, mirror, secret)` it:
  1. checks out with `fetch-depth: 0`
  2. checks that the tag names `HEAD` and that `HEAD` is on `origin/main`
  3. runs `git subtree split --prefix <prefix> -b split`
  4. fetches the mirror's `main` (it may not exist on the first release)
  5. requires `git merge-base --is-ancestor mirror/main split` when it does exist
  6. pushes `split:main` without force, and `split:refs/tags/<tag>`

  Every mirror is checked before either is pushed. A failure after publish is retried with "Re-run failed jobs": a mirror already pushed is up to date, so the re-run pushes the rest.

  Credentials are per-mirror deploy keys with write access, in the secrets `EXUNIT_REPORTER_DEPLOY_KEY` and `JEST_REPORTER_DEPLOY_KEY`, used through `GIT_SSH_COMMAND` with a temporary key file. SSH uses `StrictHostKeyChecking=yes` against a temporary `known_hosts` holding GitHub's published host keys, so no host key is trusted on first use.
- **The keys are environment secrets.** Both live only in the `reporter-mirrors` GitHub environment, whose deployment rule admits only `v*` tags, and only `mirror-check` and `mirror-reporters` name that environment. A branch, pull request or `workflow_dispatch` run cannot read them. A `v*` tag ruleset restricts who may create, move or delete release tags, since a tag is what unlocks the keys. Creating the environment, the keys and the ruleset are manual repository-admin steps (tasks section 9).
- **A `workflow_dispatch` run is a dry run** (`mirror-dry-run`): it builds, runs the drift guard, and prints the split SHAs and the source check without the keys, so it never fetches the mirrors.
- **Releases come from `main` only (hotfix policy).** A tag must be on `main`; a tag on a maintenance branch fails `mirror-check`, because the mirrors' `main` could not fast-forward to its split, and a later release from `main` would then fail the ancestry check. A fix for an older release ships as the next patch version from `main`. If maintenance releases are ever needed, they need per-line mirror branches, a separate decision.
- **Split SHAs are deterministic** for the same history, so fast-forwards hold across releases. Each reporter's `README.md` (which becomes the mirror's README) states that the mirror is read-only and that changes go to `agoodway/goodflows`.
- `scripts/test-release-scripts.sh` tests both scripts against local bare repositories (`MIRROR_URL_BASE`, with a local bare `origin`): version drift, tag mismatch, missing keys, first and fast-forward releases, `--verify` and `--dry-run`, an edited mirror, a re-run after a push, and tags off `main` or off `HEAD`.

Consumers (`v0.6.0` is the first release containing the reporters; `v0.5.0` was tagged before this change, so no mirror has it):
- Mix: `{:goodflows_exunit_reporter, github: "agoodway/goodflows-exunit-reporter", tag: "v0.6.0", only: :test}`.
- npm: `"goodflows-jest-reporter": "github:agoodway/goodflows-jest-reporter#v0.6.0"`.

Both reporters ship a `LICENSE` (MIT, as the CLI) and declare it (`licenses` in `mix.exs`, `license` in `package.json`), since each mirror is distributed on its own.

## Implementation notes

Decisions made while implementing, after `record-run-history` was archived. That change touched no reporting code, so nothing here conflicts with it.

- **Constants.** `CONTRACT_VERSION` stays the change name `generalize-test-timings`: it is the id of the OpenSpec change that defines the contract, sent in the publisher's User-Agent (`test-reports/<CONTRACT_VERSION>`), and the server pins its mirror to it. `REPORT_FORMAT` is the wire value the server validates and changes only for an incompatible wire change; its doc says it is `REPORT_FAMILY` plus a major version. The event table in `contract.rs` is the only list of events (`shape()` looks events up in it), and one helper holds the identifier character rule.
- **Validation order.** Collection validates each record as its line is decoded, so a report with several faults gets the code of the first faulty line in file order, and nothing after that line is decoded. A decode or size error on a later line no longer wins over a validation error on an earlier one. A maximum line length and a decode-time string cap were not added, because each would need a new code or change existing codes; the 33 MiB source cap still bounds a report.
- **Error codes.** Partition and identifier grammar failures keep `invalid_partition`, `invalid_partition_count` and `invalid_identifier`. A wrong `format` (in a record or the envelope) is `unsupported_format`; the envelope check used to return `invalid_envelope`, and replay still counts either as `invalid_snapshot`. Unknown keys inside `counts`, `runner`, `filters` or a filter entry are `unsupported_field`, as before. An empty text value, a bad `runner.name` and a path over 32 entries are `invalid_field`; a string over its byte limit (text, `started_at`, `runner.version`) is `field_too_long`. A `suite_started` whose `format` is not `test-timings/1` is rejected with `unsupported_format` before any other key is checked, so a retired producer gets the format code. Core keys are typed in table order, so the code reported for a record with several faults is deterministic.
- **Negative integers.** Decoding now rejects negative integers beyond -(2^53-1) with `integer_out_of_range`, matching the positive bound. Before, their range was left to the server.
- **Timestamps.** `started_at` accepts `YYYY-MM-DDTHH:MM:SS[.fraction]` followed by `Z`, `+00:00` or `-00:00`, with calendar-valid dates.
- **Pairing.** A started event whose `execution_id` was already used by that kind, even after it finished, is `unpaired_event`. Group and test ids are separate namespaces.
- **ExUnit reporter.** ExUnit can report a finished test it never started (for example after a failing `setup_all`). goodcases' formatter dropped those results; this reporter writes a `test_started` first, as the Jest reporter does. ExUnit's runner always starts a module before finishing it, but an unmatched `module_finished` (from a custom runner, say) likewise gets a `group_started` written first instead of being dropped. ExUnit gives skipped, excluded and invalid tests `parameters: %{}` even in a parameterized module, so the skip tally for group counts is kept per `{module, parameters}` with the module's shared `%{}` entry counted toward each of its parameter groups (filters never see parameters, so every parameter group skips the same tests); the shared entry is cleared once none of the module's groups is open. `configure/0` works with no formatters configured, and a throw or exit in a handler disables the reporter with one warning, as a raise does. U+0000 in text becomes U+FFFD, as in the Jest reporter, because collection rejects it. `@tag :skip` tests get `meta.skip_reason: "skipped"`. A group is `errored` when its `setup_all` failed or a test errored, `failed` when a test failed, `skipped` when every test was skipped, and `passed` otherwise. `suite_finished.duration_us` is ExUnit's `run` time. A todo in the ExUnit fixture is a test without a body, which ExUnit reports as not implemented (failed).
- **Jest reporter.** Jest delivers no case events for skipped tests (`test.skip`, a name filter), so `onTestFileResult` writes a start and finish for every result in the file that `onTestCaseResult` did not deliver. A group whose file is outside `rootDir` has `file: null` and is named by the file's basename. The Jest fixture uses a `testNamePattern` that excludes one test as its tag filter, because Jest has no tags.
- **Minimum Jest.** `onTestCaseStart` first ships in Jest 29.6.0 (`@jest/core` 29.5.0 lacks it; `onTestCaseResult` is older), so `peerDependencies.jest` is `>=29.6.0`. CI installs the minimum with `npm install --before=2023-07-05 jest@29.6.0`, so the whole tree resolves as of that release.
- **Versions.** Both reporters start at the current CLI version, `0.5.0`, so `scripts/verify-versions.sh` passes on this branch. `v0.5.0` was tagged before this change, so the first release containing the reporters, and the first mirror tag, is `v0.6.0`; the README and reporter READMEs use it in their install snippets. That release bumps `Cargo.toml`, `reporters/exunit/mix.exs` (`@version`) and `reporters/jest/package.json` together, which `verify-versions` enforces.
- **Release scripts.** The version check and the mirror push live in `scripts/verify-versions.sh` and `scripts/mirror-reporters.sh`, so they can be run locally. The mirror script splits and checks both mirrors before pushing either. A `workflow_dispatch` run of `release.yml` is always a dry run and never receives the deploy keys, so it does not fetch the mirrors (the script still fetches a mirror in `--dry-run` when a key happens to be set locally). `--verify` runs every check with the keys and pushes nothing. `MIRROR_URL_BASE` points the script at local bare repositories and `MIRROR_SOURCE_REMOTE`/`MIRROR_SOURCE_BRANCH` at the branch a tag must be on, which is how `scripts/test-release-scripts.sh` exercises every path in CI.
- **CI.** Both reporters' own unit tests (`mix format --check-formatted`, `mix compile --warnings-as-errors`, `mix test`, and the Jest reporter's `node --test`) run only in the `latest` leg of the `reporters` Jest matrix, since none of them loads Jest; both legs run every fixture through `check-reporters.sh`. The Jest fixture's default config sets `roots` to its `__tests__/`, so the `projects/` and `bail/` suites run only under their own configs.
- **Contract spec link.** The README links the contract spec at `openspec/specs/test-timing-contract/spec.md`, where it lives once this change is archived; the delta is `specs/test-timing-contract/spec.md`, so the synced path matches.

## Future contract revisions

Known gaps that `test-timings/1` leaves for a later `/2`. None blocks this change, and none can be added to `/1` without breaking the closed key sets:

- **Tests are not tied to a group execution.** A test names its group by `group` text, not by the group's `execution_id`, so when the same group runs twice at once (one Jest file in two projects, an ExUnit module with two parameter sets) the server cannot tell which execution a test belongs to. `/2` should add `group_execution_id` to test events.
- **No reporter identity in the core.** `runner` names the test runner, not the reporter that wrote the report. Both reporters now write `meta.reporter` and `meta.reporter_version` on `suite_started` (flat keys, which `/1`'s `meta` rule allows), so the server can attribute data to a reporter version today; `/2` could promote them to a core `reporter {name, version}`.
- **Selection is a boolean.** `meta.filtered` says that a run did not run every test, but not how; name and path filters have no core field.
- **Retries are implicit.** Attempts are separate test executions linked only by identity and `meta.invocations`; `/2` could add an explicit attempt number.

## Risks / Trade-offs

- [The server still speaks `exunit-timings` when this CLI ships, so uploads get 422 and are recorded as `rejected_by_server`.] → Release only after the goodflows_web change is deployed. The README states the minimum server. Deleting the rejection markers makes those reports replayable once the server catches up.
- [Pending `exunit-timings` snapshots in existing spools become `invalid_snapshot`.] → This is accepted for a greenfield product and called out in the README upgrade note. Raw JSONL stays on disk.
- [Strict pairing and unfinished-count equality reject reports from a buggy or third-party producer.] → These rules guarantee coherent data for completion. Both shipped reporters are held to them by D8, and the rejection codes tell third parties exactly what broke.
- [Jest hook availability and payload shapes vary by version.] → A pinned minimum peer version, the CI matrix at minimum and latest, and the synthesized `test_started` fallback.
- [Jest durations are only millisecond-precise.] → `meta.duration_resolution_us: 1000` records the precision. Consumers should not compare Jest and ExUnit timings at sub-millisecond precision.
- [The Jest reporter runs in the parent process, so its receipt times lag the workers.] → `started_offset_us` and `duration_us` use Jest's own `startedAt`/`duration`. Only `received_offset_us` reflects receipt, as the contract defines it.
- [The goodflows_web change copies this contract incorrectly.] → The contract spec's scenarios are written to be ported as server tests one-for-one, and D1's table is the single source.
- [Mirror deploy keys are a new secret surface.] → One key per mirror, with write access only to that mirror, stored only as secrets of the `reporter-mirrors` environment that only `v*` tags may deploy to, read only by the tag-triggered `mirror-check` and `mirror-reporters` jobs (never by a dry run), with release tags limited by a ruleset.
- [`git subtree split` rewrites can diverge if reporter history is ever rewritten in the monorepo.] → The ancestry check fails the release instead of force-pushing. A deliberate reset of a mirror is a manual operation.

## Migration Plan

1. Merge this change. `main` emits and accepts only `test-timings/1`.
2. Land and deploy the goodflows_web change that implements `test-timing-contract`.
3. Add the two deploy-key secrets (tasks section 9; README "Reporter mirror deploy keys"), confirm them with a `workflow_dispatch` dry run, bump all three versions to `0.6.0`, then tag `v0.6.0`. The mirrors receive their first `main` and the tag.
4. The goodcases change replaces `GC.TestTimingFormatter` with the mirror dependency and `reports = ["test-timings"]`.

Rollback: revert the CLI release. Because the server is switching formats at the same time, rolling back one side without the other leaves uploads rejected but local data intact, which is acceptable while greenfield.
