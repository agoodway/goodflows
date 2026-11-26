---
format: goodflows-change/1
id: validate-job-names
status: proposed
size: small
door: two_way
capabilities: [name-rule, run-report]
depends_on: []
goals:
  - id: G1
    text: One name rule governs every name goodflows accepts, in configuration, in output keys steps write at runtime, and in saved report manifests and snapshots.
  - id: G2
    text: Every name goodflows sends to goodflows_web, including run-history results[*].name and requested_jobs, passes the server's name rule, so no run is rejected with 422 for a name.
  - id: G3
    text: A name error says which kind of name broke the rule and, for jobs, steps and output keys, which one, and never prints raw control characters.
non_goals:
  - id: NG1
    text: Other string rules keep their own semantics, including project slugs, environment variable names, git branch and commit checks, contract identifiers, partition labels and text fields capped by MAX_TEXT_BYTES.
  - id: NG2
    text: No change to how depends_on, --jobs and profile partitions keys are checked; they must match a configured name, so the rule reaches them indirectly.
  - id: NG3
    text: Existing history and spool files are not rewritten or migrated.
anti_goals:
  - id: AG1
    text: Do not keep two name predicates that are meant to agree; that drift caused this inconsistency.
  - id: AG2
    text: Do not truncate, rename or silently drop a name that breaks the rule; refuse it with an error.
decisions:
  - id: D1
    question: How is the rule held in code?
    answer: One predicate holds the rule (trimmed nonempty, at most MAX_NAME_BYTES UTF-8 bytes, no control characters) and every name check calls it, with the length taken from the reporting contract's MAX_NAME_BYTES rather than a literal. valid_report_name is deleted.
    by: agent
    why: A single predicate cannot drift, and the server's limit is already defined in the contract.
    rejected:
      - Keep two predicates and make them agree, because they would drift apart again.
  - id: D2
    question: How are job names checked?
    answer: The job name check is the first check in the per-job loop of job validation, for every job in root [[jobs]] and every named workflow, whether or not it declares reports or is selected. It replaces the blank-name and control-character checks; the duplicate check stays. The separate name check in reports validation is removed because it can no longer fail.
    by: agent
    why: Checking first reports a bad name before other errors on that job, and covering every job closes the gap where only reporting jobs were capped at 64 bytes.
  - id: D3
    question: How are runtime output keys checked?
    answer: Keys written to $GOODFLOWS_OUTPUT are checked with the same predicate, and a bad key goes through the existing invalid job outputs failure path, which already fails the command, publishes no outputs and skips dependents.
    by: agent
    why: Keys are only known at runtime, and config already caps declared outputs keys at 64 bytes; reusing the existing path changes no other runtime behavior.
  - id: D4
    question: Where does the rule live in the specs?
    answer: It gets its own capability, name-rule. run-report keeps only its terminal-safety requirement and refers to the rule for job names. test-timing-publishing already requires the name rules during replay and a 64-byte cap for reporting jobs, both stay true, so it is not changed.
    by: agent
    why: The rule now covers five kinds of names across config, runtime and replay, which would otherwise be scattered across run-report, test-timing-publishing and unowned specs.
  - id: D5
    question: Do the places that already assume valid job names need changes?
    answer: No. depends_on, --jobs and profile partitions keys resolve against configured job names; path_component job components were already at most 64 bytes and stay at most 192 encoded bytes; log file stems are already truncated to 60 sanitized characters; history results, report rows and snapshots carry configured names.
    by: agent
    why: All of them derive from configured names, so validating names at load makes them valid too.
  - id: D6
    question: How are users told about the breaking change?
    answer: The README states the rule once, for workflow, environment, job, step and output names including runtime output keys, with a short upgrade note, and the next release notes repeat the upgrade note. The repo has no changelog.
    by: agent
    why: Configs with job names over 64 bytes and steps writing output keys over 64 bytes now fail, and the fix is a rename of the name and every reference to it.
  - id: D7
    question: Do replay and collection change?
    answer: They call the shared predicate for workflow, environment and job names in manifests and snapshots, so a whitespace-only name, which only hand-editing a spool file can produce, is now refused. Replay's existing error messages stay as they are.
    by: agent
    why: The old report check did not trim, so whitespace-only names reached the server, which refuses them.
left_to_builder:
  - Config name errors keep the existing wording "<kind> name must be nonempty, at most 64 UTF-8 bytes, and contain no control characters"; a job error is prefixed with job '<name escaped with escape_debug>', and inside a named workflow with that workflow's prefix as today.
  - The runtime output key error is wrapped by the existing "invalid job outputs" message and states the same rule.
  - Keep the rule's message text in one constant and add a unit test that ties the 64 in it to MAX_NAME_BYTES, so the constant and the message cannot drift.
  - The old messages "empty job name", "job '<name>' contains invalid control characters", "reporting job name must be ..." and "output keys must be nonempty and contain no control characters" go away; note them in the release notes, since scripts may match them.
  - src/reporting/run_contract.rs already has a valid_name with the same semantics and its own MAX_NAME_BYTES; reuse or merge it rather than adding a third predicate.
  - Tests that relied on long job names being accepted (the unit test job_names_reject_control_characters_only with 100 bytes, and tests/reporting.rs reporting_config_boundaries_are_accepted with an unreported 100-byte job name) need updating to the 64-byte boundary.
  - The README upgrade note says to rename long job names along with their depends_on, --jobs and profile partitions references, and to shorten long output keys along with the expressions that use them; the "Steps and outputs" section links to the rule's bullet.
  - The repo's own goodflows.toml must still validate.
---

## Why

goodflows checks names in five places, and they disagree. The config check for workflow, environment, step and output names trims, caps at 64 bytes (a hardcoded 64) and refuses control characters. Job names are checked for blanks and control characters, but the 64-byte cap applies only to jobs that declare `reports`. The check for names read back from spool manifests and snapshots does not trim, so a whitespace-only name passes. Output keys written to `$GOODFLOWS_OUTPUT` at runtime have no length cap, while declared output keys do.

The companion goodflows_web change `add-workflow-run-storage` adds `POST /api/v1/projects/:project/workflow-runs`, which accepts the run-history record (`schema_version` 1) and applies the name rule to every `results[*].name` and `requested_jobs` entry. One job name over 64 bytes makes the server reject the whole run with `422`, which goodci treats as pending and retries, so that workflow's runs never upload.

This is breaking for configs with job names over 64 bytes and steps that write output keys over 64 bytes. Valid configs are otherwise unaffected.

## Context

When the change was written (2026-11-07) none of it was built: the config check hardcodes 64, `valid_report_name` exists and is used by replay and collection, job names have no length cap unless they report, and runtime output keys have no length cap. Text-mode output already escapes control characters other than tab in job-provided text; that behavior had no durable spec, so this change adds it to `run-report` with job names now covered by the rule.

Only names over 64 bytes are newly refused in config, plus whitespace-only names in hand-edited spool files. A long runtime output key fails at runtime rather than at load, because keys are only known then; it fails through a path tests already cover.

## Approaches

The chosen approach is one predicate called from every name check, with the length from the reporting contract. The alternative, keeping separate predicates and aligning them, was rejected because separate predicates are how the rules drifted apart. Truncating or rewriting long names was not considered acceptable, because the names are identifiers that other config, CLI flags and expressions refer to.
