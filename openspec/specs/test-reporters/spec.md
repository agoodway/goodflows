# test-reporters Specification

## Purpose

Provide goodflows-maintained ExUnit and Jest reporters that turn a test run into `test-timings/1` JSONL reports that goodflows can collect, and distribute them as versioned git dependencies.

## Requirements

### Requirement: Reporters write one report per run into the report directory
Each reporter SHALL write each test run to a new file `<report_id>.jsonl` and MUST NOT overwrite an existing file. The directory SHALL be the reporter's explicit option when set, otherwise a nonempty `GOODFLOWS_REPORT_DIR`, otherwise a default: `_build/test/timings` for ExUnit and `.goodflows/test-timings` under the Jest root directory. The directory SHALL be created if it is missing. When the explicit option and a nonempty `GOODFLOWS_REPORT_DIR` are both set and name different directories, the reporter SHALL still write where the option says and SHALL print one warning to standard error that goodflows will not collect the report.

#### Scenario: Under goodflows
- **WHEN** a reporter runs in a job whose `GOODFLOWS_REPORT_DIR` is set and no option overrides it
- **THEN** the report is written into that directory

#### Scenario: Option overrides goodflows
- **WHEN** the reporter's directory option names `custom/` and `GOODFLOWS_REPORT_DIR` names another directory
- **THEN** the report is written into `custom/`, one warning names both directories, and the run's results are unchanged

#### Scenario: Jest watch mode
- **WHEN** Jest in watch mode starts a second run
- **THEN** the second run is written to a new file with a new `report_id`

### Requirement: Reporter output is crash-safe
Each record SHALL be written as one complete JSON line before the reporter handles the next runner event. A process killed during a run SHALL leave, at most, one incomplete final line after a valid prefix. Reporters MUST NOT synthesize `suite_finished` for a run that did not finish.

#### Scenario: Killed mid-run
- **WHEN** the test process is killed after ten tests finished
- **THEN** the file holds a valid prefix with those ten `test_finished` records, and goodflows collects it as truncated or incomplete

### Requirement: Reporter output is contract-valid and structural
Every report a reporter writes for a run that started SHALL pass goodflows collection without a rejection. Reporters MUST NOT write captured output, logs, failure messages, assertion values or stack traces. U+0000 in text, and in Jest a lone UTF-16 surrogate, SHALL be replaced with U+FFFD; empty text SHALL be written as `-`; a `path` longer than 32 entries SHALL keep its first 32; and text that exceeds a contract limit SHALL then be truncated to the limit on a UTF-8 boundary. A file path outside the working directory, or one that cannot be made relative to it, SHALL be written as `null`.

#### Scenario: Very long test name
- **WHEN** a test name is 5,000 bytes
- **THEN** the written `name` is its first 4,096 bytes or fewer, ending on a character boundary

#### Scenario: Failing assertion
- **WHEN** a test fails with an assertion message
- **THEN** the record has `outcome: "failed"` and the message appears nowhere in the report

#### Scenario: NUL in a test name
- **WHEN** an ExUnit or Jest test name or describe title contains U+0000
- **THEN** the written text has U+FFFD in its place and the report passes collection

#### Scenario: Lone surrogate in a Jest title
- **WHEN** a Jest test title contains an unpaired UTF-16 surrogate
- **THEN** the written `name` has U+FFFD in its place and the report passes collection

#### Scenario: Deep describe nesting
- **WHEN** a Jest test is nested in 40 describe blocks
- **THEN** its `path` holds the outermost 32 titles

