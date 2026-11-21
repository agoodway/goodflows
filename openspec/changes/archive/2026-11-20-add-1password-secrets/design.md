# Design

## Context

See proposal.md for why. What exists today:

- `Settings.env` and `SettingsOverrides.env` are `BTreeMap<String, String>` merged by key: `[settings]`, then the workflow, then the selected profile (`SettingsOverrides::apply` in `src/config.rs`). Job and step `env` layer on top. Values are literal.
- `runner::job_env` builds the map that conditions and expressions read: the caller's environment, settings env and job env, with `ReportingEnv::scrub` removing credential names. `Execution::spawn` (`src/runner.rs`) sets the command's env from that map, removes the scrubbed names again because `.envs()` only adds to the inherited environment, then sets the reserved reporting values only for non-condition commands.
- Every byte of command output passes through `runner::drain`. It appends each chunk to the shared `Tails` (the failure head and tail that feed the summary, `last`, MCP results and report uploads), writes it to the log file when logs are on, and emits line-split `job_output` events. Nothing is masked.
- `read_outputs` reads a command's `GOODFLOWS_OUTPUT` file into job outputs, which reach events, expressions and the last-run record.
- `Config::workflow(name, profile)` returns a workflow's config with settings already merged. `Config::called_workflows` returns every workflow reachable through calls under the profile, which `Run.targets` holds.
- Credential names are validated with `valid_env_name` and rejected when they collide with reserved names (`validate_env`, `RESERVED_ENV`, `PARTITION_ENV`, `ENVIRONMENT_ENV`, `OUTPUT_ENV`, `SPRITE_STATE_ENV`, the reporting `token_env`).
- Sprite runs send forwarded `pass_env` values in a private frame after the header (`src/sprite/mod.rs`). The minted cache key travels in the same frame under `SPRITE_CACHE_ENV`. On the Sprite, `Worker::user_env` filters that key out of what hooks and the workflow process inherit, and the worker writes the key to the workflow process's stdin, which `exec_workflow` reads into `Run.cache_forwarded`. Controller credentials and forwarded values are added to `Sensitive` (the snapshot excludes source files that contain them, and the run continues) and to `sprite.secrets` (`transport::sanitize` replaces them in diagnostics).
- `run --detach` re-executes goodflows in a new session with stdin from `/dev/null` and inherits the caller's environment.
- Error codes are stable. Exit 1 covers runtime errors after the command and config were accepted.

## Goals / Non-Goals

**Goals:**
- Keep secret references in the config and secret values out of it.
- A job receives only the secrets it lists.
- Resolve each reference at most once per run, before any job starts, and never when the run needs none.
- Nothing goodflows prints or stores contains a resolved value.
- Adding a second provider later means adding a URI scheme, not changing configs.

**Non-Goals:**
- Other providers (`env://`, Vault, AWS Secrets Manager). The design leaves room for them.
- Resolving `[reporting]` or `[cache]` credentials through 1Password. Those stay caller environment variables; `op run -- goodflows` still works for them.
- Masking files that jobs write: `reports` JSON Lines files, cache archives, build artifacts. A job that writes a secret into a file owns that.
- Masking transformed values (base64, URL-encoded, one line of a multi-line value). Only the exact value is masked, as `op run` does.
- Caching resolved values between runs.

## Decisions

### Config shape

```toml
[settings.secrets]
DATABASE_URL = "op://dev/postgres/url"

[environments.ci.settings.secrets]
DATABASE_URL = "op://ci/postgres/url"

[[jobs]]
name = "integration"
command = "cargo test --test db"
secrets = ["DATABASE_URL"]
```

`secrets` is a `BTreeMap<String, String>` on `Settings` and `SettingsOverrides`, merged by key in `apply` exactly like `env`. `Job.secrets` is a `Vec<String>` of names. Steps inherit their job's secrets and have no `secrets` field. A call job rejects `secrets`, as it rejects `env`, because it runs no command.

Why a job opt-in instead of every job getting every secret: least privilege, and it is what makes resolution lazy. `goodflows --jobs fmt` must not open a 1Password approval prompt.

Why a profile cannot add a name: `validate` checks that every name a job lists is defined, without knowing which profile a run will select. If a profile could introduce names, a job could be valid under `--env ci` and invalid without it, and only a run would find out. Profiles exist to point the same names at a different vault.

### References and the provider seam

A reference is a URI whose scheme picks the provider. `secrets::Reference` is an enum with one variant today:

```rust
enum Reference {
    OnePassword(String), // the whole op:// reference, passed to `op read` as written
}
```

`Reference::parse` matches on the scheme. Adding a provider later is a new variant, a parse arm and a resolve arm. There is no trait or registry until a second provider exists.

