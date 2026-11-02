# Design

## Context

See proposal.md for motivation and specs/job-partitions/spec.md for the behavior contract.

`runner::execute` schedules a flat `&[Job]`. It indexes jobs by name, keeps one `Option<JobResult>` slot per job, and treats "dependency passed" as "the dependency's single result is `Passed`". `run_job(index, job, shared, cancel, needs)` derives log stems as `{index+1:03}-{safe_name}` (1-based position in the selected job list after `--jobs` selection, name sanitized and cut to 60 chars). It builds the merged environment from the CLI environment plus job `env`, calls `ReportingEnv::scrub` on it (removing the token and, when a `[reporting]` block exists, every `config::RESERVED_ENV` name) before evaluating the job condition, and only then builds the command env with `ReportingEnv::inject(&job.name, ..)`. Steps repeat scrub/inject over job env plus step `env`. `JobExecution::command(script, shell, env, step, is_condition)` applies `.envs(env)` and then `env_remove`s the token and, when reporting is active, every `RESERVED_ENV` name, so anything in `RESERVED_ENV` is stripped from every child of every job. It provides `GOODFLOWS_OUTPUT` whenever `is_condition` is false (command-only jobs and steps) and `env_remove`s it for conditions. Config validation (`validate_jobs` → `validate_job` → `validate_env` / `validate_condition` → `validate_expression(source, needs, steps)`) runs before any execution, `validate_credential_env_names` checks reporting `url_env`/`token_env`, and `Config::select` filters by name. `Job` uses `deny_unknown_fields` and is serialized as-is into the `jobs` event that `goodflows list` emits. `goodflows validate` emits `config_valid` with job counts and selected names only.

`upload-test-timings` is implemented and archived as `openspec/specs/test-timing-publishing/`. `ReportingEnv.jobs` is a `HashMap<String, Vec<(String, String)>>` keyed by job name, `reporting::Session::start` creates one report directory per reporting job, and `Session::finish` matches results to manifest entries with `results.iter().find(|r| r.name == name)`. That is why partitions and reports are kept apart in this change.

## Goals / Non-Goals

**Goals:** Expand partitions at the scheduler boundary so config, selection and validation stay name-based. Reuse the existing per-job execution path unchanged for each instance. Keep unpartitioned output byte-for-byte compatible.

**Non-Goals:** Profile- or CLI-level overrides of the partition count. Splitting work across partitions inside Goodflows. Per-instance report directories. Retrying single instances.

## Decisions

### D1. Config shape

Add `partitions: Option<Partitions>` to `Job`, parsed from an untagged integer or a `deny_unknown_fields` table with `count` and optional `strategy`. Normalize it to a `{ count, strategy }` struct with a `Strategy::Index` enum and serialize that normalized form, so `list`/`jobs` output always shows count and strategy. Parsing keeps the count as written (`PartitionsInput { count: i64, .. }`, deserialized from the `partitions` key); `Config::validate` range-checks it and stores the validated `Partitions { count: NonZeroU32, strategy }` on the job, so scheduling (`Job::partition_ids`) never re-checks the range and an unvalidated job is simply unpartitioned. An unknown strategy fails deserialization with a message saying only `index` is supported. Because `#[serde(untagged)]` replaces inner errors with a generic "did not match any variant" message, parse through a raw helper (`#[serde(try_from = "RawPartitions")]` or a small custom `Deserialize`) so the strategy and unknown-key errors stay specific. A count outside 1..=64 fails in `validate_job`. The cap of 64 is far above any practical slot count, bounds log-file and event fan-out, and can be raised later without changing the contract.

*Alternative:* accept only the integer now and add the table later. Rejected: accepting the table with `strategy = "index"` makes the future `"timings"` value a pure addition rather than a type change.

### D2. Expand into scheduling units inside `execute`

Before scheduling, build `units: Vec<Unit>` where `Unit { job: usize, partition: Option<config::Partition> }` and `config::Partition { index, count }` (both `u32`) comes from `Job::partition_ids`. The same type is the serialized `partition` field and tags events through `Partition::tag`. An unpartitioned job yields one unit with `None`. A partitioned job yields `count` adjacent units at the job's position. Dependencies are computed per job, not per unit: a unit is ready when, for each dependency job, every unit of that job has a `Passed` result. It is blocked once any unit of a dependency job has a non-`Passed` result (failed, timed out, cancelled, or skipped, including by the instance's own condition) and all units of that job have finished. Waiting for all siblings before skipping keeps dependents from being skipped while siblings are still running. The final skip is the same either way. While a dependency is settling, its direct dependents take neither the dependency nor the fail-fast reason, so under fail-fast they are still skipped with "dependency did not pass"; unstarted jobs further down the chain see only an unfinished dependency and may be skipped for fail-fast first, as for unpartitioned jobs. `results`, `started`, slot accounting, fail-fast and cancellation then work per unit with the existing loop structure. The returned `Vec<JobResult>` has one entry per unit in unit order. Skipped and cancelled units recorded by the scheduler (`JobResult::new`) carry the unit's partition, so every instance result is identifiable. `run_job` keeps receiving the job's index in the selected job list for its log stem, not the unit index.

`needs` construction only has to cover unpartitioned dependencies, because validation forbids output references to partitioned jobs. It reads each dependency's result through its unit (`job_units[d][0]`), never through the configured-job index, because a partitioned job earlier in the list shifts result positions.

