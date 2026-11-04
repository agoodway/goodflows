# goodflows Jest reporter

> **Read-only mirror.** This repository is published automatically from
> [`agoodway/goodflows`](https://github.com/agoodway/goodflows) (`reporters/jest`)
> on every goodflows release. Do not open pull requests or push here: send
> changes to `agoodway/goodflows`. A release fails rather than overwrite commits
> made directly in this mirror.

A Jest reporter that writes `test-timings/1` JSON Lines reports for
[goodflows](https://github.com/agoodway/goodflows) to collect and publish. It is
CommonJS with no runtime dependencies and no install-time build step, and ships
TypeScript declarations for its options. It needs Node.js 18 or newer and Jest
29.6.0 or newer (the first release with the `onTestCaseStart` hook).

## Install

The reporter is versioned in lockstep with the goodflows CLI and is available
starting with goodflows v0.6.0, the first release that contains it (there is no
earlier tag). Pin the tag equal to your goodflows version, v0.6.0 or later:

```json
{
  "devDependencies": {
    "goodflows-jest-reporter": "github:agoodway/goodflows-jest-reporter#v0.6.0"
  }
}
```

```js
// jest.config.js
module.exports = {
  reporters: ['default', 'goodflows-jest-reporter'],
};
```

```toml
# goodflows.toml
[[jobs]]
name = "test"
command = "npx jest"
reports = ["test-timings"]
```

## Options

| Option | Default | Meaning |
|---|---|---|
| `outputDirectory` | `GOODFLOWS_REPORT_DIR`, then `<rootDir>/.goodflows/test-timings` | Directory for reports, resolved against `rootDir` |
| `suiteId` | `TEST_TIMING_SUITE_ID`, then `GOODFLOWS_RUN_ID`, then the report id | Suite identity shared by related reports, such as every shard of one run (1-128 bytes of `[A-Za-z0-9._-]`, starting with a letter or digit) |

An explicit `outputDirectory` wins over `GOODFLOWS_REPORT_DIR`. Under goodflows
that means the report lands outside the directory goodflows collects, so when
both are set and differ the reporter prints one warning (and still writes the
report where the option says). Leave `outputDirectory` unset in goodflows jobs.
An invalid `suiteId`, `TEST_TIMING_SUITE_ID` or `GOODFLOWS_RUN_ID` (goodflows
sets it to the workflow run's UUID) is skipped in favor of the next source.

## Behavior

- **One report per run.** Every run, including each watch-mode rerun, is a new
  `<report_id>.jsonl` file; an existing file is never overwritten. The report
  id is the time, the process id, a sequence number and random bytes, so
  containers that share a PID still get distinct ids.
- **Mapping.** Each test file is a group named by its path relative to the
  global `rootDir` (also in a multi-project run, so same-named files in two
  projects stay distinct groups), `ancestorTitles` become `path` and the test
  title is `name`. A file that two projects both run is two groups, told apart
  by `meta.project`. `pending`, `todo`, `disabled` and `focused` tests are
  `skipped`, with the Jest status in `meta.skip_reason`. A file that fails to
  run finishes its group as `errored`; otherwise a group is `failed` when any
  test failed, `skipped` when every test was skipped and `passed` otherwise.
  `filters` is always `null`. `line` is `null`; a result location (with
  `testLocationInResults`) is written as `test_finished.meta.line`.
- **Durations.** Integer microseconds, from Jest's milliseconds times 1,000
  (`suite_started.meta.duration_resolution_us` is 1000). A test's
  `duration_us` is Jest's `duration` for that attempt (`null` for tests Jest
  did not run); a group's is the file's `perfStats.runtime` in its worker (or
  `end - start`); the suite's is the wall time from `onRunStart` to
  `onRunComplete` in the parent process.
- **Retries.** With `jest.retryTimes`, Jest reports every attempt, so each
  attempt is its own `test_started`/`test_finished` pair, and attempts after the
  first carry `meta.invocations` (the attempt number). Group and suite `counts`
  count only each test's final attempt, so they match Jest's own results.
- **Partitions.** `GOODFLOWS_PARTITION`/`GOODFLOWS_PARTITION_COUNT` when both
  are valid, then Jest's `--shard` (`--shard=2/3` is partition `"2"` of `3`),
  otherwise `null` and `1`.
- **Runner details in `meta`.** `reporter` (`goodflows-jest-reporter`),
  `reporter_version`, `node_version`, `max_workers`, `duration_resolution_us`
  and `filtered` (true when Jest ran only some tests: a name or path pattern,
  `--onlyChanged`, `--onlyFailures`, `--findRelatedTests`, `--lastCommit`,
  `--changedSince` or a `filter` module; sharding is a partition, not a filter)
  on `suite_started`; the project's display name as `project` on group events;
  `load_test_environment_us`, `setup_files_us` and `slow` on `group_finished`,
  each only when Jest reports it (a file that fails to load may have none);
  `invocations` (when above 1) and `skip_reason` on `test_finished`.
- **Interruptions.** An interrupted run writes `interrupted` before
  `suite_finished`. With `bail`, `max_failures_reached` is written once when the
  failed-test count reaches it, as Jest counts it (a file that fails to load
  fails no test, so it does not count).
- **Structural only.** Failure messages, console output and stack traces are
  never written. U+0000 and lone UTF-16 surrogates in text become U+FFFD,
  empty text becomes `-`, `path` keeps the first 32 describe titles, text over
  4,096 bytes is cut on a UTF-8 boundary, and files outside `rootDir` are
  `null` (their group is named by the file's basename).
- **Crash-safe and harmless.** Records are written synchronously, one complete
  line per event. A hook never throws: a failure (for example an unwritable
  directory) prints one warning and stops writing for that run, and never
  changes test results or the exit status.
