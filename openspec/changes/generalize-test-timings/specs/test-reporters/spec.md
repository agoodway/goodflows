# Spec Delta

## Purpose

Provide goodflows-maintained ExUnit and Jest reporters that turn a test run into `test-timings/1` JSONL reports that goodflows can collect, and distribute them as versioned git dependencies.

## ADDED Requirements

### Requirement: Reporters write one report per run into the report directory
Each reporter SHALL write each test run to a new file `<report_id>.jsonl` and MUST NOT overwrite an existing file. The directory SHALL be the reporter's explicit option when set, otherwise a nonempty `GOODFLOWS_REPORT_DIR`, otherwise a default: `_build/test/timings` for ExUnit and `.goodflows/test-timings` under the Jest root directory. The directory SHALL be created if it is missing.

#### Scenario: Under goodflows
- **WHEN** a reporter runs in a job whose `GOODFLOWS_REPORT_DIR` is set and no option overrides it
- **THEN** the report is written into that directory

#### Scenario: Jest watch mode
- **WHEN** Jest in watch mode starts a second run
- **THEN** the second run is written to a new file with a new `report_id`

### Requirement: Reporter output is crash-safe
Each record SHALL be written as one complete JSON line before the reporter handles the next runner event. A process killed during a run SHALL leave, at most, one incomplete final line after a valid prefix. Reporters MUST NOT synthesize `suite_finished` for a run that did not finish.

#### Scenario: Killed mid-run
- **WHEN** the test process is killed after ten tests finished
- **THEN** the file holds a valid prefix with those ten `test_finished` records, and goodflows collects it as truncated or incomplete

### Requirement: Reporter output is contract-valid and structural
Every report a reporter writes for a run that started SHALL pass goodflows collection without a rejection. Reporters MUST NOT write captured output, logs, failure messages, assertion values or stack traces. Text that exceeds a contract limit SHALL be truncated to the limit on a UTF-8 boundary. A file path outside the working directory, or one that cannot be made relative to it, SHALL be written as `null`.

#### Scenario: Very long test name
- **WHEN** a test name is 5,000 bytes
- **THEN** the written `name` is its first 4,096 bytes or fewer, ending on a character boundary

#### Scenario: Failing assertion
- **WHEN** a test fails with an assertion message
- **THEN** the record has `outcome: "failed"` and the message appears nowhere in the report

### Requirement: Reporters identify the suite and partition
`report_id` SHALL be unique per run. `suite_id` SHALL be the reporter's explicit option or a `TEST_TIMING_SUITE_ID` that follows the identifier grammar, otherwise the `report_id`. For ExUnit that option SHALL be `:test_timing_suite_id`. For Jest it SHALL be `suiteId`. `partition` and `partition_count` SHALL come from `GOODFLOWS_PARTITION` and `GOODFLOWS_PARTITION_COUNT` when set and valid, otherwise from the runner's own partitioning, otherwise `null` and `1`.

#### Scenario: Shared suite across shards
- **WHEN** three Jest shards run with `TEST_TIMING_SUITE_ID=nightly-42`
- **THEN** all three reports have `suite_id: "nightly-42"` and distinct `report_id` values

#### Scenario: Jest shard without goodflows
- **WHEN** Jest runs with `--shard=2/3` and no goodflows partition variables
- **THEN** `partition` is `"2"` and `partition_count` is `3`

#### Scenario: Unpartitioned run
- **WHEN** no partition source is present
- **THEN** `partition` is `null` and `partition_count` is `1`

### Requirement: Reporters never change test results
A reporter failure, including an unwritable report directory, MUST NOT change any test's outcome, the runner's exit status or the output of other configured reporters or formatters. The reporter SHALL print one warning to standard error and stop writing for that run.

#### Scenario: Read-only report directory
- **WHEN** the report directory cannot be created
- **THEN** the tests run, the exit status matches a run without the reporter and one warning is printed

### Requirement: ExUnit runs map onto the core model
The ExUnit reporter SHALL map each test module to a group named by the inspected module name, a test's `describe` to a one-element `path` (or `[]`), and the test's name to `name`. Excluded tests SHALL be `skipped` with `meta.skip_reason: "excluded"`, and invalid tests and modules SHALL be `errored`. ExUnit include/exclude tags with boolean values SHALL become `filters`. ExUnit-only details SHALL go in `meta`: `suite_started` carries `elixir_version`, `otp_release`, `max_cases` and `schedulers`; group events carry `async`, `setup_all` and `parameter_id`; test events carry `async` and `parameter_id`, plus `skip_reason` on `test_finished`; `suite_finished` carries `run_us`, `async_us`, `sync_us`, `load_us` and `total_us`.

#### Scenario: Excluded test
- **WHEN** a test is excluded by a tag filter
- **THEN** its `test_finished` has `outcome: "skipped"` and `meta.skip_reason: "excluded"`

#### Scenario: setup_all fails
- **WHEN** a module's `setup_all` raises
- **THEN** its tests and its `group_finished` have `outcome: "errored"`

