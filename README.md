# goodci

A small, config-driven CI runner for local development, agent environments, and
Ampcode orbs. Define shell commands in TOML; goodci runs ready jobs in parallel,
respects dependencies, and reports exactly what passed, failed, or was skipped.

## Install

Build from this repository with Rust **1.88 or newer**:

```bash
cargo install --path . --locked
```

Or install from Git:

```bash
cargo install --git https://github.com/agoodway/goodci.git --locked
```

Supported platforms are Linux and macOS. Jobs require `sh` unless `shell = false`.
Linux release binaries use glibc and are built on Ubuntu 24.04; build from source
for older distributions or musl-based systems. Install each project's own tools
and dependencies before running its checks.

Tagged releases produce archives for Linux and macOS on x86_64 and aarch64.
After a release is published, download the matching archive and its checksum from
[GitHub Releases](https://github.com/agoodway/goodci/releases), then:

```bash
shasum -a 256 -c goodci-v<VERSION>-<TARGET>.tar.gz.sha256
tar -xzf goodci-v<VERSION>-<TARGET>.tar.gz
install -m 755 goodci ~/.local/bin/goodci
```

Create `~/.local/bin` first if needed and include it in your `PATH`.

## Quick start

Run from your project's root:

```bash
goodci init          # Detect the project and generate goodci.toml
goodci validate      # Check the config without executing commands
goodci list          # Show jobs and dependencies
goodci               # Run all checks; equivalent to goodci run
goodci --jobs test   # Run test and its transitive prerequisites
```

`init` detects `Cargo.toml`, `mix.exs`, `package.json`, or `go.mod`. Multiple
project types require `--template`; an unrecognized project gets a generic example
whose placeholder command you must edit. You can always choose explicitly:

```bash
goodci init --template elixir
goodci init --template rust
goodci init --template node
goodci init --template go
goodci init --template generic
goodci init --config ci/checks.toml
goodci init --force                     # Replace an existing config without asking
```

Without `--force`, an existing file triggers an `Overwrite? [y/N]` prompt. Only `y`
or `yes` (case insensitive) replaces it. Other responses, Enter, and end-of-input
keep it. A newly generated nested config points `workdir` back to the project root.

Node initialization uses the package manager indicated by `package-lock.json`,
`npm-shrinkwrap.json`, `pnpm-lock.yaml`, `yarn.lock`, or `bun.lock`/`bun.lockb`
(default: npm). Conflicting package-manager lockfiles are rejected. Only declared
`format:check`, `lint`, `typecheck`, `check`, `test`, and `build` scripts become jobs;
no check scripts produces an error. The Node template requires `package.json`.
Elixir jobs use `MIX_ENV=test`.

## Configuration

```toml
[settings]
parallelism = 4       # Maximum running jobs; 0 or omitted = logical CPU count
fail_fast = true      # Default, including when [settings] is omitted
workdir = "."        # Relative to the config file's directory; absolute paths work

[[jobs]]
name = "fmt"
command = "cargo fmt --check"

[[jobs]]
name = "test"
command = "cargo test --locked --all"
depends_on = ["fmt"]
timeout_secs = 300
env = { RUST_BACKTRACE = "1" }

[[jobs]]
name = "lint"
command = "cargo clippy --locked --all-targets -- -D warnings"
depends_on = ["fmt"]
```

Commands use `sh -c` by default, so pipes, redirects, and shell expansion work.
With `shell = false`, commands are split using shell-style quoting and executed
directly; quoted arguments work, but variables, pipes, and globbing are not expanded.
Jobs inherit the runner's environment plus their own `env` entries. Job stdin is
closed, so checks must run noninteractively.

Without `--config`, goodci looks in the current directory for `goodci.toml`, then
`.goodci.toml`. Commands default to that file's directory. A configured relative
`workdir` is also resolved from that directory, regardless of where you invoke
goodci. `--log-dir` is resolved from the invocation directory.

The whole config is validated before any command runs—even when selecting a subset.
Unknown fields, duplicate/empty names, empty commands, missing or duplicate
dependencies, self-dependencies, cycles, invalid environment entries, malformed
direct-command quoting, zero timeouts, and nonexistent workdirs are errors.

## Scheduling and failures

Jobs become ready as soon as **all** prerequisites pass. Unrelated work can run
concurrently up to `parallelism`; there are no batch barriers. Ready jobs are
launched in config order. `--jobs` automatically includes every prerequisite and
rejects unknown names; the same selection works with `list` and `validate`.

```bash
goodci --parallelism 2
goodci --no-fail-fast
goodci --jobs fmt,test
goodci --config ci/checks.toml
```

Fail-fast stops launching jobs after the first observed failure. Already-running
jobs finish. `--no-fail-fast` continues unrelated jobs, but a failed prerequisite
always skips its dependents, transitively. `--fail-fast` and `--no-fail-fast`
override the config and cannot be used together.

Results distinguish `passed`, `failed`, `skipped`, `timed_out`, and `cancelled`.
Skipped jobs do not inflate the failure count. Failed commands retain their exit
codes. Timeout, Ctrl+C, and SIGTERM terminate each job's Unix process group and
reap its direct child; descendants that remain in that group are terminated too.
Jobs must not detach themselves into new sessions or process groups. Descendants
still running when their job's direct child exits are also terminated, preventing
background processes from keeping output pipes open. Signal interruption stops
queued work and emits a final summary.

| Exit code | Meaning |
|---|---|
| `0` | All selected jobs passed; or successful init/list/validate (including declined overwrite) |
| `1` | A job failed/timed out, or config/runtime error |
| `2` | Invalid command-line arguments |
| `130` | Interrupted by Ctrl+C / SIGINT |
| `143` | Terminated by SIGTERM |

## Logs and machine output

Human output prefixes each stdout/stderr line with its job name and stream:

```text
[test stdout] running 12 tests
[lint stderr] warning: ...
```

Optionally save raw stdout and stderr separately:

```bash
goodci --log-dir .goodci-logs
```

Files use indexed, sanitized job names such as `001-fmt.stdout.log` and
`001-fmt.stderr.log`. Indexes follow the selected jobs' config order; files from
the same selection are overwritten on subsequent runs. Use a fresh directory for
each run to keep historical logs. Add your log directory to your project's
`.gitignore` if desired.

For agents and CI integrations:

```bash
goodci --json > results.jsonl
goodci validate --json
goodci list --jobs test --json
```

`--json` produces JSON Lines only on stdout. Each event includes `schema_version: 1`
and `type`. Types are `run_started`, `job_started`, `job_output`, `job_finished`,
`summary`, `config_valid`, `jobs`, `config_written`, `init_cancelled`, and `error`.
An interactive init confirmation goes to stderr in JSON mode. Help/version remain
human-readable even with `--json`. Argument errors produce an `error` event.

```json
{"schema_version":1,"type":"job_output","name":"test","stream":"stdout","data":"ok\n"}
```

The final `summary` contains `success`, counts for every status, and `results` in
config order. Each result contains `name`, `status`, `duration_ms`, nullable
`exit_code`, and nullable `error`. Configuration/runtime failures produce an
`error` event instead of a completed summary.

Output is streamed with bounded buffers; long lines can span multiple events.
`job_output.data` preserves newlines where present. Decode events in order and
concatenate data per job/stream to reconstruct text. Non-UTF-8 bytes use replacement
characters; saved log files retain the original bytes. Output order between
concurrent jobs or separate stdout/stderr streams is not guaranteed. Logs can
contain secrets emitted by your commands; goodci does not redact them.

Output backpressure never blocks timeout or signal handling. On interruption, if
the downstream consumer stops reading, goodci allows 250 ms for pending output
before discarding further events so it can exit. A final summary cannot be
guaranteed when the output pipe is blocked or closed.

## Development and releases

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
./target/release/goodci
```

`Cargo.lock` is committed for reproducible dependency resolution. CI runs formatting,
Clippy, tests, a release build, and goodci's own checks on Linux/macOS with Rust 1.88
and stable. Release builds run checks on all four native platform/architecture
combinations before packaging. Workflows use GitHub's documented
[runner labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

The Release workflow can be dispatched manually to build artifacts without publishing.
Pushing a `v<version>` tag matching `Cargo.toml` builds and publishes archives,
individual SHA-256 checksums, and `SHA256SUMS`. Maintainers should bump the package
version, update the lockfile, and review CI before pushing a release tag. No release
is published merely by running `goodci` or pushing a regular branch.

To exercise packaging locally for your host target:

```bash
bash scripts/package-release.sh x86_64-unknown-linux-gnu v0.1.0
```
