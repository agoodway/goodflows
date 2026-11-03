# Design

## Context

See proposal.md for motivation. A job's `partitions` value is parsed into `partitions_input`, range-checked in `Config::validate`, and stored on `job.partitions` as `{ count, strategy }`. `Config::workflow` clones those jobs and applies profile `settings` (`parallelism`, `fail_fast`, `workdir`) on top of shared and workflow settings. The runner, text and JSON output, and report collection all expand instances from `Job::partition_ids`, which reads that stored count. `Environment` is `deny_unknown_fields` and holds only `settings`.

`add-settings-env` is in progress and adds `env` to settings models. `improve-run-report` modifies the job-partitions requirement "Output identifies each instance" and does not change inspection or the declared count. This change leaves that requirement alone.

## Goals / Non-Goals

**Goals:** Resolve the effective count once, on the jobs `Config::workflow` returns, so list, validate, scheduling, partition variables, logs, summaries, and report envelopes follow it without further branches.

**Non-Goals:** See proposal.md. In particular, no new runner or reporting code, no CLI flag, and no change to how strategy is stored.

## Decisions

### Put the map on the environment, beside settings

Add `partitions: BTreeMap<String, i64>` to `Environment`, default empty, omitted from serialized TOML when empty. Values stay `i64` until validation, matching job counts, so `0`, `65`, and negatives produce one range error instead of a type error.

```toml
[environments.local.partitions]
test = 6
```

A profile may contain only this table. `settings` already defaults to empty.

Alternatives considered: a single integer on `settings` would apply one count to every partitioned job. Nesting under `[environments.local.jobs.test]` would invite overriding command, env, and timeout, which this change does not do. Putting the map on `SettingsOverrides` would mix a per-job table into run-wide settings and collide with `add-settings-env`'s `env` field. A workflow-qualified key is unnecessary while profiles stay global, the same way `parallelism` is global.

### Check names in two phases

`Config::validate` builds the set of job names that declare `partitions` across root jobs and every named workflow. Every profile entry, selected or not, must use a count from 1 to 64 and a name in that set. This catches typos and profiles that point only at unpartitioned jobs before any command runs. A name that matches no job at all gets its own message listing the partitioned jobs, in the style of the unknown-environment error, so a typo is not reported as an existing job.

`Config::workflow` then applies the selected profile to the cloned jobs of the selected workflow only:

- The job declares `partitions`: replace `Partitions.count` and keep `strategy`.
- The workflow has no job by that name: ignore the entry. A laptop profile can mention `test` and still run a workflow that has no such job.
- The workflow has the job and it does not declare `partitions`: fail. The same name may be partitioned in one workflow and plain in another; only the plain selection fails. The check covers the whole resolved workflow, before `--jobs` selection.

Range and name errors use the existing job context style: `environment 'local': job 'test': partitions count must be from 1 to 64, got 0`, `environment 'local': unknown job 'missing' (partitioned jobs: test)`, and `environment 'local': job 'test' is not partitioned`. The per-workflow failure from `Config::workflow` also names the workflow, because `validate --env`, `list --workflows --env`, and JSON error events otherwise cannot tell which workflow failed: `environment 'local': workflow 'nightly': job 'test' is not partitioned`. For root jobs it reads `workflow 'default' (root jobs)`. Job and profile counts share one `partition_count(i64) -> Result<NonZeroU32>` range check; each caller adds its job or profile context.

Aggregate `goodflows validate` without `--env` additionally resolves every workflow under every declared profile, so a config that passes plain `validate` cannot fail `run <workflow> --env <profile>` at this check. `list --workflows` without `--env` and single-workflow `validate <workflow>` without `--env` do not do this.

Non-integers and tables fail while parsing the TOML. The value is an integer only, unlike a job's `partitions` field, which accepts an integer or a `{ count, strategy }` table and therefore has a custom visitor. Keep native TOML diagnostics for these type errors: they identify the source location, offending entry, and expected integer type, but need not include the full profile path. Semantic range and name errors must name the profile and job. No custom visitor.

