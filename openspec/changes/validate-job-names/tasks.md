# Tasks

## 1. One shared name rule

- [ ] 1.1 In `src/config.rs`, add `pub fn valid_name` (trimmed nonempty, `len() <= contract::MAX_NAME_BYTES`, no `char::is_control`) and `NAME_RULE`, and rebuild `validate_name` on them. Add a unit test covering: valid at 64 ASCII bytes and 32 × `é`; invalid at 65 bytes, for `""`, `"   "`, and for `\t`, `\n`, `\u001b`, `\u009b`; valid for `a b,c` and `-x`. Also assert that `NAME_RULE` contains `MAX_NAME_BYTES`. Verify with `cargo test --lib config`.
- [ ] 1.2 Delete `valid_report_name` and switch `reporting/replay.rs` and `reporting/collect.rs::validate_envelope` to `config::valid_name`. Add tests: a manifest with a whitespace-only job name is refused before any request, and a snapshot whose `environment` is 65 bytes is counted as `invalid_envelope` and not sent. Verify `grep -rn valid_report_name src` is empty and `cargo test --lib reporting` passes.

## 2. Job names

- [ ] 2.1 In `validate_jobs`, make `validate_name(&job.name, "job").with_context(|| format!("job '{}'", job.name.escape_debug()))?` the first check in the loop. Remove the `empty job name` and trailing control-character checks, and keep the duplicate check. In `validate_reports`, remove the job-name check and its `reporting job name ...` message. Verify with `cargo build`.
- [ ] 2.2 Replace the unit test `job_names_reject_control_characters_only` with tests for jobs without `reports`: a 65-byte name rejected in root `[[jobs]]` and in an unselected named workflow; a whitespace-only name rejected; a tab (and the existing `\n`, `\r`, `\u0000`, `\u001b`, `\u009b` cases) rejected, with the name escaped in the error; a 64-byte name and 32 × `é` accepted; `a b,c` accepted. Each error must contain `job name must be nonempty, at most 64 UTF-8 bytes, and contain no control characters`. Verify with `cargo test --lib config`.
- [ ] 2.3 Update the integration tests. In `tests/runner.rs` `validation_rejects_invalid_configs_before_commands_execute`, change `empty job name` to `job name must be nonempty` and add a 65-byte job name case, which already exercises run, `validate` and `list` and asserts no command ran. In `tests/reporting.rs` `reporting_config_boundaries_are_accepted`, make the unreported job name exactly 64 bytes. Confirm `job_level_reporting_errors_name_the_job` still passes. Verify with `cargo test --test runner --test reporting`.

## 3. Runtime output keys

- [ ] 3.1 In `src/runner.rs` `read_outputs`, reject keys with `!config::valid_name(key)` and the error `output keys {NAME_RULE}`. In `tests/steps.rs`, extend `invalid_outputs_fail_the_producer_instead_of_silently_skipping_consumers` with a 65-byte key, and add a passing case with a 64-byte key that a dependent references through `needs.<job>.outputs['<key>']`. Verify with `cargo test --test steps`.

## 4. Documentation

- [ ] 4.1 In README.md "Configuration", add one bullet stating the name rule for workflow, environment, job, step and output names, including keys written to `$GOODFLOWS_OUTPUT`. Add a short upgrade note: rename job names over 64 bytes and their `depends_on`, `--jobs` and profile `partitions` references, and shorten output keys over 64 bytes and the expressions that use them. Link the bullet from "Steps and outputs". Verify the stated wording matches `NAME_RULE`.

## 5. Integration check

- [ ] 5.1 Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings` and `cargo test --locked`, and confirm they all pass. Run `goodflows validate` on the repo's own `goodflows.toml` and confirm it is still accepted.