#### Scenario: Runner identity
- **WHEN** an ExUnit run starts
- **THEN** `suite_started.runner` is `{"name": "exunit", "version": <Elixir version>}`

### Requirement: Jest runs map onto the core model
The Jest reporter SHALL map each test file to a group named by its path relative to the Jest root directory, `ancestorTitles` to `path` and the test title to `name`. Durations SHALL be Jest's millisecond values multiplied by 1,000. Statuses `pending`, `todo`, `disabled` and `focused` SHALL be `skipped`, with the original status in `meta.skip_reason`. A test file that fails to run SHALL finish its group as `errored`. Otherwise `group_finished.outcome` SHALL be `failed` when any test failed, `skipped` when every test was skipped, and `passed` otherwise. `filters` SHALL be null. `test_started.line` and `test_finished.line` SHALL be null; a result location SHALL be written only as `test_finished.meta.line`. `suite_started.meta` SHALL include `node_version`, `max_workers` and `duration_resolution_us` set to 1000. A project's display name SHALL be `meta.project` on that file's group events. `test_finished.meta` SHALL include `invocations` when Jest reports more than one. `group_finished.meta` SHALL include `load_test_environment_us`, `setup_files_us` and `slow`. The reporter SHALL write `max_failures_reached` once when bail is set and the failed-suite count reaches it.

#### Scenario: Todo test
- **WHEN** a Jest test is declared with `test.todo`
- **THEN** its `test_finished` has `outcome: "skipped"`, `duration_us: null` and `meta.skip_reason: "todo"`

#### Scenario: Test reported without a start event
- **WHEN** Jest reports a test result for which no case-start event was delivered
- **THEN** the reporter writes a `test_started` immediately before that `test_finished`, so they pair

#### Scenario: Interrupted run
- **WHEN** Jest reports the run as interrupted
- **THEN** the reporter writes `interrupted` before `suite_finished`, and `suite_finished.aborted` is true

#### Scenario: Line is known only on the result
- **WHEN** Jest's case start has no location and the result location is line 40
- **THEN** `test_started.line` and `test_finished.line` are null and `test_finished.meta.line` is 40

#### Scenario: File with a failed test
- **WHEN** a file loads and one of its tests fails
- **THEN** `group_finished.outcome` is `failed`

#### Scenario: Bail
- **WHEN** Jest is configured to bail and the failed-suite count reaches that limit
- **THEN** the reporter writes `max_failures_reached` once, before `suite_finished`

### Requirement: Reporters have no third-party runtime dependencies
The ExUnit reporter SHALL depend only on Elixir's standard library. The Jest reporter SHALL be CommonJS with no runtime dependencies and no install-time build step, and SHALL ship TypeScript declarations for its options.

#### Scenario: Installing the Jest reporter
- **WHEN** a project installs the Jest reporter from its git mirror
- **THEN** no other package is installed and no build script runs

### Requirement: Reporters are distributed through read-only mirrors in lockstep with the CLI
Each release tag SHALL publish `reporters/exunit` to `agoodway/goodflows-exunit-reporter` and `reporters/jest` to `agoodway/goodflows-jest-reporter`, each with its `main` branch and the same tag. Both reporter package versions MUST equal the CLI version. When the workflow ref is a tag, that tag's version MUST equal those package versions. A mismatch, or a mirror whose `main` is not an ancestor of the new split, MUST fail the release before anything is pushed.

#### Scenario: Release v0.6.0
- **WHEN** tag `v0.6.0` is pushed and every version is `0.6.0`
- **THEN** both mirrors receive `main` and tag `v0.6.0` containing only their reporter's files

#### Scenario: Version drift
- **WHEN** `reporters/jest/package.json` says `0.5.0` and the CLI says `0.6.0`
- **THEN** the release fails and no mirror is pushed

#### Scenario: Tag does not match the version
- **WHEN** the release ref is tag `v0.7.0` and the CLI and both reporter packages say `0.6.0`
- **THEN** the release fails and no mirror is pushed

#### Scenario: Mirror was edited directly
- **WHEN** a mirror's `main` contains a commit that is not in the new split
- **THEN** the release fails instead of overwriting it

### Requirement: CI rejects reporter output that fails collection
CI SHALL run each reporter against a fixture suite that covers passing, failing, skipped and errored tests and nested paths. CI SHALL collect the output with the CLI built from the same commit, and SHALL fail when any report is rejected or when the fixture produces no report. The fixture command's own exit status SHALL be ignored, because the suites fail on purpose. Jest SHALL be executed at its minimum peer version and at the latest version.

#### Scenario: Reporter emits a field the contract lacks
- **WHEN** a change makes the Jest reporter emit a `retries` key at the top level of `test_finished`
- **THEN** the reporters CI job fails with the rejection code

#### Scenario: Failing fixtures still pass CI
- **WHEN** the fixture suites exit nonzero and every collected report is accepted
- **THEN** the reporters job succeeds

#### Scenario: Jest version matrix
- **WHEN** the reporters job runs
- **THEN** the Jest reporter's tests and the fixture run at the minimum peer version and at the latest Jest
