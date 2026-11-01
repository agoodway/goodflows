# goodflows

A small, config-driven CI runner for local development, agent environments, and
Ampcode orbs. Define shell commands in TOML; goodflows runs ready jobs in parallel,
respects dependencies, and reports exactly what passed, failed, or was skipped.

## Install

Build from this repository with Rust **1.88 or newer**:

```bash
cargo install --path . --locked
```

Or install from Git:

```bash
cargo install --git https://github.com/agoodway/goodflows.git --locked
```

Supported platforms are Linux and macOS. Jobs require `sh` unless `shell = false`.
Linux release binaries are glibc builds linked on Debian 12 (glibc 2.36), so
they run on Ampcode orbs and on systems with glibc 2.36 or newer. Build from
source for older glibc or for musl-based systems. See
[Ampcode orbs](#ampcode-orbs) to install a published archive in an orb. Install
each project's own tools and dependencies before running its checks.

Tagged releases produce archives for Linux and macOS on x86_64 and aarch64.
After a release is published, download the matching archive and its checksum from
[GitHub Releases](https://github.com/agoodway/goodflows/releases), then:

```bash
shasum -a 256 -c goodflows-v<VERSION>-<TARGET>.tar.gz.sha256
tar -xzf goodflows-v<VERSION>-<TARGET>.tar.gz
install -m 755 goodflows ~/.local/bin/goodflows
```

Create `~/.local/bin` first if needed and include it in your `PATH`.

## Quick start

Run from your project's root:

```bash
goodflows init          # Detect the project and generate goodflows.toml
goodflows validate      # Check the config without executing commands
goodflows list          # Show jobs and dependencies
goodflows               # Run all checks; equivalent to goodflows run
goodflows --jobs test   # Run test and its transitive prerequisites
```

`init` detects `Cargo.toml`, `mix.exs`, `package.json`, or `go.mod`. Multiple
project types require `--template`; an unrecognized project gets a generic example
whose placeholder prints a replacement instruction to stderr and exits 1. Init
itself succeeds, but the checks cannot pass until you replace that command. You
can always choose explicitly:

```bash
goodflows init --template elixir
goodflows init --template rust
goodflows init --template node
goodflows init --template go
goodflows init --template generic
goodflows init --config ci/checks.toml
goodflows init --template rust --workflow ci  # Generate a named workflow starter
goodflows init --force                     # Replace an existing config without asking
```

Without `--force`, an existing file triggers an `Overwrite? [y/N]` prompt when
stdin is a terminal. Only `y` or `yes` (case insensitive) replaces it. Other
responses, Enter, and empty end-of-input keep it.

When stdin is not a terminal, init never prompts or waits for more input. It
accepts an immediately available complete `y`/`yes` line, or `y`/`yes` followed by
end-of-input. An open pipe with no data or an incomplete line cancels immediately,
as does input exceeding the 4 KiB response limit. Cancellation exits 0 and emits
`init_cancelled` in JSON mode. Piped confirmation is timing-dependent: use
`--force` for reliable automation. A newly generated nested config points `workdir`
back to the project root.

Generated TOML omits empty dependency/environment entries, `shell = true`, and
unset timeouts. Its comments explain that omitted parallelism uses every logical
CPU and recommend setting it to the machine size before running checks.

Node initialization uses the package manager indicated by `package-lock.json`,
`npm-shrinkwrap.json`, `pnpm-lock.yaml`, `yarn.lock`, or `bun.lock`/`bun.lockb`
(default: npm). Conflicting package-manager lockfiles are rejected. Only declared
`format:check`, `lint`, `typecheck`, `check`, `test`, and `build` scripts become jobs;
no check scripts produces an error. The Node template requires `package.json`.
Elixir jobs use `MIX_ENV=test`.

## Ampcode orbs

Commit `goodflows.toml`, install the binary from an executable `.agents/setup`, and
name `goodflows` in `AGENTS.md`. Ampcode runs `.agents/setup` from the repository
root while preparing an orb, then saves that disk as a project snapshot. Later
threads for the same project and orb size restore the snapshot, so `goodflows` is
already on `PATH`.

Create an Ampcode project for the repository. Generate the config and commit it
at the repository root:

```bash
goodflows init
goodflows validate
```

Ampcode orbs run Debian 12 with Git, Node.js, Python, `gh`, and `amp` already
installed. Install Rust, Go, or Elixir in `.agents/setup` when jobs call them.
Install goodflows from a published release archive into `/usr/local/bin`. Jobs
inherit the runner's environment, so copy the commands they call onto that
same default `PATH`.

This `.agents/setup` covers a Rust project. It is safe to run again, and it
expects a published `v*` release:

```bash
#!/usr/bin/env bash
set -euo pipefail

if ! command -v goodflows >/dev/null 2>&1; then
  case "$(uname -m)" in
    x86_64) target=x86_64-unknown-linux-gnu ;;
    aarch64 | arm64) target=aarch64-unknown-linux-gnu ;;
    *) echo "unsupported architecture: $(uname -m)" >&2; exit 1 ;;
  esac
  release_url=$(curl -fsSL -o /dev/null -w '%{url_effective}' \
    https://github.com/agoodway/goodflows/releases/latest) || {
    echo "no published goodflows release; push a v<version> tag first" >&2
    exit 1
  }
  tag=${release_url##*/}
  case "$tag" in
    v*) ;;
    *) echo "unexpected release URL: $release_url" >&2; exit 1 ;;
  esac
  archive="goodflows-${tag}-${target}.tar.gz"
  base="https://github.com/agoodway/goodflows/releases/download/${tag}"
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL "$base/$archive" -o "$tmp/$archive"
  curl -fsSL "$base/$archive.sha256" -o "$tmp/$archive.sha256"
  (cd "$tmp" && sha256sum --check "$archive.sha256")
  tar -xzf "$tmp/$archive" -C "$tmp"
  sudo install -m 755 "$tmp/goodflows" /usr/local/bin/goodflows
  rm -rf "$tmp"
  trap - EXIT
fi

if ! command -v rustup >/dev/null 2>&1 && [ ! -x "$HOME/.cargo/bin/rustup" ]; then
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain stable
fi
. "$HOME/.cargo/env"
rustup component add rustfmt clippy
for tool in cargo rustc rustfmt cargo-clippy; do
  sudo ln -sfn "$HOME/.cargo/bin/$tool" "/usr/local/bin/$tool"
done
```

```bash
chmod +x .agents/setup
git add .agents/setup goodflows.toml
```

Put any other job tools in that script. Node and Python jobs can use the image's
existing commands. goodflows itself comes from the release archive. Ampcode stops
setup after 20 minutes and still starts the orb. It publishes a snapshot only
when setup exits 0. The setup log is `/home/user/.cache/amp/logs/setup.log`.

After changing `.agents/setup`, delete the cached snapshots so the next thread
runs the new script:

```bash
amp projects snapshots list <project>
amp projects snapshots delete <project>
```

Omit `parallelism`, or set it to `0`, to use the orb's logical CPU count. The
sample config pins `4`, which matches an `a1.medium` orb (4 CPUs). Choose the
size in the project settings, or pass `--orb-size` when starting a thread.

Tell the agent to run the same checks:

```markdown
## Checks

Run `goodflows` from the repository root before finishing.
`goodflows --jobs test` runs `test` and its prerequisites.
`goodflows --json` prints JSON Lines. The final `summary` event reports `success`.
Fix failures and run `goodflows` again until `success` is true.
```

Run `goodflows` from the thread's Terminal tab. From your own machine, start an
orb thread:

```bash
amp -ox "Run goodflows and fix failures until the summary succeeds." \
  --project <project> \
  --orb-size a1.medium
```

`.agents/resume` runs again each time the orb wakes. Ampcode waits about 10
seconds for it. Keep installation in `.agents/setup`.

## Amp plugin command

The bundled [Amp plugin](.amp/plugins/goodflows.ts) adds **GoodFlows: Run** to Amp's
command palette. Choose it when you want checks and a repair pass. It runs only
when invoked explicitly.

In this repository, the plugin is already in `.amp/plugins/`. For another
project, copy the single file into that project's `.amp/plugins/goodflows.ts`.
For all local projects, copy it into `~/.config/amp/plugins/goodflows.ts` (or
`$XDG_CONFIG_HOME/amp/plugins/goodflows.ts` when set). To use it in orbs, include
the project plugin in the repository or distribute it through Amp's personal or
workspace plugins. See [Amp's plugin guide](https://ampcode.com/docs/customize/plugins).
Run **plugins: reload** from Amp's command palette after installing or changing
the file. `amp plugins list` verifies that Amp can load it.

Install `goodflows` and the project's check dependencies on the executor's `PATH`
first. Start an Amp thread, then choose **GoodFlows: Run**:

1. The command looks for `goodflows.toml`, then `.goodflows.toml`, in Amp's project
   working directory. It does not search parent directories or initialize files.
2. It runs the default workflow if defined. Otherwise it offers a workflow
   picker; cancelling the picker runs no jobs.
3. It sends the results to the thread that invoked the command. On failure,
   the message includes failed jobs, commands, exit codes, bounded log excerpts,
   and the exact command to rerun. It asks the agent to fix failures and rerun
   that config and workflow until checks pass or a blocker prevents progress.

The plugin does not attach an `agent.end` hook. Once it hands off failures,
the agent handles repairs and reruns through its normal shell tools. A passing
run sends a verification summary instead of repair instructions. Missing tools,
invalid configuration, and invalid runner output are reported as blockers for
the agent to investigate. Missing configuration produces a notification.

Choose **GoodFlows: Cancel** to terminate a check currently running through the
plugin without starting repairs. Once repairs have been handed to the agent,
use Amp's **Stop** control. Only one plugin run can execute at a time per plugin
instance. The diagnostic handoff is capped; rerun GoodFlows directly for full output.

To run plugin tests, use Node **22.18 or newer** (built-in TypeScript stripping):

```bash
cargo build --locked
node --test tests/amp-plugin.test.mjs
```

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

Without `--config`, goodflows looks in the current directory for `goodflows.toml`, then
`.goodflows.toml`. Commands default to that file's directory. A configured relative
`workdir` is also resolved from that directory, regardless of where you invoke
goodflows. `--log-dir` is resolved from the invocation directory.

The whole config is validated before any command runs—even when selecting a subset.
Unknown fields, duplicate/empty names, empty commands, missing or duplicate
dependencies, self-dependencies, cycles, invalid environment entries, malformed
direct-command quoting, zero timeouts, and nonexistent workdirs are errors.
Cycle errors include an actual cycle path, such as `a -> b -> a`.

`list` displays every job field: name, command or steps, conditions, output mappings,
dependencies, timeout, configured environment, and shell mode. It also shows effective `fail_fast`, `parallelism`,
and the resolved `workdir`, applying the same CLI overrides and CPU-count default
as a run. In JSON, these values are under `jobs` and `settings`; job fields retain
explicit defaults and nullable timeouts even though generated TOML omits them.

`validate` always checks the entire file before applying selection. Its
`config_valid` JSON event retains `jobs` as the selected count, and includes
`requested_jobs` (explicit CLI names; empty means all), `selected_jobs` (the
dependency closure in config order), and `total_defined_jobs`. Human output also
distinguishes selected and defined counts and lists the names.

## Named workflows

Put independently runnable workflows in one config:

```toml
[settings]
parallelism = 4
fail_fast = true

[[workflows.ci.jobs]]
name = "fmt"
command = "cargo fmt --check"

[[workflows.ci.jobs]]
name = "test"
command = "cargo test --locked"
depends_on = ["fmt"]

[workflows.release.settings]
parallelism = 1

[[workflows.release.jobs]]
name = "build"
command = "cargo build --release --locked"
```

```bash
goodflows list --workflows         # Discover workflow names, counts, and settings
goodflows run ci                  # Run ci's jobs
goodflows run ci --jobs test       # Include ci's fmt prerequisite
goodflows run release
goodflows list ci                 # Inspect ci's jobs and effective settings
goodflows validate ci --jobs test  # Report the selected closure inside ci
goodflows validate                # Validate every workflow
```

Settings precedence is **CLI overrides > environment settings (when selected) >
workflow settings > shared settings > built-in defaults**. A workflow inherits
each omitted field independently; `fail_fast = false` and `parallelism = 0` are
explicit overrides. Both shared and workflow-relative `workdir` values resolve
from the config file's directory.

Job names and dependencies are scoped to their workflow. For example, both ci and
integration can have a job named test. Dependencies must refer to local jobs;
cross-workflow dependencies are not supported. The entire config, including every
workflow's graph and effective workdir, is validated before anything runs.

Existing top-level `[[jobs]]` are the **default** workflow. They work unchanged
with `goodflows`, `goodflows run`, and `goodflows run default`. A config with named workflows
can instead define `[[workflows.default.jobs]]`; combining that with top-level jobs
is an error. Without either default, bare run/list requires you to specify a
workflow—even if there is only one. Goodflows never chooses an arbitrary workflow.

Workflow names are case sensitive, nonempty, at most 64 UTF-8 bytes, and cannot
contain control characters. `init --workflow ci` generates a whole config with
the chosen template's jobs under ci. It follows the usual overwrite confirmation
and `--force` rules; it does not append to an existing config.

In JSON mode, `list --workflows` emits a `workflows` event containing sorted
entries with name, job count, and effective settings, plus nullable
`default_workflow`. Bare `validate` on a config containing named workflows emits
one `config_valid` event with combined counts and a `workflows` array containing
each workflow's count, selected names, and settings. Its top-level requested and
selected name arrays are empty because names are scoped to workflows. Use an
explicit workflow with `--jobs`; aggregate validation does not filter job names.
Legacy single-workflow validation retains its existing fields.
For an explicitly selected workflow, count/name fields describe that workflow;
the entire config is still validated first.

## Conditional jobs

Use expression conditions for environment checks and typed outputs from dependencies.
An expression must return a boolean: `true` runs the job; `false` skips it.

```toml
[[jobs]]
name = "integration"
condition = { expression = '(env.RUN_INTEGRATION ?? "0") == "1"' }
command = "cargo test --test integration"

[[jobs]]
name = "web-tests"
condition = { shell = "test -f package.json" }
command = "npm test"
```

Conditions run once, after all dependencies pass and a parallelism slot is available.
Omitting `condition` runs the job normally. `list` and `validate` check syntax and
reference scopes without evaluating expressions or running shell commands. Runtime
type errors and missing referenced output keys fail the job instead of skipping it.

Expression values are booleans, strings, numbers, and `null`. There is no implicit
truthiness or type conversion: environment values such as `"false"` remain strings.
The supported references are:

- `env.NAME`: inherited environment plus job overrides (and step overrides inside
  a step). An unset or non-UTF-8 variable is `null`; use `??` for a fallback.
- `needs.JOB.outputs.KEY`: published outputs from a job explicitly listed in
  `depends_on`. There are no implicit dependencies or cross-workflow references.
- `steps.ID.outputs.KEY`: an earlier step's outputs, inside a step condition.
  Job conditions cannot read their own steps. Job output mappings can read all steps.

Hyphens work in dot names. Quoted brackets support other names, for example
`needs["build job"].outputs["release.version"]`. References are case sensitive.
Missing output keys, including outputs of skipped steps, are errors even with
`??`; only unset environment variables become `null`.

Operators, from highest to lowest precedence, are `!`, `< <= > >=`, `== !=`,
`&&`, `||`, and `??`. Parentheses override precedence. `&&`, `||`, and `??`
short-circuit; an unevaluated branch performs no output lookup. Comparisons require
compatible types; ordering supports numbers and case-sensitive strings. Equality
with `null` is allowed. Strings use JSON escapes and either quote style. Integer
literals fit signed/unsigned 64-bit ranges; decimal/exponent numbers use finite
floating point. Expressions are limited to 16 KiB and nesting/tree depth 64.

Use `{ shell = "..." }` for external checks. Existing string conditions such as
`condition = "test -f package.json"` remain shell commands, not expressions.
Shell conditions always use `sh -c`, even when `shell = false`. Exit **0** runs
the command; any normal nonzero exit skips it, retaining the exit code and reason.
This includes missing executables and shell syntax errors. Timeouts, cancellation,
signal termination, spawn errors, and output I/O errors retain their normal failure
statuses. Empty shell conditions and NUL bytes are rejected.

Shell conditions inherit the effective environment, use the effective workdir,
and have closed stdin. They run in separate processes: exports and `cd` do not
carry into subsequent commands, but filesystem changes do.

A condition does not override dependency or fail-fast rules. A skipped job skips
its dependents transitively; their conditions are not evaluated. Unrelated jobs
continue, and intentional skips do not trigger fail-fast or make the run fail.
A run containing only passed/skipped jobs succeeds, including when all jobs skip.

## Steps and outputs

A job defines either `command` or nonempty sequential `steps`, never both. This
works for top-level jobs and named workflows. Steps have unique, nonempty `id`s
(at most 64 UTF-8 bytes, no control characters), a required `command`, and optional
`condition`, `env`, and `shell`. Step environment entries override job entries;
an omitted step `shell` inherits the job's value.

```toml
[[jobs]]
name = "detect"
outputs = { backend_changed = "steps.scan.outputs.backend_changed" }

[[jobs.steps]]
id = "scan"
command = '''
changed=false
if test -d src; then changed=true; fi
printf '{"backend_changed":%s}\n' "$changed" > "$GOODFLOWS_OUTPUT"
'''

[[jobs.steps]]
id = "report"
condition = { expression = "steps.scan.outputs.backend_changed" }
command = "echo Backend checks are needed"

[[jobs]]
name = "backend-tests"
depends_on = ["detect"]
condition = { expression = '(env.RUN_TESTS ?? "1") == "1" && needs.detect.outputs.backend_changed' }
command = "cargo test"
```

Each command receives a fresh, private temporary file path in `GOODFLOWS_OUTPUT`.
Write one JSON object to it to publish outputs. Values must be booleans, numbers,
or strings; arrays, nested objects, and `null` values are rejected. A command that
leaves the file untouched publishes no outputs. Invalid/missing/non-regular files
or files exceeding **64 KiB** fail the command. Files are cleaned up automatically;
outputs are read only after the command succeeds and its process group is cleaned
up. Stdout remains logs and is never parsed for outputs. `GOODFLOWS_OUTPUT` is
runner-controlled, overrides any configured value, and is not supplied to shell
conditions.

Successful steps expose their outputs to later steps. Step outputs stay internal
unless explicitly promoted using the job's `outputs` mapping, whose values are
expressions evaluated after all steps complete successfully. Mapping errors fail
the job without publishing partial job outputs. Command-only jobs publish their
output-file keys directly as job outputs and cannot define output mappings.
Outputs appear in JSON results without secret redaction; do not publish secrets.

Steps share the job's workdir and filesystem, but each command has its own process:
shell exports and `cd` do not persist. A skipped step does not block later steps.
A failed/timed-out/cancelled step stops the job; remaining steps do not run.
Failed jobs publish no job outputs. A job with only skipped steps still passes
unless an output mapping fails. Summary counts count jobs, not steps.

The job condition, step conditions, and all commands share **one parallelism slot
and one `timeout_secs` budget**, with the usual signal/process-group cleanup.
Logs and bounded failure tails are shared across all steps in execution order.
Human output identifies the job and step; JSON `job_output` events include `step`
when produced by a step. Raw log paths remain per-job.

## Environment profiles

Share jobs across machines while overriding runner settings for local development,
orbs, or CI:

```toml
[settings]
fail_fast = true

[environments.local.settings]
parallelism = 2

[environments.orb.settings]
parallelism = 4

[environments.ci.settings]
parallelism = 8
fail_fast = false

[[jobs]]
name = "test"
command = "cargo test --locked"
```

```bash
goodflows --env local                 # Run the default workflow with local settings
goodflows run ci --env orb             # Apply orb settings to a named ci workflow
goodflows list ci --env orb            # Inspect effective jobs and settings
goodflows list --workflows --env ci    # Inspect all workflows with ci settings
goodflows validate --env ci            # Validate without running jobs
goodflows --env ci --parallelism 2     # Explicit CLI flags still take precedence
```

The named-workflow examples require a workflow named `ci` in your config.
Environment and workflow names are independent: `--env ci` does not select a
workflow. Without `--env`, no profile is applied; goodflows does not detect the
machine or read an environment variable to select one. The Amp plugin likewise
uses no environment profile.

Each profile can override `parallelism`, `fail_fast`, and `workdir`. Omitted fields
inherit independently, with precedence **CLI overrides > environment settings >
workflow settings > shared settings > built-in defaults**. Explicit `false` and
`0` values are preserved (`parallelism = 0` uses the logical CPU count). Relative
profile workdirs resolve from the config file's directory, not the inherited
workdir. Profiles do not change jobs, dependencies, or job environment variables.

Unknown profile names fail with the available names before jobs run. Names follow
the same case-sensitive, nonempty, 64-byte, no-control-character rules as workflow
names. Every declared profile, including its workdir, is validated even when not
selected. `--env` works with run, list, and validate, but not init. Log paths stay
unchanged; use separate `--log-dir` values to retain logs from different profiles.

Human output identifies the selected environment. JSON events include
`environment` when `--env` is supplied and omit it otherwise. Selected-workflow
`config_valid` events also include effective `settings`; aggregate validation and
workflow discovery report each workflow's settings with the selected profile and
CLI overrides applied.

## Scheduling and failures

Jobs become ready as soon as **all** prerequisites pass. Unrelated work can run
concurrently up to `parallelism`; there are no batch barriers. Ready jobs are
launched in config order. `--jobs` automatically includes every prerequisite and
rejects unknown names; the same selection works with `list` and `validate`.

```bash
goodflows --parallelism 2
goodflows --no-fail-fast
goodflows --jobs fmt,test
goodflows --config ci/checks.toml
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
| `0` | All selected jobs passed or were skipped; or successful init/list/validate (including declined overwrite) |
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
goodflows --log-dir .goodflows-logs
```

Files use indexed, sanitized job names such as `001-fmt.stdout.log` and
`001-fmt.stderr.log`. Named workflows get their own subdirectory, for example
`.goodflows-logs/ci/001-fmt.stdout.log`. Workflow names are encoded as safe path
components: lowercase ASCII letters, digits, `_`, and `-` remain unchanged;
other UTF-8 bytes are percent-encoded (including uppercase letters, avoiding
collisions on case-insensitive filesystems). Legacy top-level jobs keep their
existing flat log paths. Indexes follow the selected jobs' config order; files from
the same selection are overwritten on subsequent runs. Use a fresh directory for
each run to keep historical logs. Add your log directory to your project's
`.gitignore` if desired.

For agents and CI integrations:

```bash
goodflows --json > results.jsonl
goodflows validate --json
goodflows list --jobs test --json
```

`--json` produces JSON Lines only on stdout. Each event includes `schema_version: 1`
and `type`. Types are `run_started`, `job_started`, `step_started`, `step_finished`, `job_output`, `job_finished`,
`summary`, `config_valid`, `jobs`, `workflows`, `config_written`, `init_cancelled`, and `error`.
Events for a selected workflow include `workflow`, including the default workflow.
Discovery/aggregate-validation events and errors before selection may omit it.
An interactive init confirmation goes to stderr in JSON mode. Help/version remain
human-readable even with `--json`. Argument errors produce an `error` event.

```json
{"schema_version":1,"type":"job_output","name":"test","stream":"stdout","data":"ok\n"}
```

The final `summary` contains `success`, counts for every status, and `results` in
config order. Each result contains `name`, `command`, `status`, `duration_ms`,
nullable `exit_code`, nullable `error`, and `outputs`. Stepped jobs additionally
include `steps` in config order, each with `id`, `command`, `status`, `duration_ms`,
nullable `exit_code`, nullable `error`, and `outputs`. Their job-level `command` is
null unless a step failed, in which case it identifies that step's command.
`step_started` includes `name` (job) and `step`; `step_finished` includes `name`
and the step `result`. Failed and timed-out job results also
include `stdout_tail`, `stderr_tail`, `stdout_truncated`, and `stderr_truncated`.
The tails are captured incrementally and capped at **4096 UTF-8 bytes per stream**,
with truncation flags indicating omitted leading content. Non-UTF-8 bytes are
decoded lossily and character boundaries are preserved when trimming the tails.
Empty output produces empty tails with false truncation flags; other statuses
omit the tail fields. Human failure output shows the command and nonempty tails.
Full output remains available in the events and optional raw log files.
Configuration/runtime failures produce an
`error` event instead of a completed summary.

Output is streamed with bounded buffers; long lines can span multiple events.
`job_output.data` preserves newlines where present. Decode events in order and
concatenate data per job/stream to reconstruct text. Non-UTF-8 bytes use replacement
characters; saved log files retain the original bytes. Output order between
concurrent jobs or separate stdout/stderr streams is not guaranteed. Logs can
contain secrets emitted by your commands; goodflows does not redact them.

Output backpressure never blocks timeout or signal handling. On interruption, if
the downstream consumer stops reading, goodflows allows 250 ms for pending output
before discarding further events so it can exit. A final summary cannot be
guaranteed when the output pipe is blocked or closed.

## Development and releases

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
./target/release/goodflows
```

`Cargo.lock` is committed for reproducible dependency resolution. CI runs formatting,
Clippy, tests, a release build, and goodflows's own checks on Linux/macOS with Rust 1.88
and stable. Release builds run checks on all four native platform/architecture
combinations before packaging. Linux archives are then compiled with Rust 1.88.0
in a Debian 12 container so the binary runs on Ampcode orbs (glibc 2.36).
Workflows use GitHub's documented
[runner labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

The Release workflow can be dispatched manually to build artifacts without publishing.
Pushing a `v<version>` tag matching `Cargo.toml` builds and publishes archives,
individual SHA-256 checksums, and `SHA256SUMS`. Maintainers should bump the package
version, update the lockfile, and review CI before pushing a release tag. No release
is published merely by running `goodflows` or pushing a regular branch.

To exercise packaging locally, Linux targets need Docker and produce a Debian 12
binary. macOS targets build natively:

```bash
bash scripts/package-release.sh x86_64-unknown-linux-gnu v0.4.0
```
