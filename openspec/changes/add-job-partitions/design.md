# Design

## Context

See proposal.md for motivation and specs/job-partitions/spec.md for the behavior contract.

`runner::execute` schedules a flat `&[Job]`. It indexes jobs by name, keeps one `Option<JobResult>` slot per job, and treats "dependency passed" as "the dependency's single result is `Passed`". `run_job` derives log stems from the job index plus a sanitized name. It builds the merged environment and calls `ReportingEnv::apply` before evaluating any expression. `JobExecution::command` already uses `env_remove` to keep `GOODFLOWS_OUTPUT` away from conditions. Config validation (`validate_jobs`, `validate_job`, `validate_env`, `validate_expression`) runs before any execution, and `Config::select` filters by name. `Job` uses `deny_unknown_fields` and is serialized as-is into `jobs`/`list` events.

The uncommitted `upload-test-timings` work on this branch keys its per-job report environment and collection by job name. That is why partitions and reports are kept apart in this change.

## Goals / Non-Goals

**Goals:** Expand partitions at the scheduler boundary so config, selection and validation stay name-based. Reuse the existing per-job execution path unchanged for each instance. Keep unpartitioned output byte-for-byte compatible.

**Non-Goals:** Profile- or CLI-level overrides of the partition count. Splitting work across partitions inside Goodflows. Per-instance report directories. Retrying single instances.

## Decisions

### D1. Config shape

Add `partitions: Option<Partitions>` to `Job`, parsed from an untagged integer or a `deny_unknown_fields` table with `count` and optional `strategy`. Normalize it to a `{ count, strategy }` struct with a `Strategy::Index` enum and serialize that normalized form, so `list`/`jobs` output always shows count and strategy. An unknown strategy fails deserialization with a message saying only `index` is supported. A count outside 1..=64 fails in `validate_job`. The cap of 64 is far above any practical slot count, bounds log-file and event fan-out, and can be raised later without changing the contract.

*Alternative:* accept only the integer now and add the table later. Rejected: accepting the table with `strategy = "index"` makes the future `"timings"` value a pure addition rather than a type change.

### D2. Expand into scheduling units inside `execute`

Before scheduling, build `units: Vec<Unit>` where `Unit { job: usize, partition: Option<Partition { index, count }> }`. An unpartitioned job yields one unit with `None`. A partitioned job yields `count` adjacent units at the job's position. Dependencies are computed per job, not per unit: a unit is ready when, for each dependency job, every unit of that job has a `Passed` result. It is blocked once any unit of a dependency job has a non-`Passed` result and all units of that job have finished. Waiting for all siblings before skipping keeps dependents from being skipped while siblings are still running. The final skip is the same either way. `results`, `started`, slot accounting, fail-fast and cancellation then work per unit with the existing loop structure. The returned `Vec<JobResult>` has one entry per unit in unit order.

`needs` construction only has to cover unpartitioned dependencies, because validation forbids output references to partitioned jobs.

*Alternative:* expand jobs in config (synthesize `test#1`, `test#2` jobs) before calling the runner. Rejected: it leaks synthetic names into selection, `depends_on`, `needs` and reporting, which are exactly the surfaces the user wants name-based.

### D3. Instance execution

`run_job` takes the unit's `Option<Partition>`. When it is present:
- After building the merged env (job env, then step env), insert `GOODFLOWS_PARTITION` and `GOODFLOWS_PARTITION_COUNT` before `ReportingEnv::apply` and before any expression evaluation. Expressions and every child process then see them. `JobExecution` carries the pair so `command` sets them on the `Command` as well.
- Command-only jobs call `command` so that `GOODFLOWS_OUTPUT` is removed rather than provided. Steps keep the current output-file behavior.
- The log stem becomes `{job_index:03}-{safe_name}.p{index}`, so instance logs sort next to each other and never collide.

When it is absent, `command` calls `env_remove` for both partition variables, which handles nested invocations. `JobResult` and the `job_started`/`step_started`/`step_finished`/`job_output` events gain `partition` with `skip_serializing_if = "Option::is_none"`.

### D4. Validation

- Add `GOODFLOWS_PARTITION` and `GOODFLOWS_PARTITION_COUNT` to the reserved env names checked by `validate_env`. Generalize the error text, which currently says "for reporting jobs".
- `validate_job` rejects `outputs` mappings when `partitions` is set, and rejects `partitions` together with non-empty `reports`.
- `validate_jobs` builds the set of partitioned job names and passes it to `validate_expression`, which rejects `Reference::Needs { job, .. }` for those jobs with "partitioned job '<name>' publishes no outputs".
- These checks run for top-level jobs and named workflows through the existing paths.

### D5. Output

Text rendering formats an instance's display name as `name (i/N)` wherever `job_started`, `step_started`, `job_output`, `step_finished` and `job_finished` print a name. A small helper reads `event["partition"]` and returns the decorated name. `run_started.jobs` reports the unit count (main passes it from the expanded count). Summary counters already iterate results, so they count units without changes. The `list` text output appends `partitions: N` to partitioned jobs. JSON `jobs` events pick up the normalized `partitions` field through serialization.

### D6. Reporting interaction

`validate_job` makes `partitions` and `reports` mutually exclusive. The follow-up change `partitioned-report-collection` will key report directories by (job, partition), take `partition`/`partition_count` from Goodflows, and lift this rejection. No reporting code changes here.

## Risks / Trade-offs

- [Consumers that size `parallelism` for one test slot will see static checks queue behind instances] -> Document raising profile `parallelism` by `count - 1`. Instances already reflect real process load.
- [Tools that ignore the partition variables run the full suite N times] -> Document that the command must consume `GOODFLOWS_PARTITION`/`GOODFLOWS_PARTITION_COUNT`. Goodflows cannot detect this.
- [JSON consumers keyed by job name now see repeated names] -> `partition` is always present on repeated names, and unpartitioned output is unchanged. Document that `(name, partition.index)` is the instance key.
- [Expansion changes the internal result-vector shape] -> The results order stays deterministic (unit order). Reporting, the only other consumer, is excluded by D6.

## Migration Plan

Release the CLI with partitions support. GoodCases then adds a per-partition Mix task driven by the new variables, switches its `test` job to `partitions = 3`, and raises its orb/local profile parallelism. Those steps are `../goodcases/openspec/changes/adopt-goodflows-partitions/`. Rollback is to remove `partitions` and restore the previous command. Configs without `partitions` are unaffected.

## Open Questions

- Whether environment profiles should override `partitions` (for example, more partitions on larger machines). This can be added later as a settings override without changing instance semantics.
