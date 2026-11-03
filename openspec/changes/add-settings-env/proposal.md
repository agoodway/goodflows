# Proposal

## Why

Goodflows requires shared command environment variables such as `MIX_ENV=test` to be repeated on every job. Settings-level environment defaults will make workflows self-contained while allowing workflows, machine profiles, jobs, and steps to override individual variables.

## What Changes

- Accept string-valued `env` maps in shared settings, named workflow settings, and named environment profile settings.
- Merge by key with precedence caller environment < shared settings < workflow settings < selected profile settings < job < step. Omitted and empty maps inherit; empty strings explicitly override.
- Use the merged environment consistently in commands, shell conditions, expression conditions, and job output expressions, preserving runner-owned variables and credential scrubbing.
- Validate every configured settings env map, including unselected workflows and profiles, using existing job/step env rules.
- Expose resolved configured settings env in JSON inspection without including the caller's environment.
- Generate Elixir configurations with one shared `MIX_ENV=test` declaration and document inheritance.
- Exclude variable removal, interpolation, a separate top-level `[env]`, and new CLI variable flags.

## Capabilities

### New Capabilities

- `settings-environment`: Settings-level command environment defaults, inheritance, validation, inspection, and generated configuration.

### Modified Capabilities

None. Existing partition identity and publisher credential requirements remain in force; the new capability applies their protections to the added configuration scopes.

## Impact

- `src/config.rs`: settings models, map merging, validation, effective settings serialization.
- `src/main.rs` and `src/runner.rs`: pass resolved defaults to execution without modifying the CLI process environment.
- `src/init.rs`: Elixir defaults and clean generated TOML for default and named workflows.
- `README.md`, `tests/workflows.rs`, `tests/runner.rs`, `tests/init.rs`, and relevant reporting/partition regression tests.
- No new dependencies or changes to scheduling, report wire formats, or credential sourcing. Existing configurations retain their execution behavior.
