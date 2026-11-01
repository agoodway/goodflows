# Proposal

## Why

Projects such as GoodCases fan tests out inside a single job (`mix test.parallel`), so Goodflows schedules one slot while several processes run, cannot show per-partition status or logs, and has no place to later balance partitions from uploaded timing history. Moving fan-out into Goodflows makes partitions first-class scheduled work now and gives timing-balanced assignment (a later change) a home without changing the config shape again.

## What Changes

- Add an optional per-job `partitions = N` setting. A partitioned job expands into N scheduled instances, each occupying its own `parallelism` slot with its own log, status, timeout budget and events.
- Inject reserved `GOODFLOWS_PARTITION` (1..N) and `GOODFLOWS_PARTITION_COUNT` into every command, step and shell condition of an instance; reject configured overrides of these names in any job or step `env`.
- Keep instances addressed by the job name, carrying partition index/count: text output shows `name (i/N)`, JSON events and results include a partition field, and each instance gets a distinct log file.
- `--jobs <name>` selects all instances; `depends_on = ["<name>"]` waits for every instance to pass, and any failed, timed-out or cancelled instance skips dependents as an ordinary dependency failure would.
- Summary counts instances. Existing fail-fast, condition, step and cancellation rules apply to each instance unchanged.
- Partitioned jobs publish no job outputs: reject `outputs` mappings on them and `needs.<partitioned>.outputs.*` references to them. Step outputs still work inside each instance.
- Accept partition counts from 1 to 64. `partitions = 1` is allowed and produces a single instance with partition variables set.
- Reject combining `partitions` with `reports` until the follow-up layout change gives each instance its own report directory.
- Treat the integer form as shorthand for an index strategy (Goodflows supplies partition identity; the tool splits work). Reserve, but do not implement, a table form for later strategies such as timing-balanced file assignment.
- Unpartitioned jobs, existing workflows and the JSON schema for unpartitioned jobs keep their current behavior.

Out of scope: assigning files to partitions, balancing from timing history, selecting a single instance from the CLI, merging instance outputs, and changing the `upload-test-timings` report directory layout (a separate follow-up change).

## Capabilities

### New Capabilities

- `job-partitions`: per-job partition configuration, expansion into scheduled instances, reserved partition environment, instance identity in selection/dependencies/output, and validation rules.

### Modified Capabilities

None. This repository has no durable OpenSpec capabilities yet.

## Impact

- CLI: `src/config.rs` (job field, validation, reserved env names), `src/runner.rs` (instance expansion, slot accounting, dependency completion across instances, per-instance logs), `src/output.rs` (partition in text and JSON events, summary counts), `src/main.rs` (selection), integration tests and README.
- Interaction with the in-progress `upload-test-timings` change: that change is left unchanged. `partitioned-report-collection` will give each instance its own report directory and take partition identity from Goodflows.
- Consumers: GoodCases would replace the fan-out part of `mix test.parallel` with a per-partition task driven by `GOODFLOWS_PARTITION`/`GOODFLOWS_PARTITION_COUNT`, and raise profile `parallelism` so instances do not starve other checks. That migration is `../goodcases/openspec/changes/adopt-goodflows-partitions/`.