An `op://` reference is accepted when it is at most 1024 bytes, has no whitespace or control characters, and its path after `op://` has three or four nonempty `/`-separated segments (`vault/item/field` or `vault/item/section/field`), optionally followed by a `?` query such as `?attribute=otp`. goodflows does not check further: `op` owns the full syntax and reports its own errors at resolution.

Any other value is `config_invalid`. The message says which schemes are supported, so a typo such as `0p://` or a pasted value fails in `validate` instead of reaching a job as a literal.

### Names and collisions

A secret name must match `[A-Za-z_][A-Za-z0-9_]*` and must not be a reserved goodflows name, the reporting `url_env` or `token_env`, or the `[cache] token_env`. These rejections are reported with the existing `validate_env` messages.

- The same name in `env` and `secrets` of one settings table is `config_invalid`. That conflict has no reading where both are intended.
- A job's or step's `env` must not set a name the job lists in `secrets`.
- Across layers, a secret wins: for a job that lists `DATABASE_URL`, its commands get the secret even when the caller or a settings `env` also sets `DATABASE_URL`.
- A job that lists a name defined nowhere in `[settings.secrets]` or its workflow's `settings.secrets` is `config_invalid`, and so is a profile that defines a name neither of them defines.
- Listing a name twice in one job's `secrets` is `config_invalid`.

### What gets resolved, and when

After the root workflow's selection (`--jobs`, `--failed` and their dependencies) and `Run.targets` are known, and before the root invocation starts, goodflows collects every `(name, reference)` pair listed by a selected root job or by any job in a called workflow, under each workflow's merged secrets. Jobs that a condition later skips are included, since conditions are only known while running. References are deduplicated, so two workflows that point different names at one reference cause one read, and two workflows that point one name at different references cause two.

The result is `Run.secrets: Arc<Secrets>`, holding the values keyed by reference and the masker built from all values. Every invocation, root and child, reads it. A run whose selection lists no secrets builds an empty `Secrets` and never looks for `op`.

`validate` and `list` parse and check references but never resolve them.

### Running `op`

goodflows finds `op` on `PATH`. When it is missing, the run fails with `secret_unavailable` and a message pointing to the 1Password CLI install page.

Each read is `op read --no-newline <reference>`, with stdin from `/dev/null`, the caller's environment (so `OP_SERVICE_ACCOUNT_TOKEN`, `OP_ACCOUNT` and `OP_CONNECT_*` work), and the run's working directory.

- The first read runs alone, with a 120-second timeout that leaves time for a desktop approval prompt. The rest then run at most 8 at a time, each with a 30-second timeout. Without the first read alone, parallel reads before the app has approved this session can each open a prompt.
- Exit 0 with stdout that is valid UTF-8 without NUL is the value. Values are kept as `String` and never logged.
- A nonzero exit, a timeout or an invalid value fails the run before any job starts with `secret_unavailable`, exit 1. The message is `secret NAME (REFERENCE) could not be read: ` followed by the first line of `op`'s stderr, at most 512 bytes, passed through the masker built so far. The `error` event carries `secret` with the name. When several reads fail, the first in config order is reported.
- Cancelling the run (SIGINT or SIGTERM) while reads are running kills them and exits with the usual interrupt code.

Why one `op read` per reference instead of one `op inject`: `op read` returns the value byte for byte, so a PEM key or a password with quotes needs no escaping. A single `op inject` template would need an escaping scheme that every value survives. Separate reads also tie each failure to its reference without parsing `op`'s error text.

A value shorter than 4 bytes cannot be masked without corrupting unrelated output (the threshold `transport::sanitize` uses). goodflows still delivers it and prints one warning naming the secret, not the value.

### Delivering values

`Execution::spawn` sets the job's listed secrets on the command after the reporting removals and before the partition and profile variables, for the job's command and each step, but not for condition commands. `job_env`, and therefore conditions, cache keys and output expressions, never contains a secret value. An expression that reads `env.DATABASE_URL` sees whatever the caller's environment has, exactly as today.

### Masking

`secrets::Masker` holds the values of at least 4 bytes, sorted longest first so a value that contains another is replaced whole. Each `drain` call gets its own stream state:

- `feed(chunk)` appends to a pending buffer, replaces every complete occurrence with `[redacted]`, and returns everything except the longest suffix that is a proper prefix of some value. That suffix waits for the next chunk.
- `finish()` returns what is left, after a final replacement, at end of stream.

