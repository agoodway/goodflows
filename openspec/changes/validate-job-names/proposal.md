# Proposal

## Why

goodflows checks names in five places, and they don't apply the same rule:

- `validate_name` in `src/config.rs` checks workflow, environment, step and output names in config. A name must be nonempty after trimming, at most 64 UTF-8 bytes, and free of control characters. The 64 is hardcoded rather than taken from `contract::MAX_NAME_BYTES`.
- Job names are checked for blanks and control characters, but the 64-byte limit is enforced only by `validate_reports`, so it applies only to jobs that declare `reports`.
- `valid_report_name` checks workflow, environment and job names read back from spool manifests and snapshots during replay and collection. It does not trim, so a whitespace-only name passes.
- Output keys that a step writes to `$GOODFLOWS_OUTPUT` at runtime have no length limit, while output keys declared in config are capped at 64 bytes.

The companion goodflows_web change `add-workflow-run-storage` adds `POST /api/v1/projects/:project/workflow-runs`. It accepts the run-history `Record` from `src/history.rs` (`schema_version` 1) and applies the name rule to every `results[*].name` and `requested_jobs` entry, both of which come from job names. One job name over 64 bytes makes the server reject the whole run with `422`. goodci treats `422` as pending and retries, so that workflow's runs never upload. Using one rule for every name removes this gap and any similar ones in the other checks.

## What Changes

- One shared name rule (nonempty after trimming, at most `MAX_NAME_BYTES` = 64 UTF-8 bytes, no control characters) is used by every name check in goodflows. `valid_report_name` and the separate job-name checks are removed.
- **BREAKING**: every job name, in root `[[jobs]]` and in every named workflow, must follow the rule whether or not it declares `reports` or is selected. A config with a job name over 64 bytes now fails to load. Upgrade by renaming the job and every `depends_on`, `--jobs` and profile `partitions` entry that refers to it.
- **BREAKING**: output keys a step writes to `$GOODFLOWS_OUTPUT` must follow the rule. A key over 64 bytes now fails the step with `invalid job outputs`. Upgrade by shortening the key and the expressions that reference it.
- Replay and collection apply the same rule to names read from manifests and snapshots, so a whitespace-only name is now refused. Config could never produce such a name, so only hand-edited spool files are affected.
- Every config name error uses the existing form `<kind> name must be nonempty, at most 64 UTF-8 bytes, and contain no control characters`. A job error is prefixed with `job '<escaped name>'`. This replaces `empty job name`, `job '<name>' contains invalid control characters` and `reporting job name must be ...`. The runtime output key error states the same rule.
- README states the rule once, for all name kinds, and adds a short upgrade note. The repo has no changelog. Breaking changes are documented in README upgrade sections.

## Capabilities

### New Capabilities

- `name-rule`: the single goodflows name rule and every name it governs: configured workflow, environment, job, step and output names, runtime output keys, and names in saved report manifests and snapshots.

### Modified Capabilities

- `run-report`: the requirement "Job-provided text cannot rewrite the terminal" no longer says "other job-name rules are unchanged". It requires job names to follow the name rule whether or not the job reports or is selected.

## Impact

- Code: `src/config.rs` (shared rule, `validate_name`, `validate_jobs`, `validate_reports`, removal of `valid_report_name`), `src/reporting/replay.rs`, `src/reporting/collect.rs`, `src/runner.rs` (`read_outputs`).
- Tests: the unit test `job_names_reject_control_characters_only`, `tests/runner.rs` (`empty job name`), `tests/reporting.rs` (`reporting_config_boundaries_are_accepted` uses a 100-byte job name without reports), `tests/steps.rs` and replay tests, plus new boundary tests for each kind of name.
- Users: configs with job names over 64 bytes, and steps writing output keys over 64 bytes, now fail. Scripts that match the old error strings need updating. Valid configs are otherwise unaffected.
- Downstream: run-history records, `requested_jobs`, report snapshots and manifests always carry names that the goodflows_web endpoints accept.
- `depends_on`, `--jobs`, profile `partitions` keys, log file names and `path_component` directories need no code change. See design.md.
