# Proposal

## Why

A partitioned job has one count for every machine. GoodCases already runs six test partitions locally and three in an orb by branching on `GOODFLOWS_ENVIRONMENT` inside a single command, because a profile can change runner settings and cannot change the job. Scheduled partitions need the count before the command starts, or that split disappears the moment the job declares `partitions`.

## What Changes

- Accept an integer partition count per job on an environment profile: `[environments.<profile>.partitions] <job> = <count>`.
- When that profile is selected, replace the count of a job that already declares `partitions`. Keep the job's strategy. A bare run and a profile that omits the job keep the declared count.
- Use the effective count for scheduling, `GOODFLOWS_PARTITION` / `GOODFLOWS_PARTITION_COUNT`, events, logs, summaries, and `goodflows list`.
- Validate every profile at load, selected or not. Out-of-range counts and names that match no partitioned job fail before execution. A selected profile that names an unpartitioned job in the selected workflow fails. A name absent from the selected workflow is unused.
- Document the syntax and that parallelism stays a separate setting.

Out of scope: adding or removing partitioning, overriding strategy, a `--partitions` flag, resizing parallelism from the count, workflow-qualified profile keys, and the GoodCases migration.

## Capabilities

### New Capabilities

None.

### Modified Capabilities

- `job-partitions`: A selected environment profile replaces the partition count of a job that already declares `partitions`. Inspection shows that effective count.

## Impact

- `src/config.rs`: profile partition map, load-time checks, and count replacement when a workflow is selected.
- `src/main.rs`: carry the selected profile name and full profile into workflow resolution for runs, inspection, aggregate validation, and workflow discovery.
- `README.md`: environment profiles and partitioned jobs.
- `tests/inspection.rs`, `tests/workflows.rs`, `tests/runner.rs`, and `tests/reporting.rs`: cover effective instance counts and their inspection, output, log, and report metadata.
- `goodflows list` and run output already read the resolved job, so events, logs, and report envelopes follow the effective count without a wire change.
- Existing configs keep their counts. Older binaries reject the new profile field. The in-progress `add-settings-env` change adds `settings.env` and does not share this field.
