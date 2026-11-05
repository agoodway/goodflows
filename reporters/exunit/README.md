# goodflows ExUnit reporter

> **Read-only mirror.** This repository is published automatically from
> [`agoodway/goodflows`](https://github.com/agoodway/goodflows) (`reporters/exunit`)
> on every release of this reporter. Do not open pull requests or push here: send
> changes to `agoodway/goodflows`. A release fails rather than overwrite commits
> made directly in this mirror.

An ExUnit formatter that writes `test-timings/1` JSON Lines reports for
[goodflows](https://github.com/agoodway/goodflows) to collect and publish. It
depends only on Elixir's standard library (Elixir `~> 1.18`, using the
built-in `JSON` module).

## Install

The reporter is versioned on its own, apart from the goodflows CLI, and only
gets a new tag when the reporter itself changes. Every release writes
`test-timings/1`, which goodflows v0.6.0 and later accept, so any reporter tag
works with any of those goodflows versions. Pin the latest tag (v0.6.0 is the
first; there is no earlier tag):

```elixir
# mix.exs
{:goodflows_exunit_reporter,
 github: "agoodway/goodflows-exunit-reporter", tag: "v0.6.0", only: :test}
```

```elixir
# test/test_helper.exs
ExUnit.start()
Goodflows.ExUnitReporter.configure()
```

`configure/0` appends the reporter to the formatters already configured, so the
normal CLI output is unchanged. With `mix test --formatter ...`, list
`Goodflows.ExUnitReporter` as one of the formatters.

```toml
# goodflows.toml
[[jobs]]
name = "test"
command = "mix test"
reports = ["test-timings"]
```

## Behavior

- **Where it writes.** Each run is a new `<report_id>.jsonl` file (never
  overwritten) in the `:test_timing_directory` ExUnit option, otherwise a
  nonempty `GOODFLOWS_REPORT_DIR`, otherwise `_build/test/timings/`. When the
  option and a nonempty `GOODFLOWS_REPORT_DIR` resolve to different
  directories, the option still wins, and the reporter prints one warning that
  goodflows will not collect the report; it keeps writing.
- **Suite identity.** `suite_id` is the first of the `:test_timing_suite_id`
  option, `TEST_TIMING_SUITE_ID` and `GOODFLOWS_RUN_ID` (which goodflows sets
  for every job that produces reports) that follows the identifier grammar
  (1-128 bytes of `[A-Za-z0-9._-]`, starting with a letter or digit), otherwise
  the report id. Use it to group reports from several partitions or machines.
- **Partitions.** `GOODFLOWS_PARTITION` with `GOODFLOWS_PARTITION_COUNT` when
  both are valid, otherwise `MIX_TEST_PARTITION` with `TEST_TIMING_PARTITIONS`
  when both are valid, otherwise `null` and `1`. A partition is 1-64 bytes of
  `[A-Za-z0-9._-]` and a count is a positive decimal integer; a partition
  without a valid count (or a count without a partition) is never used alone.
- **Mapping.** A test module (one per parameter set) is a group named by its
  inspected module name, a `describe` is a one-element `path` (or `[]`), and
  `name` is the test's own title, without ExUnit's `test ` and describe prefix
  (other names, such as doctests, are kept whole). Excluded and skipped tests
  are `skipped` with `meta.skip_reason` (`excluded` or `skipped`); tests made
  invalid by a failing `setup_all` and that module's group are `errored`. A
  group's `counts` include its excluded and skipped tests, and its `outcome` is
  `errored` (failing `setup_all` or an errored test), then `failed`, then
  `skipped` (every test skipped), otherwise `passed`. Boolean include/exclude
  tags become `filters`.
- **Meta.** ExUnit details go in `meta`:
  - `suite_started`: `elixir_version`, `otp_release`, `max_cases`,
    `schedulers`, `reporter` (`"goodflows_exunit_reporter"`),
    `reporter_version` (this package's version), and `filtered`: `true` when
    the run selected tests by tag or name (a nonempty include or exclude
    filter, or `:only_test_ids`, as with `mix test --failed`), else `false`.
  - group events: `async`, `setup_all` and `parameter_id`.
  - test events: `async` (the module's setting for tests the runner never
    tagged, such as excluded and skipped ones), `parameter_id`, and
    `skip_reason` on `test_finished`.
  - `suite_finished`: `run_us`, `async_us`, `sync_us`, `load_us` and
    `total_us`.

  `parameter_id` is `null` without parameters, otherwise a hash of the
  parameters (an integer below 2^32), so the same parameters get the same id in
  every run and partition without their values being written. ExUnit passes
  parameters only to the tests it runs, so excluded, skipped and invalid tests
  in a parameterized module have a `null` `parameter_id`.
- **Durations.** All durations and offsets are integer microseconds. A test's
  `duration_us` is ExUnit's own test time for `passed` and `failed` tests and
  `null` for skipped, excluded and invalid ones. A group's `duration_us` is the
  time between the reporter receiving the module's start and its finish. The
  suite's `duration_us` is ExUnit's run time (`meta.run_us`).
- **Structural only.** Captured output, logs, failure messages, parameter values
  and arbitrary tags are never written. U+0000 in text becomes U+FFFD, empty
  text becomes `-`, text over 4,096 bytes is cut on a UTF-8 boundary, and files
  outside the working directory are `null`.
- **Crash-safe and harmless.** Every record is written as one complete line as
  the event arrives. A reporter failure (for example an unwritable directory,
  or a raise, throw or exit while handling an event) prints one warning and
  stops writing; it never changes test results.
