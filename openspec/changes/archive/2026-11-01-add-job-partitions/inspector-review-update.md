# Inspector Review-Update — add-job-partitions

**Reviewed:** 2026-11-01
**Reviewer:** inspector-review-update
**Mode:** auto
**Verdict:** Ready to implement

## Summary

The change adds per-job `partitions`, which expand into scheduled instances that carry `GOODFLOWS_PARTITION`/`GOODFLOWS_PARTITION_COUNT`. Its behaviour contract held up. Its codebase description was stale, though: it named `ReportingEnv::apply`, which does not exist, called upload-test-timings uncommitted, and claimed no durable capabilities. It also had a latent bug. Adding the partition names to `config::RESERVED_ENV` would have caused the reporting scrub to strip them from instances whenever a `[reporting]` block exists. All findings are patched.

**Original counts:** Critical: 1 · Warning: 6 · Suggestion: 4
**Patches:** Auto-patched: 8 · User-guided: 0 · Model-recommended: 3 · Skipped: 0

## Scope inspected

- Proposal: `openspec/changes/add-job-partitions/proposal.md`
- Design: `openspec/changes/add-job-partitions/design.md`
- Tasks: `openspec/changes/add-job-partitions/tasks.md`
- Deltas:
  - `openspec/changes/add-job-partitions/specs/job-partitions/spec.md` (ADDED)
  - `openspec/changes/add-job-partitions/specs/test-timing-publishing/spec.md` (MODIFIED, new in this pass)
- Canonical specs consulted: `openspec/specs/test-timing-publishing/spec.md`
- Other active changes consulted: `partitioned-report-collection`, `generalize-test-report-filters`; archive `2026-11-01-upload-test-timings`
- Code verified: `src/config.rs`, `src/runner.rs`, `src/main.rs`, `src/output.rs`, `src/expression.rs`, `src/reporting/mod.rs`, `tests/`, `README.md`

## Patches applied

8 findings were auto-patched and 3 were patched from model recommendations. None were skipped.

### Auto-patched

1. **(Critical) Partition names must not go into `RESERVED_ENV`.** `design.md` D3/D4 and `tasks.md` 1.2. `RESERVED_ENV` drives `ReportingEnv::scrub` and the reporting `env_remove` loop in `JobExecution::command`. Adding the partition names there would strip them from instances whenever a reporting block exists. The design now requires a separate constant. A spec scenario "Reporting block present" and a matching task test were added.
2. **Stale `ReportingEnv::apply` reference.** `design.md:7`, D3. Rewritten to describe the real flow: `scrub` runs before the condition, then `inject(&job.name, ..)` and the `env_remove`s run in `command`. Partition insertion is placed after `scrub` and before the condition.
3. **upload-test-timings described as uncommitted or in progress.** `design.md:9`, `proposal.md:35`. Now states that it is archived as `test-timing-publishing`, that `ReportingEnv.jobs` is keyed by name, and that `Session::finish` matches results by name.
4. **Mechanism for withholding `GOODFLOWS_OUTPUT` was unspecified.** `design.md` D3, `tasks.md` 3.2. `command` provides the output file whenever `!is_condition`. Reusing `is_condition` would also bring in its exit-code skip, the unset `spawned` flag and the `condition:` error prefix. The design now calls for a separate flag.
5. **Log stem used an ambiguous `job_index`.** `design.md` D3, `tasks.md` 3.3. Clarified as `{index+1:03}`, the 1-based position of the job in the selected job list (after `--jobs`), with `run_job` receiving the job index rather than the unit index.
6. **Validation threading.** `design.md` D4. `validate_expression` is reached through `validate_job` → `validate_condition`. The partitioned-job set must be threaded through those functions.
7. **Serde untagged error messages.** `design.md` D1, `tasks.md` 1.1. With `#[serde(untagged)]` the specific "only `index`" error would be lost. The design now specifies a `try_from`/custom deserializer.
8. **Small details.** README section names in `tasks.md` 4.3 now read "Scheduling and failures" and "Logs and machine output". Scheduler-recorded skipped and cancelled `JobResult`s carry `partition` (D2, task 4.1). `job_output` is emitted by `drain` (task 4.1). `main.rs` selection is unchanged (proposal Impact).

### Model-recommended patches

1. **Reporting `url_env`/`token_env` naming a partition variable.** New delta `specs/test-timing-publishing/spec.md`, plus `design.md` D4, `proposal.md` Modified Capabilities and `tasks.md` 1.2.
   - **Chose:** extend `validate_credential_env_names` and the canonical "Reporting is explicitly configured" requirement to reserve `GOODFLOWS_PARTITION` and `GOODFLOWS_PARTITION_COUNT`.
   - **Rationale:** `command` calls `env_remove(token)` after `.envs(env)`, so a token named `GOODFLOWS_PARTITION` would silently remove the instance's partition. This follows the existing rule for `RESERVED_ENV` and `GOODFLOWS_OUTPUT`, and it does not overlap the requirement that `partitioned-report-collection` modifies.
2. **Inherited partition values in unpartitioned expressions.** `specs/job-partitions/spec.md` "Instances receive their partition identity", Nested invocation scenario, `design.md` D3, `tasks.md` 3.1.
   - **Chose:** remove both names from the unpartitioned merged env map as well, so `env.GOODFLOWS_PARTITION` is null.
   - **Rationale:** this mirrors the reporting rule that reserved values "MUST NOT reach any command or expression". It also keeps expressions and commands consistent.
3. **`goodflows validate` showing partition count.** `specs/job-partitions/spec.md` "Inspection shows partition configuration", `tasks.md` 1.3.
   - **Chose:** only `list` (text and the `jobs` JSON event) shows count and strategy. `config_valid` is unchanged and counts configured jobs.
   - **Rationale:** `config_valid` carries only counts and names today (`src/main.rs:250-270`). This is the smallest change and leaves the validate output schema intact.

### Skipped

_None._

## Critical

_None._

## Warning

_None._

## Suggestion

1. **Replay of old manifests.** `specs/test-timing-publishing/spec.md`. Replay also applies `validate_credential_env_names`. A saved manifest whose `token_env` was `GOODFLOWS_PARTITION` would now be refused. This is implausible in practice and acceptable.

## Alignment notes

- **Other active changes:** `partitioned-report-collection` removes "Partitions are not combined with reporting" and modifies "Collection belongs to one invocation and job". This change modifies only "Reporting is explicitly configured", so there is no overlap.
- **Canonical specs:** the MODIFIED delta copies the full canonical requirement plus one scenario.
- **Codebase assumptions verified:** `Job` has `deny_unknown_fields` and no `partitions` field. `validate_env`'s error text is "goodflows sets it for reporting jobs". `Reference::Needs { job, key }` exists. `Config::select` is name-based. `run_started.jobs` comes from `config.jobs.len()`. Summary counters iterate results. `tests/{inspection,scheduling,runner,steps,output}.rs` exist. The expression syntax `env.X == "1"` is valid. `../goodcases/openspec/changes/adopt-goodflows-partitions/` exists.

## What looks good

- Expanding at the scheduler keeps selection, `depends_on` and `needs` name-based.
- Blocking only after every sibling finishes avoids premature skips.
- `partition` is omitted for unpartitioned jobs (`skip_serializing_if`), which keeps existing JSON byte-compatible.
- Rejecting partitions together with reports until per-instance collection exists is a clean sequencing boundary.
