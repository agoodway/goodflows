# Tasks

## 1. Partition configuration and validation

- [ ] 1.1 Add `partitions` to `Job` in `src/config.rs`, accepting an integer or a `{ count, strategy }` table (strategy `index` only, unknown keys rejected), normalized and serialized as `{ count, strategy }`; verify with unit tests for integer, table, default strategy, `strategy = "timings"`, unknown key and non-integer values in `cargo test --locked`.
- [ ] 1.2 Validate counts in 1..=64. Reject `outputs` mappings on partitioned jobs, `partitions` together with `reports`, and `needs.<partitioned>.outputs.*` references in conditions and output mappings. Add `GOODFLOWS_PARTITION`/`GOODFLOWS_PARTITION_COUNT` to the reserved env names and generalize the reserved-variable error text. Verify with inspection tests in `tests/inspection.rs` that each invalid config fails `goodflows validate` before execution, for both top-level jobs and named workflows.
- [ ] 1.3 Show partition count/strategy in `list` text output and in `jobs`/`config_valid` JSON while keeping defined-job counts per configured job; verify with `tests/inspection.rs` cases for text and `--json` output.
- [ ] 1.4 Document the `partitions` field, accepted forms, 1..=64 range, reserved variables, the no-outputs and no-reports rules, and the reserved future strategies in the README Configuration section; verify the documented example passes `goodflows validate` with the built CLI.

## 2. Scheduler expansion

- [ ] 2.1 In `runner::execute`, expand jobs into ordered units (job index plus optional partition), track results/started state per unit, and resolve readiness and blocking per dependency job across all of its units (blocked only after all sibling units finish); verify with `tests/scheduling.rs` cases: all instances pass and the dependent runs, one instance fails and the dependent is skipped after siblings finish, an upstream failure skips every instance, and results are in unit order.
- [ ] 2.2 Apply slot accounting, fail-fast and cancellation per unit; verify with `tests/scheduling.rs` cases for parallelism 4 with 3 instances leaving a free slot, parallelism 2 with 3 instances queuing the third, fail-fast skipping an unstarted sibling while a running sibling finishes, and a signal cancelling running and unstarted instances with the signal exit code preserved.
- [ ] 2.3 Give each instance its own `timeout_secs` budget and failure tails; verify with a `tests/runner.rs` case where one instance times out and the others pass.

## 3. Instance environment and logs

- [ ] 3.1 Pass the unit's partition to `run_job`, insert `GOODFLOWS_PARTITION`/`GOODFLOWS_PARTITION_COUNT` into the merged env before reporting adjustments and expression evaluation, set them on every child `Command`, and `env_remove` both for unpartitioned jobs; verify with `tests/runner.rs` cases for shell and non-shell commands, steps, shell conditions, an expression condition selecting only partition 1, and an unpartitioned job launched with both variables exported in the parent CLI environment.
- [ ] 3.2 Withhold `GOODFLOWS_OUTPUT` from command-only partitioned jobs while keeping step output files and step-to-step outputs inside an instance; verify with `tests/steps.rs` cases for a command-only instance (variable absent, job outputs empty) and a two-step instance reading `steps.a.outputs.key`.
- [ ] 3.3 Use per-instance log stems (`NNN-name.pI`) in `run_job`; verify with a `tests/runner.rs` `--log-dir` case that a 3-partition job writes three distinct stdout and three distinct stderr logs, each containing only its own instance's output.

## 4. Output

- [ ] 4.1 Add an optional `partition` field to `JobResult` and to `job_started`, `step_started`, `step_finished` and `job_output` events, omitted for unpartitioned jobs; verify with `tests/output.rs` that instance events carry `{"index","count"}`, every `--json` line remains a valid versioned event, and existing unpartitioned event assertions are unchanged.
- [ ] 4.2 Render instance names as `name (i/N)` in text output for job and step lines, and report the unit count in `run_started`; verify with `tests/output.rs` text-mode assertions and a summary case where `compile` plus a 3-partition `test` reports 4 jobs and 4 passed.
- [ ] 4.3 Document instance naming, the `(name, partition.index)` JSON key, per-instance logs, slot usage and the guidance to raise `parallelism` by `count - 1` in the README Scheduling and Logs sections; verify the documented output examples against a run of the built CLI on a sample config.

## 5. Integration check

- [ ] 5.1 Run `cargo fmt --check`, `cargo clippy --locked -- -D warnings`, `cargo test --locked` and the repository's own `goodflows` checks. Then run the built CLI against a scratch config with `compile` and a 3-partition `test` whose command echoes `$GOODFLOWS_PARTITION/$GOODFLOWS_PARTITION_COUNT`, in both text and `--json` modes. Completion requires all checks to pass and the scratch run to show three correctly identified instances.
