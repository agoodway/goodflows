# goodflows nextest reporter

> **Read-only mirror.** This repository is published automatically from
> [`agoodway/goodflows`](https://github.com/agoodway/goodflows) (`reporters/nextest`)
> on every release of this reporter. Do not open pull requests or push here: send
> changes to `agoodway/goodflows`. A release fails rather than overwrite commits
> made directly in this mirror.

A wrapper for [cargo-nextest](https://nexte.st) runs that writes `test-timings/1`
JSON Lines reports for [goodflows](https://github.com/agoodway/goodflows) to
collect and publish. Rust's test harness has no stable reporter hook, and
nextest's JSON event stream is still experimental, so the reporter reads the
JUnit report nextest writes at the end of a run. It needs Rust 1.88 or newer to
build, and any cargo-nextest with JUnit support.

## Install

The reporter is versioned on its own, apart from the goodflows CLI. Every release
writes `test-timings/1`, which goodflows v0.6.0 and later accept. Pin a tag
(v0.1.0 is the first):

```bash
cargo install --locked --git https://github.com/agoodway/goodflows-nextest-reporter \
  --tag v0.1.0 goodflows-nextest-reporter
```

Enable JUnit for the nextest profile you run, and skipped (`#[ignore]`) tests if
you want them in the report:

```toml
# .config/nextest.toml
[profile.default.junit]
path = "junit.xml"
report-skipped = "ignored"
```

Then put the reporter in front of the nextest command:

```toml
# goodflows.toml
[[jobs]]
name = "test"
command = "goodflows-nextest-reporter -- cargo nextest run"
reports = ["test-timings"]
```

## Options

| Option or variable | Meaning |
|---|---|
| `--junit <path>` | The JUnit file to read. By default it is `<workspace>/<store dir>/<profile>/<junit path>`, from `.config/nextest.toml` (or `--config-file`), the profile from `--profile`/`-P` or `NEXTEST_PROFILE` (following `inherits`), and `[store] dir` (default `target/nextest`) |
| `GOODFLOWS_REPORT_DIR` | Directory for reports. Without it, `.goodflows/test-timings` |
| `TEST_TIMING_SUITE_ID` | Suite identity shared by related reports, then `GOODFLOWS_RUN_ID`, then the report id |

## Behavior

- **Wraps the run.** The reporter runs the command, waits for it, converts the
  JUnit file and exits with the command's exit code (128 plus the signal number
  when a signal ended it). A reporter failure prints one warning and never
  changes the exit code. If JUnit is not enabled for the profile, the warning
  says what to add, and no report is written.
- **One report per run.** Each run writes a new `<report_id>.jsonl`. A JUnit
  file older than the run (for example after a build failure, when nextest ran
  nothing) is not read: the run is reported as `aborted` with no tests.
- **Mapping.** Each test binary is a group, named by its nextest binary id
  (`my-crate`, `my-crate::api`, `my-crate::bin/tool`), and `file` is the
  binary's source file relative to the workspace root, from `cargo metadata`. A
  test's name is split on `::`: the module path becomes `path` and the last
  segment `name`. `file` and `line` of a test are `null`, because JUnit has no
  source locations. `filters` and `seed` are `null`.
- **Outcomes.** A passing test is `passed`; a test with `<failure>`, including
  timeouts and aborts, is `failed`; `<error>` is `errored`; `<skipped>` is
  `skipped`. The JUnit `type` (such as `test timeout` or `test abort`) is
  `meta.failure_type`, and a skipped test's message is `meta.skip_reason`. A
  group is `failed` when any test failed, `errored` when any errored and none
  failed, `skipped` when every test was skipped, and `passed` otherwise.
- **Retries.** Every attempt is its own `test_started`/`test_finished` pair, and
  attempts after the first carry `meta.invocations`. A flaky test's last attempt
  carries `meta.flaky`. Group and suite `counts` count each test's final
  attempt, so they match nextest's summary. With `flaky-fail-status =
  "failure"`, a flaky test's last attempt is `failed`, as nextest counts it.
- **Timeline.** Records are written in the order events happened, with
  `received_offset_us` set to each event's own time, so binaries that ran in
  parallel interleave. Offsets are from the run's JUnit `timestamp`
  (`started_at`). A test's `duration_us` is its JUnit `time`, at millisecond
  resolution (`suite_started.meta.duration_resolution_us` is 1000). A group's is
  the span from its first attempt's start to its last attempt's end, and the
  suite's is the run's JUnit `time`.
- **Partitions.** `GOODFLOWS_PARTITION`/`GOODFLOWS_PARTITION_COUNT` when both
  are valid, then nextest's `--partition <kind>:<m>/<n>` (partition `"m"` of
  `n`), otherwise `null` and `1`.
- **Runner details in `meta`.** `reporter`, `reporter_version`,
  `duration_resolution_us` and `run_uuid` (nextest's run id) on
  `suite_started`; `exit_code` (`null` after a signal) on `suite_finished`.
- **Interruptions.** When Ctrl-C or a signal stops the run, nextest still
  writes its JUnit file, and the report ends with `interrupted` and
  `suite_finished.aborted: true`. `max_failures_reached` is never written,
  because JUnit does not say whether `--max-fail` stopped the run.
- **Structural only.** Failure messages and captured output are never written.
  Text over 4,096 bytes is cut on a UTF-8 boundary, empty text becomes `-`, and
  `path` keeps the first 32 module segments.
