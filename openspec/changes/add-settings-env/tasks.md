# Tasks

## 1. Settings models, inheritance, and validation

- [ ] 1.1 Add default-empty string env maps to `Settings`, `SettingsOverrides`, and `EffectiveSettings`, include env in override emptiness checks, and implement per-key merging and resolution; verify config unit tests cover absent/empty maps, empty strings, literal values, non-string rejection, and preservation of unrelated keys.
- [ ] 1.2 Validate shared and every workflow/profile env map before selection using existing env validation and scope context; verify tests reject invalid names/values, reserved reporting/partition variables, `GOODFLOWS_ENVIRONMENT`, configured reporting token names, unselected invalid profiles/workflows, and invalid entries hidden by overrides without exposing values in semantic env validation errors. Preserve existing TOML syntax/type diagnostics, which can include source excerpts.
- [ ] 1.3 Add workflow integration coverage for shared < workflow < selected profile inheritance, legacy top-level jobs, absent profile selection, sibling isolation, and env-only profile serialization; verify `cargo test --locked --test workflows` passes.
- [ ] 1.4 Update README configuration and environment-profile sections with settings env syntax, per-key precedence, empty-map/empty-string behavior, and literal values; verify examples parse and no statement still says profiles cannot override command environment variables.

## 2. Execution and inspection

- [ ] 2.1 Pass resolved settings env from `src/main.rs` through runner options and merge caller < settings < job before existing scrubbing and condition evaluation; update construction sites and verify runner integration tests cover caller conflicts, shell and direct commands, job/step precedence, subsequent-step isolation, and unchanged behavior without settings env.
- [ ] 2.2 Add behavioral coverage for settings/profile values in job and step shell/expression conditions and job output expressions, including exclusion of step-only overrides from job outputs; verify the relevant runner/output integration tests pass.
- [ ] 2.3 Preserve reporting credential scrubbing and runner-owned partition/profile/output variables with settings env present; verify focused reporting and partition tests cover inherited-token invisibility, partitioned identity, unpartitioned identity removal, selected-profile `GOODFLOWS_ENVIRONMENT` injection and its removal without profile selection in commands and expressions, and `GOODFLOWS_OUTPUT` behavior. Also add reporting endpoint-source isolation regressions: when settings override the configured `url_env` variable, commands receive the settings value while the publisher still uses the caller's endpoint; when the caller endpoint is absent, settings must not supply it to the publisher.
- [ ] 2.4 Expose merged configured defaults through `EffectiveSettings` JSON without flattening job env or including caller env; verify selected list/validate, workflow discovery, aggregate validation, and no-env `{}` cases in `tests/workflows.rs`.
- [ ] 2.5 Document JSON `settings.env` and condition inheritance in README; verify documented fields match integration test output and human summaries retain their existing behavior.

## 3. Generated Elixir configuration

- [ ] 3.1 Move generated `MIX_ENV=test` from every Elixir job into shared settings, preserving clean TOML omission of empty maps; update `tests/init.rs` and verify default and named workflow generation contain exactly one declaration, no job copies, and validate successfully.
- [ ] 3.2 Verify generated Elixir jobs inherit the default using a stub `mix` executable, including named workflow generation, and verify other templates omit empty settings env maps; run `cargo test --locked --test init` and update README initializer guidance to match.

## 4. Integration verification

- [ ] 4.1 Run `cargo fmt --check`, `cargo clippy --locked --all-targets -- -D warnings`, and `cargo test --locked --all`; verify all pass and inspect the diff for unintended changes to existing scheduling, reporting, and output contracts.
- [ ] 4.2 Run `openspec validate add-settings-env --strict` and review implementation against every settings-environment scenario; verify all scenarios have passing behavioral coverage or directly observable confirmation and record any remaining limitations before declaring implementation complete.
