# Design

## Context

See proposal.md for motivation. `Settings` holds shared defaults, `SettingsOverrides` is reused by workflows and profiles, and `Config::workflow` applies workflow settings followed by selected profile settings. `EffectiveSettings` backs inspection JSON. None currently accepts env.

The runner builds an environment from the caller and `Job.env`, scrubs reporting credentials, applies partition identity, evaluates the job condition, and adds step overrides for individual steps. Commands and expressions use those maps. `src/init.rs` currently inserts `MIX_ENV=test` into each Elixir job. README explicitly states that profiles cannot change job environment variables; that statement must change.

The existing partition and test timing specs define authoritative runner variables and publisher credential isolation. This design preserves those contracts and extends existing env validation to settings rather than changing those capabilities.

## Goals / Non-Goals

**Goals:** Reuse settings resolution for configured env inheritance; keep one consistent runtime merge path for commands and expressions; preserve explicit job env configuration in inspection; validate source scopes before merging.

**Non-Goals:** Mutating the CLI process environment, changing publisher credential sourcing, changing scheduling, or expanding the configuration surface beyond the scope in proposal.md.

## Decisions

### Add maps to the existing settings models

Add a default-empty string map to `Settings`, `SettingsOverrides`, and `EffectiveSettings`. Use the existing env map representation for consistency. Include env in `SettingsOverrides::is_empty`, and omit empty env maps when serializing configuration TOML; effective inspection JSON always contains an object.

`SettingsOverrides::apply` clones the lower-precedence map and extends it with override entries. This handles workflow and profile merging through their existing application order. Empty maps do nothing; an empty value remains a real override.

Alternatives considered: a separate root `[env]` with workflow/profile env fields would duplicate inheritance machinery; caller exports retain the repetition problem's dependence on external setup; replacing entire maps would discard unrelated inherited keys. The selected settings approach fits the user-approved precedence.

### Pass configured defaults to execution explicitly

Carry resolved settings env through runner options from `src/main.rs`, and merge it between caller env and job env in the runner's existing environment assembly. Keep the resolved defaults separate from `Job.env` so JSON continues to show authored job overrides and shared defaults separately. Avoid flattening defaults into every job or using process-wide environment mutation, which risks cross-workflow leakage and publisher side effects.

The existing step merge, condition evaluation, and job output expression evaluation then inherit the same values naturally. Preserve reporting scrubbing before expression evaluation and command spawn, partition identity authority, selected-profile `GOODFLOWS_ENVIRONMENT` injection (and removal when no profile is selected), and command-owned `GOODFLOWS_OUTPUT` handling. Settings values do not become publisher endpoint or token sources.

### Validate each authored settings map before workflow selection

Extend `Config::validate` to validate shared settings and every workflow/profile settings map with existing `validate_env`, using the global reporting config. This includes rejecting `GOODFLOWS_ENVIRONMENT` in every settings scope, alongside the existing reserved reporting and partition names. Add scope context to semantic env validation errors without including values. This value-free guarantee applies after TOML deserialization; retain existing TOML syntax/type diagnostics, which can include source excerpts and configured values. Validate originals, not just effective merged maps: otherwise an overridden invalid entry or unselected profile could escape checks. Preserve current permissive key rules rather than tightening existing job/step grammar as incidental work.

### Extend effective settings inspection

Have `Settings::resolve` carry the merged configured map into `EffectiveSettings`. Existing list, workflow discovery, and validation JSON paths already serialize that type. Do not enumerate caller env or flatten job/step values into it. Human output retains its existing summary; README explains where JSON exposes defaults. No event version bump is planned for this additive settings field.

### Generate the Elixir default in shared settings

Set `MIX_ENV=test` on generated shared settings when the selected template is Elixir and remove per-job insertion. The named-workflow generator already moves jobs into the named workflow while retaining shared settings, so both paths inherit the same default. Preserve omission of empty maps for other templates and update the initializer regression that currently expects a job-level declaration.

## Risks / Trade-offs

- [Different values in expressions and child processes] -> Merge settings before the existing scrub/condition path and test both expression and shell conditions plus direct commands.
- [Defaults accidentally exposed as authored job overrides] -> Keep configured defaults in effective settings and runner options, leaving `Job.env` intact.
- [Invalid values hidden by selection or overriding] -> Validate every source settings map before selection and add regressions for unselected profiles and overridden entries.
- [Credential or reserved-variable handling bypassed] -> Reuse validation and existing runtime scrub/injection paths; exercise reporting and partition integration tests with settings defaults present.
- [Configured values are visible in inspection] -> Match existing explicit job env inspection behavior; never add the caller environment, and retain reporting-token rejection before serialization.
- [Struct additions affect existing construction sites] -> Update runner options/settings literals in production code and tests and compile all targets.

## Migration Plan

Existing TOML remains valid without edits. New initializer output uses settings env immediately; users may migrate repeated job values into shared settings manually. README documents profile env precedence and literal/empty-value behavior. No persistent data or report wire migration is needed. Rolling back the binary requires removing settings env from configurations created or edited for this feature; older binaries reject the new field. No automatic rewriting of existing files is included.
