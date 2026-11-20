# Tasks

## 1. References and masking

- [ ] 1.1 Add `src/secrets.rs` with `Reference` (one `OnePassword` variant) and `Reference::parse` per design.md. Unit-test: three and four segments, a `?attribute=otp` query, an empty segment, two and five segments, a space, a tab, 1024 and 1025 bytes, `0p://`, `OP://` (rejected; schemes are lowercase), and a bare value. Verify with `cargo test --lib secrets`.
- [ ] 1.2 Add `Masker` with per-stream `feed`/`finish` that holds back only a suffix that is a proper prefix of a value, and replaces values longest first. Unit-test: a value split at every byte boundary across two chunks, a value that contains another value, overlapping candidates (`abab` in `ababab`), a held-back suffix that turns out not to match and is released, a multi-byte UTF-8 value, no values (chunk returned unchanged), and a value under 4 bytes ignored. Verify with `cargo test --lib secrets`.

## 2. Config

- [ ] 2.1 Add `secrets` to `Settings`, `SettingsOverrides` (merged in `apply`, counted in `is_empty`) and `EffectiveSettings`, and `secrets: Vec<String>` to `Job`. Reject `secrets` on call jobs.
- [ ] 2.2 Validate per design.md: reference syntax, name rule, reserved and credential names, `env`/`secrets` overlap in one table, job or step `env` setting a listed name, undefined and duplicate names in a job, and a profile defining a name no lower layer defines. Test each rejection and the accepted shapes in `src/config.rs`. Verify with `cargo test --lib config`.

## 3. Resolution

- [ ] 3.1 Add `secrets::resolve(pairs, workdir, cancel)`: find `op`, run the first read alone (120 s) and the rest at most 8 at a time (30 s each), check UTF-8 and NUL, warn for values under 4 bytes, and return `Secrets { values, masker }` or an error carrying the name, the reference and `op`'s first stderr line, masked and capped at 512 bytes.
- [ ] 3.2 Add `Run.secrets: Arc<Secrets>`. In `main.rs`, after selection and `targets`, collect the pairs from selected root jobs and every job in `targets`, and call `resolve` only when there are any. Add the `secret_unavailable` code (exit 1) with the `secret` field.
- [ ] 3.3 Add `tests/fixtures/fake-op/op`, a script that reads values from a fixture file keyed by reference, fails for unknown ones, and appends each call to a log. Add `tests/secrets.rs`: one read per distinct reference; `--jobs fmt` with no `op` on `PATH` passes; a failed read starts no job and exits 1 with `secret_unavailable`; `op` missing; a non-UTF-8 value; a profile switching the reference; a called workflow's secret resolved before the root's first job; `validate` and `list` never call `op`. Verify with `cargo test --test secrets`.

## 4. Delivery and masking

- [ ] 4.1 In `Execution::spawn`, set the job's listed secrets after the reporting removals, for job and step commands but not conditions. Keep them out of `job_env`. Test the "only the listing job" and "expressions never see the value" scenarios, and that a listed secret replaces a caller variable of the same name.
- [ ] 4.2 Thread a per-stream masker state into `drain` ahead of `Tails`, the log file and the line splitter, and mask `read_outputs` strings. Test the failure-output, split-chunk, other-job and published-output scenarios against the terminal, `--json`, the log file, `goodflows last` and the summary. Verify with `cargo test --test secrets --test runner --test steps`.

## 5. Inspection

- [ ] 5.1 Add the `secrets` column to `list` text output and `secrets` to its JSON and the MCP `list` result. Verify with `cargo test --test inspection`.

## 6. Sprite

- [ ] 6.1 On the controller, resolve after `check_remote_env`, add values to `Sensitive` and `sprite.secrets`, and send them under `GOODFLOWS_SPRITE_SECRETS` in the private frame. Reject that name in `pass_env`.
- [ ] 6.2 On the Sprite, filter the key in `Worker::user_env`, write `{"cache", "secrets"}` to the workflow process's stdin, and set `Run.secrets` in `exec_workflow` without running `op`. Extend `tests/sprite.rs`: the remote job gets the value, the fake Sprite environment has no `op` calls, the setup hook sees no secret, and a snapshot file that contains a value stops the run. Verify with `cargo test --test sprite`.

## 7. Documentation

- [ ] 7.1 README: a "Secrets" section (shape, opt-in, profiles, `op` authentication with the desktop app and `OP_SERVICE_ACCOUNT_TOKEN`, what is masked and what is not), `secret_unavailable` in the error code table, and `secrets` in the call-job rejection list. Update docs/sprites.md and `src/agents_guide.md`.

## 8. Integration check

- [ ] 8.1 Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked`. With a real 1Password account, run a job that lists an `op://` secret and prints it, and confirm one approval prompt and `[redacted]` in the terminal, the log and `goodflows last`.