### Replace the count before any caller sees the jobs

Currently `Config::environment` returns only `Option<&SettingsOverrides>` and `Config::workflow` accepts that settings reference. Have `Config::environment` return `Option<&Environment>` and have `Config::workflow` accept the selected profile name as `Option<&str>`, resolving the full profile through that lookup. Preserve unknown-profile diagnostics in `Config::environment`; apply `environment.settings` through the existing settings precedence and use the sibling partition map for count replacement and the selected name for contextual errors. Update the `src/main.rs` callers for individual workflow resolution and the aggregate validation/workflow discovery loop. No CLI syntax changes are needed.

`list`, `validate`, aggregate validation, workflow discovery, and run all obtain jobs through `Config::workflow`. Replacing `Partitions.count` there is enough. `partition_ids` feeds the scheduler, `GOODFLOWS_PARTITION` and `GOODFLOWS_PARTITION_COUNT`, event tags, log names, summary counts, and `reporting::Session`, which already loops `job.partition_ids()`. List JSON serializes `job.partitions`, and the text renderer prints `partitions["count"]` and `strategy`. Neither path needs a second lookup.

Also set `partitions_input.count` on the resolved copy so the job stays self-consistent. `partitions_input` is never serialized, so list and validate output are unaffected; they already read `partitions`. Do not put the profile map on `EffectiveSettings`. Inspection shows the count a run will use, as it already shows effective parallelism.

Count replacement itself does not change parallelism. Shared, workflow, selected-profile settings, and CLI overrides keep their existing parallelism precedence, including when one profile contains both `settings.parallelism` and a partition map.

Outputs stay illegal because the job still declares `partitions`. Changing the count cannot make an outputs mapping legal.

## Risks / Trade-offs

- [A shared profile names a job that is partitioned in one workflow and plain in another] -> The plain workflow fails with the profile, workflow, and job named, even when `--jobs` excludes that job (see Open Questions). Document that an entry applies only where that job declares `partitions`.
- [Users expect the profile to raise parallelism with the count] -> Leave parallelism independent and cover the existing "extra instances wait" behavior. README says to size it for the effective count.
- [List hides the declared count once a profile is selected] -> The file still holds the declared count. Showing the effective count matches `list --env` for other settings.
- [`add-settings-env` also edits `Environment` and the profile section of the README] -> This field is a sibling of `settings`, so the models merge. Resolve the textual overlap in `config.rs` and the README when both land.
- [Older binaries reject the new field] -> Call that out in the README. No rewrite of existing files.

## Open Questions

### A profile entry blocks every command for a workflow with a same-named plain job

The spec requires `Config::workflow` to fail when the selected workflow has an unpartitioned job named by the profile. That check runs on the whole workflow before `config.select` applies `--jobs`, so with `test = 6` in `local`, every command for a workflow that has a plain `test` job fails under `--env local`, including `goodflows run nightly lint --env local`, which would never run `test`. This change keeps the spec-mandated behavior. Options for a follow-up:

- Scope the check to the jobs `--jobs` selects (plus their dependencies), so unselected plain jobs do not fail the run. Aggregate commands would still check every job.
- Treat a same-name plain job like an absent one and ignore the entry, accepting that a typo against a plain job in that workflow goes unreported there.
- Add workflow-scoped keys such as `[environments.local.workflows.ci.partitions]`, so a profile can target one workflow's job without affecting others.

## Migration Plan

Existing configs stay valid. A user who wants a machine-specific count keeps `partitions` on the job (the bare-run count) and adds `[environments.<profile>.partitions]`. Rollback is removing that table. Older binaries reject the field. Manifests and report envelopes already store the instance count from the run, so replay does not change. GoodCases dropping its `GOODFLOWS_ENVIRONMENT` case is a separate change in that repo.