### Requirement: Reporters identify the suite and partition
`report_id` SHALL be unique per run, including across processes that share a process id (Jest's report id includes random bytes). `suite_id` SHALL be the first of these that follows the identifier grammar: the reporter's explicit option, `TEST_TIMING_SUITE_ID`, `GOODFLOWS_RUN_ID` (which goodflows sets for every report-producing job), otherwise the `report_id`. For ExUnit that option SHALL be `:test_timing_suite_id`. For Jest it SHALL be `suiteId`. `partition` and `partition_count` SHALL come from `GOODFLOWS_PARTITION` and `GOODFLOWS_PARTITION_COUNT` when both are set and valid. When either is missing or invalid, neither SHALL be used: both values SHALL come from the runner's own partitioning when it supplies both a valid partition and a valid count (ExUnit: `MIX_TEST_PARTITION` with `TEST_TIMING_PARTITIONS`; Jest: `--shard`), otherwise `null` and `1`. Each `suite_started.meta` SHALL carry `reporter` (the package name), `reporter_version` (the package version) and `filtered`, a boolean that is true when the runner was asked to run only some tests (ExUnit include, exclude or only-test filters; Jest name or path patterns, changed-file, related-test or only-failures selection), and false otherwise. Sharding is a partition, not a filter.

#### Scenario: Shared suite across shards
- **WHEN** three Jest shards run with `TEST_TIMING_SUITE_ID=nightly-42`
- **THEN** all three reports have `suite_id: "nightly-42"` and distinct `report_id` values

#### Scenario: Jest shard without goodflows
- **WHEN** Jest runs with `--shard=2/3` and no goodflows partition variables
- **THEN** `partition` is `"2"` and `partition_count` is `3`

#### Scenario: Unpartitioned run
- **WHEN** no partition source is present
- **THEN** `partition` is `null` and `partition_count` is `1`

#### Scenario: goodflows partition without a valid count
- **WHEN** `GOODFLOWS_PARTITION=1` is set, `GOODFLOWS_PARTITION_COUNT` is missing or `0`, and Jest runs with `--shard=2/3`
- **THEN** `partition` is `"2"` and `partition_count` is `3`, not `"1"` with a null count

#### Scenario: ExUnit partition without a count
- **WHEN** `MIX_TEST_PARTITION=2` is set without a valid `TEST_TIMING_PARTITIONS` and no goodflows partition variables are set
- **THEN** `partition` is `null` and `partition_count` is `1`

#### Scenario: Suite identity under goodflows
- **WHEN** a reporter runs under goodflows with no suite option and no `TEST_TIMING_SUITE_ID`
- **THEN** `suite_id` is the `GOODFLOWS_RUN_ID` value, shared by every report of that goodflows run

#### Scenario: Reporter identity
- **WHEN** a run starts
- **THEN** `suite_started.meta` has `reporter`, `reporter_version` equal to the reporter package version, and `filtered`

### Requirement: Reporters never change test results
A reporter failure, including an unwritable report directory, MUST NOT change any test's outcome, the runner's exit status or the output of other configured reporters or formatters. The reporter SHALL print one warning to standard error and stop writing for that run.

#### Scenario: Read-only report directory
- **WHEN** the report directory cannot be created
- **THEN** the tests run, the exit status matches a run without the reporter and one warning is printed

### Requirement: ExUnit runs map onto the core model
The ExUnit reporter SHALL map each test module (per parameter set) to a group named by the inspected module name, a test's `describe` to a one-element `path` (or `[]`), and the test's own name to `name`, without ExUnit's `test ` and `<describe> ` prefixes (other names, such as doctests, are kept whole). Excluded tests SHALL be `skipped` with `meta.skip_reason: "excluded"`, and invalid tests and modules SHALL be `errored`. `group_finished.counts` and `outcome` SHALL include the module's skipped and excluded tests: a group is `errored` when its `setup_all` failed or a test errored, `failed` when a test failed, `skipped` when every test was skipped, and `passed` otherwise. A test's `duration_us` SHALL be ExUnit's test time for passed and failed tests and `null` otherwise; a group's SHALL be the time between the reporter receiving the module's start and finish; the suite's SHALL be ExUnit's run time. When ExUnit finishes a test or module for which no start was delivered, the reporter SHALL write the matching `test_started` or `group_started` immediately before the finish, so they pair. ExUnit include/exclude tags with boolean values SHALL become `filters`. ExUnit-only details SHALL go in `meta`: `suite_started` carries `elixir_version`, `otp_release`, `max_cases` and `schedulers` (besides `reporter`, `reporter_version` and `filtered`); group events carry `async`, `setup_all` and `parameter_id`; test events carry `async` (the module's when ExUnit gives the test none, as for skipped tests) and `parameter_id`, plus `skip_reason` on `test_finished`; `suite_finished` carries `run_us`, `async_us`, `sync_us`, `load_us` and `total_us`. `parameter_id` SHALL be a stable hash of the parameter map (the same parameters give the same id in every run and partition), or `null` without parameters.

#### Scenario: Excluded test
- **WHEN** a test is excluded by a tag filter
- **THEN** its `test_finished` has `outcome: "skipped"` and `meta.skip_reason: "excluded"`

#### Scenario: setup_all fails
- **WHEN** a module's `setup_all` raises
- **THEN** its tests and its `group_finished` have `outcome: "errored"`

#### Scenario: Every test in a module is skipped
- **WHEN** both tests of a module are tagged `@tag :skip`
- **THEN** its `group_finished` has `outcome: "skipped"` and `counts: {"skipped": 2}`

#### Scenario: Test name inside a describe
- **WHEN** ExUnit names a test `test math adds` with describe `math`
- **THEN** `path` is `["math"]` and `name` is `adds`

#### Scenario: Module finished without a start
- **WHEN** ExUnit delivers `module_finished` for a module with no open `group_started`
- **THEN** the reporter writes a `group_started` immediately before its `group_finished`, so they pair

#### Scenario: Runner identity
- **WHEN** an ExUnit run starts
- **THEN** `suite_started.runner` is `{"name": "exunit", "version": <Elixir version>}`

### Requirement: Jest runs map onto the core model
The Jest reporter SHALL map each test file to a group named by its path relative to the Jest root directory (the global `rootDir`, also in a multi-project run), or by its basename with `file: null` when it is outside that directory, `ancestorTitles` to `path` and the test title to `name`. Durations SHALL be Jest's millisecond values multiplied by 1,000: a test's is Jest's duration for that attempt, a group's is the file's runtime in its worker, and the suite's is the wall time from run start to run completion. Statuses `pending`, `todo`, `disabled` and `focused` SHALL be `skipped`, with the original status in `meta.skip_reason`. A test file that fails to run SHALL finish its group as `errored`. Otherwise `group_finished.outcome` SHALL be `failed` when any test failed, `skipped` when every test was skipped, and `passed` otherwise. `filters` SHALL be null. `test_started.line` and `test_finished.line` SHALL be null; a result location SHALL be written only as `test_finished.meta.line`. `suite_started.meta` SHALL include `node_version`, `max_workers` and `duration_resolution_us` set to 1000. A project's display name SHALL be `meta.project` on that file's group events. `test_finished.meta` SHALL include `invocations` when Jest reports more than one. `group_finished.meta` SHALL include `load_test_environment_us`, `setup_files_us` and `slow`, each when Jest reports it for that file. When Jest reports several attempts of one test (`jest.retryTimes`), each attempt SHALL be its own `test_started`/`test_finished` pair, and group and suite `counts` SHALL count only each test's final attempt. A file that two projects both run SHALL be one group per project; because Jest gives case events no reliable project, tests SHALL be matched to their file by path, and counts SHALL still agree with Jest's own results. The reporter SHALL write `max_failures_reached` once when bail is set and Jest's failed-test count reaches it, which is when Jest bails; a file that fails to load fails no test and does not count.

#### Scenario: Todo test
- **WHEN** a Jest test is declared with `test.todo`
- **THEN** its `test_finished` has `outcome: "skipped"`, `duration_us: null` and `meta.skip_reason: "todo"`

#### Scenario: Test reported without a start event
- **WHEN** Jest reports a test result for which no case-start event was delivered
- **THEN** the reporter writes a `test_started` immediately before that `test_finished`, so they pair

#### Scenario: Skipped test with no case events
- **WHEN** Jest reports a skipped test only in its file's result, with no case-start or case-result event
- **THEN** the reporter writes that test's `test_started` and `test_finished` before the file's `group_finished`, so suite counts agree with group counts

#### Scenario: Interrupted run
- **WHEN** Jest reports the run as interrupted
- **THEN** the reporter writes `interrupted` before `suite_finished`, and `suite_finished.aborted` is true

#### Scenario: Multi-project run
- **WHEN** projects `a` and `b`, with root directories `packages/a` and `packages/b` under the Jest root directory, each have `__tests__/same.test.js`
- **THEN** their groups are `packages/a/__tests__/same.test.js` and `packages/b/__tests__/same.test.js` (relative to the root, not to each project), with `meta.project` `a` and `b`

#### Scenario: Retried test
- **WHEN** Jest reports a test result with `invocations: 2`
- **THEN** its `test_finished.meta.invocations` is 2, and results with one invocation have no `invocations` key

#### Scenario: Test that fails once, then passes on retry
- **WHEN** `jest.retryTimes(1)` reruns a test whose first attempt fails and second passes
- **THEN** both attempts are written, and the group and suite `counts` have one `passed` and no `failed` for it

#### Scenario: Same file in two projects
- **WHEN** projects `node-a` and `node-b` both run `projects/__tests__/same.test.js`
- **THEN** there are two groups named `projects/__tests__/same.test.js`, with `meta.project` `node-a` and `node-b`, each with that file's counts

#### Scenario: Line is known only on the result
- **WHEN** Jest's case start has no location and the result location is line 40
- **THEN** `test_started.line` and `test_finished.line` are null and `test_finished.meta.line` is 40

#### Scenario: File with a failed test
- **WHEN** a file loads and one of its tests fails
- **THEN** `group_finished.outcome` is `failed`

#### Scenario: Bail
- **WHEN** Jest is configured with `bail: 2` and one file has two failing tests
- **THEN** the reporter writes `max_failures_reached` once, after that file's `group_finished` and before `suite_finished`, and `suite_finished.aborted` is true

#### Scenario: Bail and a file that fails to load
- **WHEN** Jest is configured with `bail: 1` and the only failure is a file that throws while loading
- **THEN** no `max_failures_reached` is written, as Jest does not bail

### Requirement: Reporters have no third-party runtime dependencies
The ExUnit reporter SHALL depend only on Elixir's standard library. The Jest reporter SHALL be CommonJS with no runtime dependencies and no install-time build step, and SHALL ship TypeScript declarations for its options.

#### Scenario: Installing the Jest reporter
- **WHEN** a project installs the Jest reporter from its git mirror
- **THEN** no other package is installed and no build script runs

### Requirement: Reporters are distributed through read-only mirrors, versioned apart from the CLI
Each reporter SHALL be versioned and released on its own, apart from the CLI and from the other reporter. Tag `exunit-reporter-v<version>` SHALL publish `reporters/exunit` to `agoodway/goodflows-exunit-reporter`, and tag `jest-reporter-v<version>` SHALL publish `reporters/jest` to `agoodway/goodflows-jest-reporter`, each with the mirror's `main` branch and the tag `v<version>`. A CLI release tag (`v<version>`) MUST NOT push either mirror, and a reporter release MUST NOT push the other reporter's mirror. The first release in each mirror is `v0.6.0`; no earlier tag exists in either mirror. A reporter release tag's version MUST equal that reporter's package version (`@version` in `reporters/exunit/mix.exs`, `version` in `reporters/jest/package.json`), and a CLI release tag's version MUST equal the `Cargo.toml` version; the three versions need not agree, and CI MUST fail on any commit where one is missing. A reporter release tag push MUST fail before pushing anything when that mirror's deploy-key secret is missing, with an error that names the missing secret. The deploy keys SHALL be secrets of a GitHub environment that only reporter release tags may deploy to, and a dry run MUST NOT receive them. Pushes over SSH SHALL trust only GitHub's published host keys. The reporter release tag MUST name a commit on `main`. A version mismatch, a tag off `main`, or a mirror whose `main` is not an ancestor of the new split, MUST fail the release before anything is pushed. A reporter release SHALL run the reporter drift guard before pushing, and a CLI release SHALL run it before publishing. Each reporter's README SHALL state the report format it writes and the goodflows versions that accept it, instead of pinning to a goodflows version.

#### Scenario: Reporter release
- **WHEN** tag `jest-reporter-v0.6.1` is pushed and `reporters/jest/package.json` says `0.6.1`
- **THEN** `agoodway/goodflows-jest-reporter` receives `main` and tag `v0.6.1` containing only the Jest reporter's files, and the ExUnit mirror is not pushed

#### Scenario: CLI release
- **WHEN** tag `v0.7.0` is pushed, `Cargo.toml` says `0.7.0` and both reporter packages say `0.6.0`
- **THEN** the CLI is released and neither mirror is pushed

#### Scenario: Tag does not match the version
- **WHEN** tag `exunit-reporter-v0.7.0` is pushed and `reporters/exunit/mix.exs` says `0.6.0`
- **THEN** the release fails and no mirror is pushed

#### Scenario: Missing deploy key
- **WHEN** tag `jest-reporter-v0.6.1` is pushed and `JEST_REPORTER_DEPLOY_KEY` is not set
- **THEN** the release fails before pushing, the error names `JEST_REPORTER_DEPLOY_KEY`, and no mirror is pushed

#### Scenario: Mirror was edited directly
- **WHEN** a mirror's `main` contains a commit that is not in the new split
- **THEN** the release fails instead of overwriting it

#### Scenario: Tag not on main
- **WHEN** tag `jest-reporter-v0.6.1` is pushed on a commit that is not on `main`
- **THEN** the release fails, and no mirror is pushed

### Requirement: CI rejects reporter output that fails collection
CI SHALL run each reporter against a fixture suite that covers passing, failing, skipped and errored tests and nested paths, and, for Jest, separate fixture runs with retries across two projects that run the same files and with bail. CI SHALL collect the output with the CLI built from the same commit, and SHALL fail when any report is rejected, when a fixture does not produce exactly one report, when a report does not end with `suite_finished` with the fixture's expected `aborted`, zero unfinished tests and groups, and the fixture's expected suite counts and group outcomes and counts, or when a reporter prints a "stopped writing" warning. The fixture command's own exit status SHALL be ignored, because the suites fail on purpose. Every fixture SHALL be executed with Jest at its minimum peer version and at the latest version.

#### Scenario: Reporter emits a field the contract lacks
- **WHEN** a change makes the Jest reporter emit a `retries` key at the top level of `test_finished`
- **THEN** the reporters CI job fails with the rejection code

#### Scenario: Failing fixtures still pass CI
- **WHEN** the fixture suites exit nonzero and every collected report is accepted
- **THEN** the reporters job succeeds

#### Scenario: Truncated report
- **WHEN** a reporter stops writing before `suite_finished` and the truncated report is accepted by collection
- **THEN** the reporters CI job fails, naming the job and the last record's event

#### Scenario: Jest version matrix
- **WHEN** the reporters job runs
- **THEN** every Jest fixture runs at the minimum peer version and at the latest Jest, and the reporters' own unit tests, which replay recorded payloads and never load Jest, run once
