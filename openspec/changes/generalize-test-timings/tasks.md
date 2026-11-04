# Tasks

## 1. Contract schema

- [ ] 1.1 Rewrite `src/reporting/contract.rs` as the typed per-event table from design D1/D3. Set `REPORT_FORMAT = "test-timings/1"` and `CONTRACT_VERSION = "generalize-test-timings"`, invert the module doc so the server mirrors this module, and remove the ExUnit tables (`TIMES`, `BUCKET`, `FILE_EXTRA`, `VERSION_FIELDS`). Verify with module unit tests that assert every event's required key set, the field types from D1 and the outcome list.
- [ ] 1.2 Add the `meta` rule (D4) to `contract.rs`: at most 32 identifier keys, with scalar values (null, bool, integers within ±(2^53-1), strings of at most 256 bytes). Verify with unit tests for each "Runner metadata is flat and bounded" scenario.

## 2. Collection and validation

- [ ] 2.1 Rework `collect.rs::validate_records` into the single-pass validator from D2: types, required keys, `meta`, order, pairing via open-group and open-test maps, and the `unfinished_*` equality check. Emit the new codes `invalid_meta`, `unpaired_event` and `unfinished_mismatch`. Verify with unit tests covering each scenario in `specs/test-timing-contract/spec.md` and the "Contract violations map to stable structural codes" scenarios.
- [ ] 2.2 Read `suite_id` instead of `run_id` in `parse_report` and in `validate_envelope`, where `report.suite_id` must match the first record's `suite_id`. Emit `format: "test-timings/1"` in `envelope`, and keep `assign_partition` unchanged: it rewrites only `partition` and `partition_count` on the first record. Verify that the existing partition-rewrite and identity-mismatch tests pass against the new record shape.
- [ ] 2.3 Make `validate_envelope` apply the new record validation and refuse `exunit-timings`. Verify with a test that replays a run directory holding an `exunit-timings` snapshot: replay reports `invalid_snapshot`, sends nothing, exits nonzero and leaves the file intact.
- [ ] 2.4 Update the remaining `collect.rs` unit tests and helpers (`suite_started`, `test_finished`, `complete`, filters, limits, nesting, numbers) to the `test-timings/1` shape. Verify that `cargo test --locked reporting` passes. `exunit-timings` may remain only where a test or message rejects the retired format.

## 3. Configuration and CLI tests

- [ ] 3.1 Replace the `config.rs::REPORT_FORMAT` alias of `contract::REPORT_FORMAT` with its own constant `"test-timings"` (D6). Leave `contract::REPORT_FORMAT` as `"test-timings/1"`. Verify with config tests that `reports = ["test-timings"]` is accepted, that `["exunit-timings"]` and duplicates are rejected, and that the unsupported-format error names `test-timings`.
- [ ] 3.2 Rewrite the synthetic report in `tests/common/mod.rs` to emit `test-timings/1` (groups, paths, `meta`), and update `tests/reporting.rs` and `tests/inspection.rs` to match. Verify that `cargo test --locked` passes. `exunit-timings` may remain only in tests and messages that reject the retired format.
- [ ] 3.3 Rewrite the README "Test timing reports" section: `test-timings` config, a summary of the core model and `meta` rules linking to the contract spec, installation of both reporters from their mirrors, and an upgrade note (old snapshots become `invalid_snapshot`; the server must support `test-timings/1` before uploading). Verify that the README's TOML examples pass `goodflows validate`.

## 4. ExUnit reporter

- [ ] 4.1 Scaffold `reporters/exunit/` as the Mix app `:goodflows_exunit_reporter`, with Elixir `~> 1.18`, no dependencies, version equal to `Cargo.toml`, and a README stating the mirror is read-only. Verify that `mix compile --warnings-as-errors` succeeds in that directory.
- [ ] 4.2 Port `GC.TestTimingFormatter` from goodcases `app/test/support/test_timing_formatter.ex` (it is not in this repo) to `Goodflows.ExUnitReporter` using the built-in `JSON`. Apply the D7 mapping (group, path, name, outcomes, `meta` keys, `filters`, runner) and partition precedence. The suite option is `:test_timing_suite_id`, renamed from `:test_timing_run_id`, and the env is `TEST_TIMING_SUITE_ID`, otherwise the `report_id`. Keep `:test_timing_directory`. Do not copy the `@identifier` module-attribute regex; test the grammar at the call site. An unwritable directory logs one warning and turns the formatter into a no-op. Verify with ported ExUnit tests covering the "ExUnit runs map onto the core model", identity, crash-safety and "never change test results" scenarios.

