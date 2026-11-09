# Design

## Context

See proposal.md for why. Name checks today:

| Check | Where | Names | Rule |
|---|---|---|---|
| `validate_name(name, kind)` | `src/config.rs` | workflow, environment, step id, job `outputs` key | trimmed nonempty, `len() <= 64` (hardcoded), no `char::is_control` |
| inline checks in `validate_jobs` | `src/config.rs` | job | trimmed nonempty (`empty job name`), no control characters (checked after `validate_job`); no length limit |
| `valid_report_name` in `validate_reports` | `src/config.rs` | job with `reports` | **untrimmed** nonempty, `<= MAX_NAME_BYTES`, no control characters |
| `valid_report_name` | `reporting/replay.rs` (manifest), `reporting/collect.rs::validate_envelope` (snapshot) | workflow, environment, job | as above |
| inline check in `read_outputs` | `src/runner.rs` | runtime `$GOODFLOWS_OUTPUT` keys | trimmed nonempty, no control characters; no length limit |

Two existing tests rely on long job names being accepted: the unit test `job_names_reject_control_characters_only` (100 bytes) and `tests/reporting.rs::reporting_config_boundaries_are_accepted` (100 bytes, no reports).

## Goals / Non-Goals

**Goals:**
- One predicate holds the rule. Every check listed above calls it, and the length comes from `contract::MAX_NAME_BYTES`.
- Config errors keep the existing `validate_name` wording, and job, step and output errors still identify which name failed.

**Non-Goals:**
- Other string rules with different semantics stay as they are: `valid_project_slug`, environment variable names, git branch and commit checks, contract identifiers, partition labels and text fields (`MAX_TEXT_BYTES`).
- No change to `--jobs`, `depends_on` or profile `partitions` checks. They must match a configured name, so the rule applies to them indirectly.

## Decisions

### One predicate, one error helper
In `src/config.rs`:

```rust
/// The goodflows name rule. Every name check uses this.
pub fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= crate::reporting::contract::MAX_NAME_BYTES
        && !name.chars().any(char::is_control)
}

pub const NAME_RULE: &str = "must be nonempty, at most 64 UTF-8 bytes, and contain no control characters";

fn validate_name(name: &str, kind: &str) -> Result<()> {
    if !valid_name(name) { bail!("{kind} name {NAME_RULE}"); }
    Ok(())
}
```

`valid_report_name` is deleted. Its callers in replay and collect call `config::valid_name`, and the replay error messages stay as they are. A unit test asserts that `NAME_RULE` mentions `MAX_NAME_BYTES`, so the constant and the message stay in sync.

Alternative: keep two predicates and make them agree. Rejected because they would drift apart again, which is how this inconsistency arose.

### Job names
`validate_name(&job.name, "job").with_context(|| format!("job '{}'", job.name.escape_debug()))?` becomes the first check in the per-job loop of `validate_jobs`. It replaces `empty job name` and the trailing control-character check, and the duplicate check stays. Running it first means a bad name is reported before other errors on that job. `escape_debug` keeps the guarantee that errors never print raw control characters. Inside a named workflow the result reads `workflow 'ci': job '<name>': job name must be ...`.

The name check in `validate_reports` can no longer fail, so it is removed with its message.

### Runtime output keys
`read_outputs` rejects a key with `!config::valid_name(key)` and the error `output keys {NAME_RULE}`. The runner already wraps this as `invalid job outputs: ...`, marks the command failed, publishes no outputs and skips dependents, so no other runtime behavior changes. This matches the 64-byte cap that config already puts on declared `outputs` keys.

### Spec placement
The rule now covers five kinds of names across config, runtime and replay. It gets its own capability, `name-rule`, rather than being scattered across `run-report`, `test-timing-publishing` and the specs with no owner (steps and outputs). `run-report` keeps its terminal-safety requirement but refers to the rule instead of saying "other job-name rules are unchanged". `test-timing-publishing` already requires "the workflow, environment and job name rules" during replay and a 64-byte cap for reporting jobs. Both stay true, so it is not changed.

### Places that already assume valid job names
- **`depends_on`, `--jobs` / `Config::select`, profile `partitions` keys**: all resolve against configured job names (`unknown job`, `not partitioned`), so `requested_jobs` in history only ever holds valid names. No change.
- **`path_component`**: encodes job names only for the reporting spool, and the job names there were already at most 64 bytes. Log and history directories encode workflow names, which were already validated. With the cap, any encoded job component is at most 192 bytes. No change.
- **Log file stems** (`runner.rs`): already truncated to 60 sanitized characters. No change.
- **History `results[*].name`, report rows, snapshots**: carry configured names, so they now always pass the server's rule.

### Documentation
README "Configuration" gets one bullet stating the rule for workflow, environment, job, step and output names (including runtime output keys). It also gets a short upgrade note: rename long job names and their references, and shorten long output keys and their expressions. The "Steps and outputs" section links to that bullet. The release notes for the next version repeat the upgrade note (`docs/releasing.md` process).

## Risks / Trade-offs

- [Configs or steps that worked now fail] → Only names over 64 bytes are newly rejected, plus whitespace-only names in hand-edited spool files. The errors state the rule, and the README gives the fix.
- [Changed error strings (`empty job name`, `contains invalid control characters`, `reporting job name ...`, `output keys must be nonempty and contain no control characters`)] → These are human-facing messages, and the JSON event types are unchanged. Note them in the release notes.
- [A step failure at runtime instead of at load for long output keys] → Unavoidable, because keys are only known at runtime. The failure goes through the existing `invalid job outputs` path, which tests already cover.

## Migration Plan

Users rename job names and shorten output keys that are longer than 64 UTF-8 bytes, along with every reference to them. Existing history and spool files are untouched. A spool with a whitespace-only name, which only hand-editing could produce, is refused on replay.
