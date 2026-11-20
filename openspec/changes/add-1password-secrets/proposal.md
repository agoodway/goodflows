# Proposal

## Why

A job that needs a credential, such as a database URL or an npm token, gets it from the caller's environment today. `env` values in the config are literal strings, so a secret either sits in plain text in `goodflows.toml` or every caller has to export it first. Teams that keep credentials in 1Password wrap goodflows by hand:

- `op run --env-file=.env.op -- goodflows` resolves `op://` references before goodflows starts. Every job then inherits every secret, and `op` masks only what goodflows prints to its own terminal. The log files, the `last` record, `job_output` events and the output tails sent with reports keep the plain value.
- `command = "op run -- cargo test"` with an `op://` value in the job's `env` masks that job's output, but each job signs in to 1Password on its own, and the wrapper is repeated in every command.

goodflows masks nothing in local job output. Only Sprite runs scrub known values from their diagnostics (`sprite::transport::sanitize`).

## What Changes

- `[settings.secrets]` maps an environment variable name to a secret reference, such as `DATABASE_URL = "op://ci/postgres/url"`. Workflows (`[workflows.NAME.settings.secrets]`) and profiles (`[environments.NAME.settings.secrets]`) override it by key, the same way `env` merges. A profile can only override names that `[settings]` or the workflow already define.
- A job lists the secrets it needs with `secrets = ["DATABASE_URL"]`. Only that job's commands and steps receive them. Other jobs, conditions and expressions never do.
- A reference is a URI, and its scheme picks the provider. Only `op://` is supported: goodflows calls the 1Password CLI. Any other scheme is a `config_invalid` error. A later provider is a new scheme, with no change to configs written now.
- Before the first job starts, goodflows resolves each distinct reference that a selected job, or a job in a workflow it calls, needs. It runs one `op read --no-newline <ref>` per reference: the first alone, so the 1Password app asks for approval once, then the rest in parallel. A run that needs no secrets never calls `op`.
- A missing `op` binary, a failed read, or a value that is not valid UTF-8 or contains NUL stops the run before any job starts, with a new `secret_unavailable` error code (exit 1). The message names the variable and the reference, never a value.
- goodflows masks every resolved value as `[redacted]` in every job's stdout and stderr before it reaches the terminal, `job_output` events, log files, failure tails (the summary, `last`, MCP results and report uploads) and published job outputs.
- `goodflows validate` and `list` check secrets without calling `op`. `list` shows each job's secret names.
- Sprite runs resolve secrets on the controller and send them to the remote workflow process the same private way the minted cache key travels. The Sprite never runs `op`, and its setup and resume hooks never see secret values.

## Capabilities

### New Capabilities

- `secrets`: declaring secret references in settings, requesting them per job, resolving them through the 1Password CLI, delivering them to job commands only, masking their values in everything goodflows prints or stores, and forwarding them to Sprite runs.

## Impact

- Code: a new `src/secrets.rs` (references, resolution, the masker), `src/config.rs` (`secrets` in `Settings`, `SettingsOverrides` and `Job`, validation), `src/main.rs` (resolution before the root invocation, `Run.secrets`, the `secret_unavailable` code), `src/runner.rs` (command env, masking in `drain` and `read_outputs`), `src/output.rs` (`list`), `src/mcp.rs` (`list`), `src/sprite/mod.rs` and `src/sprite/agent.rs` (controller resolution and the private frame).
- Tests: a fake `op` script on `PATH`, like `tests/fixtures/fake-sprite/sprite`. No test needs 1Password.
- Users: nothing changes unless a config adds `secrets`. Callers need the 1Password CLI signed in, through the desktop app or `OP_SERVICE_ACCOUNT_TOKEN` in CI.
- Sprite: the worker's stdin to the workflow process changes shape (cache key plus secrets). Controller and runner are the same version, as for every protocol change; a development build that falls back to the published runner of the same version cannot use secrets, as docs/sprites.md already warns.
- Docs: README (a "Secrets" section, the error code table), docs/sprites.md, and `src/agents_guide.md`.