## 5. Jest reporter

- [ ] 5.1 Determine the earliest Jest version that provides `onTestCaseStart` and `onTestCaseResult` (from Jest's changelog or by installing candidates), and record it as `peerDependencies.jest`. Verify by running the reporter's tests against that version.
- [ ] 5.2 Scaffold `reporters/jest/`: `package.json` named `goodflows-jest-reporter` (no `dependencies`, no `prepare`/`install` scripts, `engines.node >= 18`, version equal to `Cargo.toml`), `index.js` (CommonJS), `index.d.ts`, and a read-only-mirror README. Verify that `npm pack --dry-run` lists only those files.
- [ ] 5.3 Implement the reporter hooks per D7: sync `wx` file writes, groups per file, `ancestorTitles` mapped to `path`, millisecond-to-microsecond conversion, status mapping, the synthesized `test_started`, file errors as `errored`, `interrupted`/`max_failures_reached`, shard and goodflows partitions, a new report per watch run, and hooks that never throw. Verify with `node --test` unit tests that drive the hooks with recorded Jest payloads for each "Jest runs map onto the core model" scenario.

## 6. Drift guard

- [ ] 6.1 Add `reporters/fixtures/exunit/` (a Mix project with a path dependency on `../../exunit`) and `reporters/fixtures/jest/` (Jest pinned in a lockfile). Both cover passing, failing, skipped, errored (`setup_all` raise / file throws on load), nested describes, a todo test and a tag filter. Verify that each fixture produces a JSONL report when run by hand with `GOODFLOWS_REPORT_DIR` set.
- [ ] 6.2 Add `reporters/fixtures/goodflows.toml` and `scripts/check-reporters.sh`. The script runs the built CLI with `--json` and no API key, and fails unless each job's `reports_collected` event has at least one collected report and an empty `rejected`. Verify by running it locally (it passes), then temporarily adding a top-level `retries` key in the Jest reporter and confirming the script fails and names the code.
- [ ] 6.3 Add a `reporters` job to `.github/workflows/ci.yml`, separate from the `check` matrix, on `ubuntu-24.04`. Install Rust, Node 24, and Elixir with `erlef/setup-beam` pinned to `elixir-version: "1.18.6"` and `otp-version: "27"`. The job runs both reporters' unit tests and `scripts/check-reporters.sh`, with Jest at the minimum peer version and the latest. Verify that the job passes on a pull request.

## 7. Release mirrors

- [ ] 7.1 Add a `verify-versions` job to `release.yml`, which `build` depends on. It fails unless `Cargo.toml`, `reporters/exunit/mix.exs` and `reporters/jest/package.json` versions agree, and, when the workflow ref is a tag, unless that tag's version agrees too. Verify with `workflow_dispatch` on a branch where one version deliberately differs (it fails) and where they agree (it passes).
- [ ] 7.2 Add a `mirror-reporters` job (needs `build`; `publish` then needs it) that performs the subtree split, the ancestry check, the non-force push of `main` and the tag push for each mirror, per D9, using `EXUNIT_REPORTER_DEPLOY_KEY` and `JEST_REPORTER_DEPLOY_KEY`. Verify with a dry-run mode on `workflow_dispatch` that prints the split SHAs and the ancestry result without pushing.
- [ ] 7.3 Manual prerequisite: create write-enabled deploy keys on `agoodway/goodflows-exunit-reporter` and `agoodway/goodflows-jest-reporter`, and store the private halves as the two secrets in `agoodway/goodflows`. Verify that the dry-run from 7.2 can fetch each mirror over SSH.
- [ ] 7.4 Document in the README how to install each reporter from its mirror (the Mix `github:` + `tag:` dependency and the npm `github:...#vX.Y.Z` dependency) and that both reporters are versioned in lockstep with the CLI. Verify that each snippet resolves against a mirror tag once the first release is published.

## 8. Integration check

- [ ] 8.1 Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked` and `scripts/check-reporters.sh`. Then run `openspec validate generalize-test-timings --strict`. Verify that all pass.