Slots are shared without a per-job cap: a ready partitioned job launches instances in index order until `parallelism` is reached, so one job with up to 64 instances can hold every slot and later ready jobs wait. This keeps scheduling identical to N independent ready jobs; the documented mitigation is raising `parallelism` by `count - 1`. A per-job concurrency limit can be added later without changing instance semantics.

*Alternative:* expand jobs in config (synthesize `test#1`, `test#2` jobs) before calling the runner. Rejected: it leaks synthetic names into selection, `depends_on`, `needs` and reporting, which are exactly the surfaces the user wants name-based.

### D3. Instance execution

`run_job` takes the unit's `Option<Partition>`. When it is present:
- Insert `GOODFLOWS_PARTITION` and `GOODFLOWS_PARTITION_COUNT` into the merged env right after `ReportingEnv::scrub` and before the job condition is evaluated, so conditions, expressions, `inject`ed command env and step env (built from that map) all see them. Validation forbids job or step `env` from setting them, so no later merge can override them. `JobExecution` carries the pair and `command` sets them on the `Command` after the reporting `env_remove`s, so they win over any inherited value.
- The partition names MUST NOT be added to `config::RESERVED_ENV`: that array drives `ReportingEnv::scrub` and the `env_remove` loop in `command`, so adding them would strip the values from instances whenever a `[reporting]` block exists. Use a separate constant (for example `config::PARTITION_ENV`).
- Command-only partitioned jobs call `command` with a new flag (for example `outputs: bool`, or an enum replacing `is_condition`) that withholds the output file and `env_remove`s `GOODFLOWS_OUTPUT` without the condition semantics of `is_condition` (exit-code skip, `spawned` not set, `condition:` error prefix). Steps keep the current output-file behavior.
- The log stem becomes `{index+1:03}-{safe_name}.p{partition:02}` (for example `002-test.p01`), where `index` is the job's position in the selected job list, so instance logs sort next to each other in index order and never collide.

When it is absent, `run_job` removes both partition names from the merged env map (so expressions resolve them to null) and `command` calls `env_remove` for both, which handles nested invocations. `JobResult` and the `job_started`/`step_started`/`step_finished`/`job_output` events gain `partition` with `skip_serializing_if = "Option::is_none"`.

### D4. Validation

- Make `validate_env` reject `GOODFLOWS_PARTITION` and `GOODFLOWS_PARTITION_COUNT` alongside `RESERVED_ENV` (checked against the separate constant from D3). Generalize the error text, which currently says "goodflows sets it for reporting jobs".
- Make `validate_credential_env_names` reject `url_env`/`token_env` values naming either partition variable, as it already does for `RESERVED_ENV` and `GOODFLOWS_OUTPUT`. Otherwise `command`'s `env_remove(token)` would strip a partition value from instances.
- `validate_job` rejects `outputs` mappings when `partitions` is set, and rejects `partitions` together with non-empty `reports`.
- `validate_jobs` builds the set of partitioned job names and threads it through `validate_job` and `validate_condition` (job conditions, step conditions and output mappings) to `validate_expression`, which rejects `Reference::Needs { job, .. }` for those jobs with "partitioned job '<name>' publishes no outputs".
- These checks run for top-level jobs and named workflows through the existing paths.

### D5. Output

Text rendering formats an instance's display name as `name (i/N)` wherever `job_started`, `step_started`, `job_output`, `step_finished` and `job_finished` print a name. A small helper reads `event["partition"]` and returns the decorated name. `run_started.jobs` reports the unit count (main passes it from the expanded count). Summary counters already iterate results, so they count units without changes. The `list` text output (the `jobs` event renderer in `src/output.rs`) appends `partitions: N (strategy=<strategy>)` to partitioned jobs. JSON `jobs` events pick up the normalized `partitions` field through serialization, with `skip_serializing_if = "Option::is_none"` so unpartitioned jobs keep their shape. `config_valid` (`goodflows validate`) is unchanged: it carries counts and names only, and its counts stay per configured job.

### D6. Reporting interaction

`validate_job` makes `partitions` and `reports` mutually exclusive. The follow-up change `partitioned-report-collection` will key report directories by (job, partition), take `partition`/`partition_count` from Goodflows, and lift this rejection. No `src/reporting/` code changes here: `Session::finish` finds results by job name, and with the rejection in place no partitioned job has a manifest entry. A config may still have a `[reporting]` block alongside partitioned jobs that do not report; D3 keeps their partition variables intact in that case.

## Risks / Trade-offs

- [Consumers that size `parallelism` for one test slot will see static checks queue behind instances] -> Document raising profile `parallelism` by `count - 1`. Instances already reflect real process load.
- [Tools that ignore the partition variables run the full suite N times] -> Document that the command must consume `GOODFLOWS_PARTITION`/`GOODFLOWS_PARTITION_COUNT`. Goodflows cannot detect this.
- [JSON consumers keyed by job name now see repeated names] -> `partition` is always present on repeated names, and unpartitioned output is unchanged. Document that `(name, partition.index)` is the instance key.
- [Expansion changes the internal result-vector shape] -> The results order stays deterministic (unit order). Reporting (`Session::finish`, which matches by name) is the only other consumer and is excluded by D6.

## Migration Plan

Release the CLI with partitions support. GoodCases then adds a per-partition Mix task driven by the new variables, switches its `test` job to `partitions = 3`, and raises its orb/local profile parallelism. Those steps are `../goodcases/openspec/changes/adopt-goodflows-partitions/`. Rollback is to remove `partitions` and restore the previous command. Configs without `partitions` are unaffected.

## Open Questions

- Whether environment profiles should override `partitions` (for example, more partitions on larger machines). This can be added later as a settings override without changing instance semantics.