`drain` passes each chunk through `feed` before it touches `Tails`, the log file or the event line splitter, and passes `finish()` through the same three sinks before its existing flush. A value split across two `read` calls, or spanning lines, is still masked. Output waits only while its tail could be the start of a secret, so streaming output without newlines is not delayed otherwise. With no secrets, `feed` returns the chunk unchanged.

`read_outputs` parses each value as raw JSON first. It masks every key and every string, and replaces a number with the string `[redacted]` when either the text the job wrote or serde's rendering of it contains a value: a 23-digit number parses as a float that renders differently (`1.2345678901234568e22`), and `1234.50` renders as `1234.5`. Two keys that mask to the same text fail the job instead of one silently replacing the other. When the file is not a JSON object, the error gives only the line and column: serde's type error quotes the rejected input in its parsed form (`1234.50` as `1234.5`), which masking cannot match. A value that fails to parse names its masked key. Every error a bad output file produces is also masked before it reaches `result.error`, so a published output cannot carry a value into events, expressions, the summary or the last-run record.

A local run and a Sprite controller that need any secret call `reporting::protect_process()` before the first `op read`, so no other process of the same user can read the values from goodflows' memory or `/proc/<pid>/environ` while they are held.

Whether the 1Password desktop app still identifies the caller while goodflows is not dumpable is unverified: it may inspect `op`'s parent through `/proc/<pid>/exe`, which a non-dumpable process refuses. Task 8.1 checks that the approval prompt names goodflows or the terminal correctly. If it fails or shows an unknown app, the fallback is to call `protect_process()` after `resolve` instead of before, which leaves the values unprotected only while the reads run.

Why mask every resolved value in every job, not only the job's own: a job can receive a secret indirectly, for example from a file an earlier job wrote. Masking all values costs nothing extra.

### Sprite runs

The controller resolves the run's secrets after `check_remote_env` and before the snapshot, with the same selection rules applied to the request. Their values go into `Sensitive`, so a source file that contains one is excluded from the snapshot, as forwarded values are, and into `sprite.secrets`, so `transport::sanitize` masks them in diagnostics.

The values travel in the private environment frame under a new reserved key, `GOODFLOWS_SPRITE_SECRETS`, as JSON mapping references to values. `pass_env` cannot name it. On the Sprite:

- `Worker::user_env` filters it out with `SPRITE_CACHE_ENV`, so setup, resume and the workflow process's environment never contain it.
- The worker writes `{"cache": …, "secrets": {…}}` to the workflow process's stdin instead of the bare cache key. `exec_workflow` reads both and sets `Run.secrets` from the map, so the remote run never looks for `op`.
- A reference the remote run needs that is missing from the map is a runtime error. It only happens when controller and runner versions differ.

### Detached runs

The detached child resolves secrets itself, with the caller's environment. A failure ends the detached run with the `secret_unavailable` `error` event, which `status` and `wait` report. In unattended use, a service account token is the reliable way to authenticate; whether the desktop app's approval prompt works from a detached session is an open question below.

### `list` and MCP

`list` prints a `secrets` column with each job's names, and its JSON carries `secrets` as a list of names. References are not secret, but `list` shows names only, because names are what a job receives. The MCP `list` tool returns the same field.

## Risks / Trade-offs

- [A value printed in a form other than the exact bytes, such as base64 or one line of a PEM key, is not masked] → Same limit as `op run`. Documented next to the masking guarantee.
- [A value shorter than 4 bytes is not masked] → A warning at resolution names the secret. Such values are rare as real credentials.
- [One read per reference costs one process and one 1Password call each] → Runs need few secrets, references are deduplicated, and reads after the first run in parallel. Service accounts have hourly read limits; a run that needs dozens of secrets will feel it.
- [Every run that selects a secret-using job reads 1Password, even when a condition then skips the job] → Conditions are only known while running. `--jobs` narrows the selection.
- [A desktop approval prompt in an unattended or detached run] → The first read alone has a 120-second timeout and then fails with `secret_unavailable`. CI and detached runs should use a service account.
- [Masking holds back a partial match] → Only the bytes that could start a secret wait, and only until the next chunk or end of stream.

## Migration Plan

1. Land this change. Configs without `secrets` behave exactly as before, and goodflows never looks for `op` for them.
2. Projects move `op://` values from wrapper scripts or `.env.op` files into `[settings.secrets]` and add `secrets = [...]` to the jobs that need them.

## Open Questions

- Should a caller variable with a secret's name skip the read, so CI that already injects `DATABASE_URL` from its own secret store needs no 1Password token? It would make a stale shell variable silently win over the vault locally. This change says no; a later change could add an explicit opt-in.
- Does the 1Password desktop app's approval prompt work for `op` started by a detached run in its own session? If not, detached runs need a service account, and the docs should say so.
