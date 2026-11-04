# Tasks

## 1. Settings and flags

- [ ] 1.1 Add `history`, `history_dir`, `log_runs`, `history_runs`, and `history_bytes` to `Settings`, `SettingsOverrides`, and `EffectiveSettings`, with `history` defaulting off and an explicit profile `false` overriding a lower `true`. Verify `cargo test --lib config::` covers shared, workflow, and profile precedence, and that `0` for any cap fails validation.
- [ ] 1.2 Add `--history`, `--no-history`, `--history-dir`, `--log-runs`, `--history-runs`, and `--history-bytes`. `--history` and `--no-history` conflict. `--no-history` forces off; otherwise `--history` or `--history-dir` forces on. Numeric flags override resolved settings. Verify a CLI parse test and a settings-resolution test for each override.
- [ ] 1.3 Include the resolved history settings in `list` and `validate` JSON and in human `list` output, using the defaults from `run-history` when unset. Verify `cargo test --test inspection` shows history off, `goodflows-history`, `log_runs` 10, `history_runs` 1000, and `history_bytes` 10485760.
- [ ] 1.4 Document the five settings, the flags, and the precedence in `README.md`. Verify the documented defaults match the spec.

## 2. Per-run log directories

- [ ] 2.1 Create one UTC-second timestamp and lowercase UUID at run start. With `--log-dir`, write logs under `<effective log dir>/<stem>/`, keeping named-workflow subdirectories and the flat legacy layout. Keep today's basenames and raw bytes. Verify a scheduling test that a named workflow and a legacy config land in the paths from `run-logs`, and that a shell condition and a command share one stdout file.
- [ ] 2.2 After creating the run directory, prune older matching directories to `log_runs`, never deleting the new directory, unrelated names, or symlinks. An unfinished directory counts. Creation failure still fails the run; prune failure is reported and does not change the exit code. Verify tests for the default of 10, a profile cap of 2, an unrelated file, a symlink left in place, and two partitioned runs keeping six files each.
- [ ] 2.3 Point `summary.log_dir` and the text report's log line at the per-run directory, lossy when the path is not UTF-8. Verify `cargo test --test output` for one run, two runs, and no `--log-dir`.
- [ ] 2.4 Update the README log-layout section for the per-run directory and the break from truncated flat files. Verify the section shows a named-workflow path and states that old flat files are left in place.

## 3. Optimizer measurements

- [ ] 3.1 Record `blocked_ms`, `queued_ms`, and `max_in_flight` in the scheduler using the definitions in `run-history`. Verify a parallelism-1 test where the second job's queue wait is nonzero, and a dependency-skip test where `queued_ms` is 0 and `started` is false.
- [ ] 3.2 On Unix, record `cpu_user_ms` and `cpu_system_ms` from `wait4` for each spawned condition, command, and step. On Linux, sample the process group's summed `VmRSS` on output reads and at least every 200ms, and store the peak as `peak_rss_bytes`. Omit fields the platform cannot measure. Verify a Linux test that a short command has `wall_ms` and CPU fields, and that `peak_rss_bytes` is present.
- [ ] 3.3 Add `sha2` and compute `graph_sha256` from the canonical selected-job document in `design.md`. Verify the same selection hashes the same across two runs, an env-value change does not change it, and a command change does.
- [ ] 3.4 Add `outcome`, `parallelism`, `graph_sha256`, `max_in_flight`, the host object, and per-result `blocked_ms`, `queued_ms`, and `commands` to the `summary` event only. Leave live `job_finished` events unchanged, and leave existing `command` and `error` strings untruncated. Verify `cargo test --test output` for a `--json` run with history off, and that `job_finished` lacks the new fields.

## 4. History files

- [ ] 4.1 When history is enabled, write one compact `schema_version` 1 JSON file at `<history dir>/<encoded workflow>/<stem>.json`, using `default` for legacy jobs. Resolve config paths from the config file and `--history-dir` from the invocation directory. Refuse a symlink directory. Verify tests for the default path, a profile `history_dir`, a legacy workflow component, and `--history-dir`.
- [ ] 4.2 Fill the file from the same result data as the summary, truncate command and error strings to 1024 bytes on a character boundary, and omit stdout, stderr, env values, absolute paths, and log filenames. Write atomically, then prune oldest matching files to `history_runs` and `history_bytes` without deleting the new file, unrelated names, other workflows, or symlinks. A write or prune failure is reported, leaves the exit code unchanged, and omits `history_file`. Verify a byte cap that keeps only the newest file, a count cap of 2, a second run that does not change the first file's bytes, and a secret printed by a job that does not appear in the file.
- [ ] 4.3 Show the history path in the text report only when the file was written, and set `summary.history_file` to that path. `validate` and `list` must not create a file. Verify an output test for the report line and a validate test that the history directory is absent.
- [ ] 4.4 Document enabling history, the file layout, the three caps, and that the byte cap is a default rather than a maximum. Document committing `goodflows-history/`, gitignoring the log directory, `text eol=lf linguist-generated`, and that prune does not shrink `.git`. Verify the README examples use the flag and setting names from the spec.

## 5. Integration

- [ ] 5.1 Run `cargo test` and verify the suites that cover config, inspection, scheduling, output, and reporting pass with the new log paths and summary fields.
