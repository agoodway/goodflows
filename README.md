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
The Elixir template declares `MIX_ENV = "test"` once under `[settings.env]`, so every
generated job (including jobs generated into a named workflow with `--workflow`)
inherits it without repeating it per job. Other templates write no `[settings.env]`.
Because `[settings.env]` is shared, `MIX_ENV = "test"` also applies to workflows you
add later; the generated file says so in a header comment, and a workflow can override
it with `[workflows.NAME.settings.env]`. Goodflows binaries older than settings env
support reject the generated `env` field as unknown.

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
history = false       # Opt-in run history files (see Run history)
log_runs = 10         # --log-dir run directories kept per workflow

[settings.env]        # Command environment defaults inherited by every job
CARGO_TERM_COLOR = "always"

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
Jobs inherit the runner's environment, then settings `env` defaults, then their own
`env` entries. Job stdin is closed, so checks must run noninteractively.

### Settings environment defaults

`[settings.env]`, `[workflows.NAME.settings.env]`, and
`[environments.NAME.settings.env]` declare command environment defaults so values
such as `MIX_ENV=test` need not be repeated on every job. Variables merge **by key**,
lowest to highest precedence:

1. the environment goodflows was started with (the caller),
2. shared `[settings.env]`,
3. the selected workflow's `settings.env`,
4. the selected `--env` profile's `settings.env` (only when `--env` selects it),
5. job `env`,
6. step `env` (for that step only).

Each scope keeps the keys it does not override. An omitted or empty map changes
nothing; an empty string (`VALUE = ""`) sets the variable to an empty value. Values
are literal strings: there is no `$VAR` interpolation and no way to unset a
variable. Non-string values (numbers, booleans, arrays, tables) are rejected. A
workflow's overrides never affect sibling workflows or top-level `[[jobs]]`.

```toml
[settings.env]
MIX_ENV = "test"
KEEP = "shared"

[workflows.ci.settings.env]
MIX_ENV = "dev"

[environments.ci.settings.env]
MIX_ENV = "prod"   # `goodflows run ci --env ci` -> MIX_ENV=prod, KEEP=shared, CI=true
CI = "true"
```

Settings defaults reach job and step commands, shell and expression conditions, and
job output expressions the same way job `env` does. Runner-owned variables keep their
existing behavior: partition identity, `GOODFLOWS_ENVIRONMENT`, `GOODFLOWS_OUTPUT`,
and reporting variables are still set or removed by goodflows, and the reporting
`token_env` is still removed from every command. Settings never become the reporting
publisher's endpoint or token; the publisher reads `url_env` and `token_env` only from
the caller's environment.

Without `--config`, goodflows looks in the current directory for `goodflows.toml`, then
`.goodflows.toml`. Commands default to that file's directory. A configured relative
`workdir` is also resolved from that directory, regardless of where you invoke
goodflows. `--log-dir` is resolved from the invocation directory. A configured
`history_dir` (and the default `goodflows-history`) resolves from the config file's
directory, while `--history-dir` resolves from the invocation directory like
`--log-dir` (see [Run history](#run-history)).

The whole config is validated before any command runs—even when selecting a subset.
Unknown fields, duplicate/empty names, empty commands, missing or duplicate
dependencies, self-dependencies, cycles, invalid environment entries, malformed
direct-command quoting, zero timeouts, nonexistent workdirs, and job, step, or
settings `env` entries for the reserved `GOODFLOWS_RUN_ID`, `GOODFLOWS_REPORT_DIR`,
`GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT`, and `GOODFLOWS_ENVIRONMENT`
(or the configured reporting `token_env`) are errors. Every shared, workflow, and
profile `settings.env` map is validated, including unselected workflows and profiles
and entries that a later scope overrides; errors name the settings scope (for
example `environment 'local' settings env`) without printing configured values.
Cycle errors include an actual cycle path, such as `a -> b -> a`.

### Partitioned jobs

A job can declare `partitions` to run as several scheduled instances instead of
fanning work out inside one command:

```toml
[settings]
parallelism = 6   # 4 for other checks, plus count - 1 = 2 for the extra instances

[[jobs]]
name = "compile"
command = "mix compile --warnings-as-errors"

[[jobs]]
name = "test"
command = "MIX_TEST_PARTITION=$GOODFLOWS_PARTITION mix test --partitions $GOODFLOWS_PARTITION_COUNT"
depends_on = ["compile"]
partitions = 3
# Equivalent table form:
# partitions = { count = 3, strategy = "index" }
```

- `partitions = N` is shorthand for `partitions = { count = N, strategy = "index" }`.
  The count must be an integer from 1 to 64. `partitions = 1` is allowed and runs a
  single instance with the partition variables set.
- `index` is the only strategy: goodflows supplies each instance's identity and the
  command splits the work. Other strategies (such as timing-balanced file
  assignment) are reserved for later releases; today any other `strategy` value or
  any other table key is a validation error.
- Every command, step and shell condition of instance *i* receives
  `GOODFLOWS_PARTITION=i` (1-based) and `GOODFLOWS_PARTITION_COUNT=N`, and
  expressions see the same values through `env.GOODFLOWS_PARTITION`. They override
  inherited values. Unpartitioned jobs never see these variables, even when
  goodflows itself was launched with them exported (expressions resolve them to
  `null`). Setting either name in any job, step, or settings `env` (shared, workflow,
  or profile) is a validation error.
- The command must consume the partition variables; a tool that ignores them runs
  its full workload in every instance.
- Partitioned jobs publish no job outputs. An `outputs` mapping on a partitioned job
  and any `needs.<partitioned job>.outputs.*` reference are validation errors.
  Command-only instances do not receive `GOODFLOWS_OUTPUT`; steps inside an instance
  still do, and later steps of the same instance can read `steps.<id>.outputs.*`.
- Partitioned jobs may declare `reports`: each instance gets its own report
  directory and its reports carry the instance's partition (see
  [Test timing reports](#test-timing-reports)).
- An [environment profile](#environment-profiles) can replace the count for one
  machine with `[environments.<profile>.partitions]`. The job's `partitions` value
  is the declared count used by a bare run and by any profile that omits the job.

See [Scheduling and failures](#scheduling-and-failures) for how instances are
scheduled and [Logs and machine output](#logs-and-machine-output) for how they are
reported.
The text run report printed at the end of a run counts configured jobs and shows a
partitioned job as one row (for example `FAIL  test  3ms  1/3 failed`), while the
JSON `summary` counts and `run_started.jobs` count instances.

`list` displays every job field: name, command or steps, conditions, output mappings,
dependencies, timeout, configured environment, shell mode, and partitions (effective
count and strategy, shown only for partitioned jobs; with `--env`, the count is the
selected profile's override when it sets one). It also shows effective `fail_fast`, `parallelism`,
and the resolved `workdir`, applying the same CLI overrides and CPU-count default
as a run. In JSON, these values are under `jobs` and `settings`; job fields retain
explicit defaults and nullable timeouts even though generated TOML omits them.

`validate` always checks the entire file before applying selection. Its
`config_valid` JSON event retains `jobs` as the selected count, and includes
`requested_jobs` (explicit CLI names; empty means all), `selected_jobs` (the
dependency closure in config order), and `total_defined_jobs`. Human output also
distinguishes selected and defined counts and lists the names. These counts are
configured jobs; a partitioned job counts once.

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
integration can have a job named test. Job names must be nonempty and cannot
contain control characters (such as newlines, tabs or terminal escape sequences);
other characters, including spaces and commas, are allowed. Dependencies must refer to local jobs;
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

- `env.NAME`: inherited environment, then settings `env` defaults (shared,
  workflow, selected profile), then job overrides (and step overrides inside a
  step). Job output mappings see the job environment without step-only
  overrides. An unset or non-UTF-8 variable is `null`; use `??` for a fallback.
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
conditions. A value inherited from goodflows' own environment (for example when
goodflows runs inside another goodflows command) is dropped, so expressions see
`env.GOODFLOWS_OUTPUT` as `null` unless job or step `env` sets it. Settings `env`
cannot set it (validation error).

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

Share jobs across machines while overriding runner settings and partition counts
for local development, orbs, or CI:

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

Each profile can override `parallelism`, `fail_fast`, `workdir`, `env`, `history`,
`history_dir`, `log_runs`, `history_runs`, and `history_bytes` under `settings`, and the counts of partitioned jobs under `partitions` (see
[Profile partition counts](#profile-partition-counts)). Omitted fields
inherit independently, with precedence **CLI overrides > environment settings >
workflow settings > shared settings > built-in defaults**. Explicit `false` and
`0` values are preserved (`parallelism = 0` uses the logical CPU count). Relative
profile workdirs resolve from the config file's directory, not the inherited
workdir. Profiles do not change jobs or dependencies, except for the partition
counts described below. A profile's `settings.env` overrides
shared and workflow `settings.env` per key, while job and step `env` still win (see
[Settings environment defaults](#settings-environment-defaults)).
Every command and expression sees the selected profile as `GOODFLOWS_ENVIRONMENT`.
Goodflows sets that variable itself, so job, step, and settings `env` cannot. A run without
`--env` removes it, even when the parent process exported it.

Unknown profile names fail with the available names before jobs run. Names follow
the same case-sensitive, nonempty, 64-byte, no-control-character rules as workflow
names. Every declared profile, including its workdir, is validated even when not
selected. `--env` works with run, list, and validate, but not init. The profile does
not change the log layout: with `--log-dir`, every run writes a new per-run directory
under the workflow's log directory, and each run then prunes that whole directory to
the newest `log_runs` runs, whichever profile produced them, using the cap of the run
doing the prune (see [Logs and machine output](#logs-and-machine-output)). A profile's
`log_runs` therefore does not reserve room for that profile's runs; to keep one
profile's runs apart, give it its own `--log-dir`. History retention works the same way
per workflow history directory (see [Run history](#run-history)).

Human output identifies the selected environment. JSON events include
`environment` when `--env` is supplied and omit it otherwise. Selected-workflow
`config_valid` events also include effective `settings`; aggregate validation and
workflow discovery report each workflow's settings with the selected profile and
CLI overrides applied.

Effective JSON `settings` (from `list WORKFLOW --json`, selected-workflow
`validate --json`, `list --workflows --json`, and aggregate `validate --json`)
include an `env` object with the merged shared, workflow, and selected-profile
`settings.env` defaults (`{}` when none are configured). It never includes the
caller's environment or job/step overrides; `jobs[*].env` in `list` output still shows
each job's explicitly configured `env`. Human `list` output shows the same merged
map on the settings line (`Settings: fail_fast=... parallelism=... env={...}`) and in
each `list --workflows` entry.

### Profile partition counts

A profile can replace the count of a job that already declares
[`partitions`](#partitioned-jobs), so one config can run six test partitions on a
laptop and three in an orb:

```toml
[settings]
parallelism = 4

[environments.local.settings]
parallelism = 8   # size parallelism for the effective count yourself

[environments.local.partitions]
test = 6

[environments.orb.partitions]
test = 3

[[jobs]]
name = "test"
command = "MIX_TEST_PARTITION=$GOODFLOWS_PARTITION mix test --partitions $GOODFLOWS_PARTITION_COUNT"
partitions = 3
```

- Each entry maps a job name to an integer count from 1 to 64. The job keeps its
  strategy and every other field; only the count changes. The effective count is
  what scheduling, `GOODFLOWS_PARTITION_COUNT`, events, logs, summaries, report
  envelopes, and `list` use.
- Without `--env`, or with a profile that omits the job, the job's declared count
  applies.
- A profile cannot add or remove partitioning. Every entry, in every profile
  (selected or not), must name a job that declares `partitions` in at least one
  workflow. A name that matches no job fails with `environment '<profile>':
  unknown job '<job>' (partitioned jobs: ...)`; a name whose jobs are all
  unpartitioned fails with `environment '<profile>': job '<job>' is not
  partitioned`. Counts outside 1 to 64 fail with the profile and job named, and
  non-integer values fail while parsing the TOML.
- Entries apply per selected workflow. A name the selected workflow does not
  contain is ignored. When the selected workflow has a job by that name that does
  not declare `partitions`, the run, `list`, and `validate` fail before any job
  runs with `environment '<profile>': workflow '<workflow>': job '<job>' is not
  partitioned`; aggregate `validate --env` and `list --workflows --env` fail if any
  workflow would. Aggregate `validate` without `--env` checks every profile against
  every workflow the same way, so it fails if any `run <workflow> --env <profile>`
  would.
- Known limitation: this check covers the whole workflow, not just the jobs
  `--jobs` selects. If a profile sets `test` and a workflow has a plain `test` job,
  every command for that workflow fails with that profile, including
  `goodflows run nightly --jobs lint --env local`. Rename one of the jobs or use a
  separate profile for that workflow.
- The count does not change `parallelism`. Extra instances wait for free slots, so
  set the profile's `settings.parallelism` (or `--parallelism`) for the effective
  count; the usual parallelism precedence still applies.
- Older goodflows versions reject the `partitions` table on a profile. Remove it
  before rolling back to an older binary. Saved runs keep the count they ran with,
  so `reports upload` replays them unchanged.

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

A [partitioned job](#partitioned-jobs) expands into one instance per partition.
Each instance occupies its own `parallelism` slot and has its own condition
evaluation, steps, `timeout_secs` budget, status and failure tails. Instances are
launched in index order at the job's position in the config. With `parallelism = 4`,
a ready 3-partition job leaves one slot for other work; with `parallelism = 2`, two
instances start and the third waits for a free slot. Raise `parallelism` by
`count - 1` to keep other checks from queuing behind the instances. There is no
per-job concurrency cap: a ready partitioned job (up to 64 instances) can take every
slot, so jobs later in the config wait until its instances finish. `--jobs test`
selects every instance of `test`, and a job with `depends_on = ["test"]` starts only
after every instance passes. If any instance fails, times out, is cancelled or is
skipped (including by its own condition), dependents are skipped with the usual
dependency reason once the remaining instances finish, even under fail-fast. Jobs
further down the chain that have not started may instead be skipped for fail-fast. Fail-fast, conditions, steps and cancellation apply to each
instance as to any job: a failed instance stops unstarted siblings from starting,
while running siblings finish.

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

## Test timing reports

goodflows can publish per-test timing reports (the runner-neutral
`test-timings/1` JSON Lines format) to GoodFlows. Reporting is opt-in: configs
without a `[reporting]` block behave exactly as before, create no spool
directory, and make no network requests. goodflows ships reporters for
[ExUnit and Jest](#test-reporters); any other runner can produce the
[format](#the-test-timings1-contract) directly.

```toml
[reporting]
project = "goodcases"            # Existing GoodFlows project slug (required)
url_env = "GOODFLOWS_URL"        # Default; names the variable holding the server origin
token_env = "GOODFLOWS_API_KEY"  # Default; names the variable holding the private API key
spool_dir = ".goodflows/reports" # Default; relative to this config file's directory

[[jobs]]
name = "test"
command = "mix test.parallel"
reports = ["test-timings"]
```

- `project` must be 1-64 bytes matching `[a-z0-9][a-z0-9_-]*`. It is the slug of a
  project that **already exists** on the GoodFlows account owning the API key;
  goodflows never creates projects. Reports for an unknown project stay pending
  with reason `project_not_found`.
- `url_env` and `token_env` are environment variable *names*. Credentials are never
  written in TOML: `token`, `api_key` and `url` keys are rejected. Provide values
  to the goodflows process from your shell or CI secret store, for example
  `GOODFLOWS_URL=https://flows.example.com GOODFLOWS_API_KEY=... goodflows`. Sample
  env files should contain empty placeholders only.
- `GOODFLOWS_URL` must be an origin only (`https://host[:port]`), with no path,
  user info, query or fragment. Plain `http://` is accepted only for `localhost`
  and loopback addresses.
- `spool_dir` is resolved against the config file's directory, independent of
  `workdir` and of where goodflows is invoked. Add it to your project's
  `.gitignore` (for the default, `/.goodflows/`).
- `reports` currently supports only `test-timings`, which names the format
  family: reports are collected as the version this goodflows defines
  (`test-timings/1`). Unknown or duplicate formats (including the retired
  `exunit-timings`), `reports` without a `[reporting]` block, and reporting job
  names longer than 64 UTF-8 bytes or containing control characters are
  validation errors.
- `GOODFLOWS_RUN_ID` and `GOODFLOWS_REPORT_DIR` are reserved: setting either in any
  job, step, or settings `env` is a validation error. `url_env` and `token_env` must
  differ and must not name a reserved variable (`GOODFLOWS_RUN_ID`,
  `GOODFLOWS_REPORT_DIR`, `GOODFLOWS_PARTITION`, `GOODFLOWS_PARTITION_COUNT`) or
  `GOODFLOWS_OUTPUT`, and no job, step, or settings `env` may set the `token_env`
  variable (it is removed from every command).
- A [partitioned job](#partitioned-jobs) may declare `reports`. Every instance
  receives its own `GOODFLOWS_REPORT_DIR` (all instances share one
  `GOODFLOWS_RUN_ID`) and is collected separately, once, with its own
  `job_status`. For reports collected from an instance, goodflows sets `partition`
  to the instance index (as a decimal string, for example `"2"`) and
  `partition_count` to the instance count, in both the request envelope and the
  report's initial `suite_started` record, replacing whatever the producer wrote
  (the server requires the two to agree). No other record field changes. Malformed
  producer values (for example `"a b"`) still reject the report. Unpartitioned
  reporting jobs keep the producer's partition values. A producer that ignores
  `GOODFLOWS_REPORT_DIR` and writes to a shared path is never collected.

`validate` and `list` never read credentials, create spool directories or contact
the server. `list --json` includes the nonsecret `reporting` block (project, env
variable names and resolved spool path).

### What happens during a run

When at least one selected job declares `reports`, goodflows:

1. Creates a unique invocation directory `<spool_dir>/<workflow_run_id>/` (a
   lowercase v4 UUID) and captures source context (`git rev-parse HEAD`, the
   current branch and a tracked-files dirty flag) in the resolved `workdir` with a
   2-second limit. Outside Git, or on any failure, those fields are null.
2. Runs report-producing jobs with `GOODFLOWS_RUN_ID` (the UUID) and an absolute
   `GOODFLOWS_REPORT_DIR` pointing to that job's own empty `reports/` directory.
   Producers (and parallel workers that inherit the variable) write `*.jsonl` there.
   Job and step condition commands and expressions do not receive either variable:
   a condition decides whether the job runs and is never a report producer. Values
   of these names inherited from the environment that launched goodflows (for
   example an enclosing goodflows run) are removed from every command whenever a
   `[reporting]` block exists. The run directory and each job's (or, for a
   partitioned job, each instance's) `reports/`, `pending/` and `acks/` directories
   are created private (mode 0700) before any job starts.
3. Removes the `token_env` variable from **every** job, step and condition command,
   and from `${{ env.* }}` expression lookups, even when it is exported in the shell
   that launched goodflows. Only the publisher holds the key. On Linux goodflows also
   marks itself non-dumpable once it has read the key, so other processes of the
   same user (including jobs) cannot read it from `/proc/<pid>/environ`. This is a
   mitigation, not isolation: other platforms, privileged processes and the shell
   that exported the key are unaffected.
4. After all jobs finish (including failed ones), reads only direct, regular
   `*.jsonl` files from each executed reporting job's directory. Symlinks,
   subdirectories and files elsewhere (including older runs and the producer's
   default history directory) are never read. A job counts as executed only once
   its command or a step command spawns; jobs skipped by a dependency or their own
   condition, or cancelled before a command spawns, produce no report
   (`not_executed`).
5. Freezes each valid report as an immutable request snapshot, then publishes the
   snapshots one request per report to
   `<origin>/api/v1/projects/<project>/test-reports` with
   `Authorization: Bearer <key>`, before the final `summary` event.

Publishing is best-effort and never changes check results or the exit status:

- **Uploaded**: a 200/201 acknowledgement with a valid server id and the matching
  `report_id` marks the report acknowledged (`report_uploaded`).
- **Pending**: anything else (offline server, TLS failure `tls_error`, 401/403, 404
  `project_not_found`, other 4xx, redirects, malformed acknowledgements, or
  exhausted budget) leaves the report pending (`report_upload_pending` with a
  structural `reason` and optional `http_status`). Redirects are never followed, so
  the key is never sent to another origin.
- **Rejected by the server**: 400, 409 (immutable conflict), 413, 415 and 422 refuse
  the frozen bytes themselves, so resending them cannot succeed. They are reported
  as `report_upload_pending` with `terminal: true` and recorded in
  `<job>/acks/NNN.rejected.json`; `reports upload` does not resend them.
- **Local only**: without both credential variables, an invalid origin, or after a
  signal, nothing is sent (`reporting_skipped` with reason
  `credentials_unavailable` or `invalid_url`, or `report_upload_pending` with reason
  `cancelled`). If the spool cannot be created, checks run normally without
  reporting variables and `reporting_skipped` reports `spool_unavailable`.

All publishing in one invocation shares a **30-second budget**, with a 3-second
connect timeout and a per-request limit of 10 seconds plus 1 second per started MiB
of body, each clipped to the remaining budget. Transport errors, 429 and 5xx
(including 503) are retried up to three attempts with 1- and 2-second backoffs. A
`Retry-After` in seconds replaces the backoff; if it is not shorter than the
remaining budget the report is left pending at once. An HTTP-date `Retry-After`
falls back to the normal backoff. Each snapshot is re-read (never through a
symlink) and re-validated against the contract and the run's identity right before
it is sent. HTTPS trusts both the bundled WebPKI roots and the operating system's
certificate store. Other 4xx responses and redirects are not retried. A signal stops waits and
requests immediately and preserves the signal exit code. Job `duration_ms` never
includes reporting time.

Collection is bounded: at most 256 reports and 256 MiB of source data per job
(shared by all instances of a partitioned job and consumed in partition order,
so when the instances together exceed a cap the highest-numbered instances lose
reports first: two instances with 130 reports each collect 130 and 126),
100,000 records and 32 MiB encoded per report. Source files over 33 MiB are
rejected (`request_too_large`) without being parsed, and each valid report is
frozen as soon as it is built, so only one report body is held in memory. An
invalid final unterminated line (for example a worker killed mid-write) is
dropped and the report is sent with `truncated: true`; an invalid earlier line
rejects the report. Every record is then validated in full against the
[contract](#the-test-timings1-contract) before anything is frozen, so a frozen
snapshot is one the server accepts: unknown events or keys, missing core keys,
wrong types, mismatched suite/report identities, an invalid partition, invalid
`meta`, an unpaired or duplicated started/finished event, unfinished counts that
disagree with the records, U+0000, an oversized field, a duplicate object key,
nesting deeper than the 16-level body limit, a number with a fraction or
exponent (or an integer beyond ±(2^53-1)), a filter that breaks the tag
grammar, or a `report_id` reused by two reports in the same job (including
across its instances, which rejects every copy as `duplicate_report_id`) rejects
that report locally. The codes are `unsupported_format`, `unsupported_event`,
`unsupported_field`, `missing_field`, `invalid_field`, `field_too_long`,
`invalid_order`, `identity_mismatch`, `invalid_identifier`,
`invalid_partition`, `invalid_partition_count`, `unsupported_filter`,
`invalid_meta`, `unpaired_event`, `unfinished_mismatch`, `duplicate_key`,
`nul_character`, `non_integer_number`, `integer_out_of_range`,
`nested_too_deeply`, `malformed_record`, `invalid_utf8`, `empty_report` and
`missing_suite_started`, plus `too_large`, `record_limit` and
`request_too_large` for limits. A snapshot that cannot be written is counted as
`snapshot_write_failed` (and, if it was also a duplicate, only as that). Each
rejected file is counted under exactly one code, except that a rejected
duplicate whose already-frozen snapshot cannot be removed is also counted as
`snapshot_discard_failed`, which marks the leftover snapshot rather than another
file (it is never listed in the manifest or sent). Only regular files that were
read count toward the 256-report limit. Rejections appear only as codes and
counts in `reports_collected.rejected`; source files are never modified and
record contents are never printed. A signal that arrives while reports are
being collected stops collection between files (`collection_interrupted`).

### The `test-timings/1` contract

goodflows owns the contract: the field tables, limits and identifier grammar
live in one versioned module (`src/reporting/contract.rs`, contract version
`generalize-test-timings`) and the
[`test-timing-contract` spec](openspec/specs/test-timing-contract/spec.md),
and the GoodFlows server mirrors them. In short:

- A report is JSON Lines. Every record has `event`, `suite_id`, `report_id`
  (identifiers: 1-128 bytes of `[A-Za-z0-9._-]`, starting with a letter or
  digit, equal in every record) and `received_offset_us`, and may have `meta`.
- Events are `suite_started` (first, and only first), `group_started`,
  `group_finished`, `test_started`, `test_finished`, `interrupted`,
  `max_failures_reached` and `suite_finished` (last, when present). Each event
  carries all of its core keys (nullable ones as `null`) and nothing else.
- `suite_started` has `format: "test-timings/1"`, `started_at`, `runner`
  (`{name, version}`), `partition`, `partition_count`, `seed` and `filters`
  (`null`, or boolean identifier tags in `include` and `exclude`).
- A *group* is the runner's scheduling unit (an ExUnit module, a Jest file):
  `group`, `file`, `execution_id`, `started_offset_us`, plus `duration_us`,
  `outcome` and `counts` when finished. A *test* has `group`, `path` (describe
  blocks, outermost first), `name`, `file`, `line`, `execution_id` and
  `started_offset_us`, plus `duration_us` and `outcome` when finished. A
  finished event repeats its started event's core values exactly.
- Outcomes are `passed`, `failed`, `skipped` and `errored`. Times are integer
  microseconds. `suite_finished` has `aborted`, `unfinished_tests`,
  `unfinished_groups` (which must equal the started events left unfinished),
  `counts` and `duration_us`.
- `meta` holds runner-specific details: at most 32 identifier keys whose values
  are null, booleans, safe integers or strings of at most 256 bytes. It never
  holds logs, failure messages, assertion values or stack traces.

### Test reporters

goodflows maintains two zero-dependency reporters in `reporters/`, released in
lockstep with the CLI (each reporter's version equals the goodflows version) and
published on every release tag to read-only mirrors. The reporters ship starting
with goodflows v0.6.0, the first release that contains them; earlier goodflows
tags have no reporter tag. Pin the mirror tag equal to your goodflows version
(v0.6.0 or later):

- **ExUnit** (Elixir `~> 1.18`), from
  [`agoodway/goodflows-exunit-reporter`](https://github.com/agoodway/goodflows-exunit-reporter):

  ```elixir
  # mix.exs
  {:goodflows_exunit_reporter,
   github: "agoodway/goodflows-exunit-reporter", tag: "v0.6.0", only: :test}
  ```

  ```elixir
  # test/test_helper.exs
  ExUnit.start()
  Goodflows.ExUnitReporter.configure()
  ```

- **Jest** (Jest 29.6 or newer, Node.js 18 or newer), from
  [`agoodway/goodflows-jest-reporter`](https://github.com/agoodway/goodflows-jest-reporter):

  ```json
  "devDependencies": {
    "goodflows-jest-reporter": "github:agoodway/goodflows-jest-reporter#v0.6.0"
  }
  ```

  ```js
  // jest.config.js
  module.exports = { reporters: ['default', 'goodflows-jest-reporter'] };
  ```

Both write each run to a new `<report_id>.jsonl` in `GOODFLOWS_REPORT_DIR` when
goodflows runs them, take their partition from `GOODFLOWS_PARTITION` and
`GOODFLOWS_PARTITION_COUNT` when both are valid (otherwise the runner's own
partitioning, when it is complete, otherwise `null` and `1`), and take their
suite id from the reporter's option, then `TEST_TIMING_SUITE_ID`, then
`GOODFLOWS_RUN_ID` (so every report of one goodflows run shares it), then the
report id. Each `suite_started.meta` names the reporter (`reporter`,
`reporter_version`) and says whether the run was `filtered` by name, tag or
path. A reporter's own directory option (`:test_timing_directory`,
`outputDirectory`) wins over `GOODFLOWS_REPORT_DIR`; because goodflows then
cannot collect the report, the reporter prints a warning when both are set and
differ, so leave the option unset in goodflows jobs. See each reporter's README
for its options, mapping and duration semantics. Changes go to this repository;
CI (and every release, before publishing) runs both reporters over fixture
suites with Jest at its minimum peer version and the latest, and fails if
goodflows rejects any report they write, if a report is truncated, or if its
outcomes and counts differ from the fixture's (`scripts/check-reporters.sh`).

### Upgrading from `exunit-timings`

- Change `reports = ["exunit-timings"]` to `reports = ["test-timings"]`, and
  replace a copied `GC.TestTimingFormatter` with the ExUnit reporter (see [Test
  reporters](#test-reporters); the suite option is now `:test_timing_suite_id`
  and `TEST_TIMING_SUITE_ID`). Until you do, validation fails with "format
  'exunit-timings' was retired", which links here.
- The GoodFlows server must support `test-timings/1` before you upload with this
  version. Until it does, uploads are rejected (`rejected_by_server`); delete the
  matching `acks/NNN.rejected.json` files to resend them once it does.
- Pending snapshots frozen in the old format are never sent: `reports upload`
  counts each as `invalid_snapshot` and exits nonzero. Their raw JSONL stays on
  disk.

### Spool layout and retention

```text
.goodflows/reports/<workflow_run_id>/
  manifest.json              # running -> finalized (or aborted)
  <job>/reports/*.jsonl      # raw producer output, kept as written
  <job>/pending/000.json     # frozen request snapshots, written before any HTTP
  <job>/acks/000.json        # server acknowledgement, written atomically on success
  <job>/acks/000.rejected.json  # terminal server rejection (400/409/413/415/422)
  <job>/p<index>/reports/*.jsonl   # partitioned jobs: one directory per instance,
  <job>/p<index>/pending/000.json  # each with its own reports/, pending/ and acks/
  <job>/p<index>/acks/000.json
```

Job directory names use the same reversible encoding as workflow log directories
(`Test` becomes `%54est`). A partitioned job (including `partitions = 1`) has no
`reports/`, `pending/` or `acks/` of its own; each instance `p1`, `p2`, ... has them
instead, and numbers its snapshots from `000`. Unpartitioned jobs keep the layout
above. The manifest stores only structural metadata: project, workflow,
environment, the credential variable *names*, source context, job statuses,
collection outcomes and each report's id and snapshot index (file locations are
derived from the job directory, partition and index, never stored as paths). A
partitioned job has one manifest entry per instance, with `partition`
(`{"index": i, "count": N}`) and that instance's own status; unpartitioned entries
have no `partition` key. A manifest is `"version": 1` unless some entry has a
partition, in which case it is `"version": 2`. It never stores the endpoint, the key or other environment
values.

The manifest is `running` while jobs execute and becomes `finalized` once every
snapshot is written; a scheduler error marks it `aborted`. If goodflows is killed
before finalizing, the raw files and unfinished manifest remain for diagnosis but
cannot be replayed. Nothing is pruned automatically: after every report in a run is
acknowledged, delete its directory manually (for example
`rm -rf .goodflows/reports/<workflow_run_id>`).

### Retrying a saved invocation

```bash
GOODFLOWS_URL=https://flows.example.com GOODFLOWS_API_KEY=... \
  goodflows reports upload .goodflows/reports/67f7946d-136b-4608-ab01-e1f251a09754
```

`reports upload` reads only the finalized manifest and its frozen snapshots. It
validates the manifest first (version 1, or 2 exactly when an entry has a
partition; a lowercase canonical run id, the project slug and credential variable
name rules, job names, and partition entries: an index from 1 to its count, a
count from 1 to 64, no repeated index, one count per job, and no job with both
partitioned and unpartitioned entries) and refuses a tampered one without
contacting the server. Partitioned runs replay per instance: each instance's
snapshots are read from its own `p<index>/` directory, re-validated against that
instance's partition, and acknowledged in its own `acks/`. Entries may appear in
any order, and a manifest listing only some of a job's instances replays the
instances it lists. A `report_id` listed more than once within one job, even by
different instances, is not sent at all: every copy is reported as
`invalid_snapshot`. Manifests written before partitions existed replay unchanged.
Replay a run with the goodflows version that wrote it or a newer one: an older
goodflows refuses a version 2 manifest with an unsupported-version error. The
same rule covers snapshot contents, because every snapshot is re-validated
against the replaying goodflows's contract before it is sent: a newer goodflows
refuses (`invalid_snapshot`) a snapshot its rules no longer accept, such as one
frozen in the retired `exunit-timings` format. A refused
snapshot is never sent; its raw JSONL stays in `reports/`. Because the manifest chooses which of your
environment variables become the endpoint and key, only replay run directories
produced by your own invocations. It does
not need the project checkout or config, runs no jobs and does not read current Git
state, so the run directory can be copied elsewhere. Credentials are read from the
variable names saved in the manifest. Already acknowledged reports are skipped; the
same immutable request is resent for every other report, so a retry after a lost
acknowledgement completes as an idempotent duplicate. Concurrent retries are safe:
each acknowledgement is recorded in its own file. Reports the server rejected
permanently are skipped (`rejected_by_server`); delete the matching
`acks/NNN.rejected.json` to send one again. `--budget <seconds>` (1-3600) replaces
the 30-second publishing budget for large reports on slow links.

It uses the same budget, retry and validation rules as a run, ends with a
`reports_upload_summary` event (`acknowledged`, `pending`, `rejected`, `invalid`;
`invalid` counts every file collection rejected, once each: it excludes
`snapshot_discard_failed`, which marks a leftover snapshot of a file already
counted as `duplicate_report_id`),
and exits 0 only when every frozen report is acknowledged, none was rejected by the
server and collection rejected no files; otherwise it exits 1 (including missing
credentials or an invalid origin, and for every replay of a run whose collection
rejected a file). A signal exits with its signal code. Only the
global `--json` flag applies.

## Logs and machine output

Human output prefixes each stdout/stderr line with its job name and stream:

```text
[test stdout] running 12 tests
[lint stderr] warning: ...
```

Instances of a partitioned job are shown as `name (index/count)` on job, step and
output lines:

```text
→ test (2/3)
[test (2/3) stdout] 2/3
test (2/3) PASSED  2ms exit=0
```

Optionally save raw stdout and stderr separately:

```bash
goodflows --log-dir .goodflows-logs
```

Each invocation writes its logs in a new run directory named
`YYYYMMDDTHHMMSSZ-<run-id>`: the run's UTC start second and a lowercase UUID run id
(the same id names the run's history file and, when reporting is configured, is the
reporting `workflow_run_id`). Named workflows keep their own subdirectory, so
workflow `ci` writes `.goodflows-logs/ci/20261104T093000Z-<run-id>/001-fmt.stdout.log`;
legacy top-level jobs write `.goodflows-logs/20261104T093000Z-<run-id>/001-fmt.stdout.log`
(no `default/` subdirectory). Besides the logs, the run directory holds only
`.goodflows.lock`, which the run locks for as long as it runs.

Files use indexed, sanitized job names such as `001-fmt.stdout.log` and
`001-fmt.stderr.log`, with raw stdout and stderr bytes (including output from shell
conditions and every step) and no prefixes or timestamps. An instance that never
starts creates no files. Each instance of a partitioned job writes
its own pair of files, with the partition index zero-padded to two digits after the
job name so instance logs sort in index order: a 3-partition `test` at position 2 in
the selected job list writes `002-test.p01.stdout.log` through
`002-test.p03.stdout.log` and the matching `.stderr.log` files. Workflow names are encoded as safe path
components: lowercase ASCII letters, digits, `_`, and `-` remain unchanged;
other UTF-8 bytes are percent-encoded (including uppercase letters, avoiding
collisions on case-insensitive filesystems). Indexes follow the selected jobs'
config order.

A later run never truncates an earlier run's files. After creating its run
directory, goodflows keeps the newest `log_runs` run directories (default 10,
minimum 1) in that workflow's log directory, by name, and deletes older ones. That
count covers every run in the directory, whichever profile produced it, and uses the
cap of the run doing the prune; to keep one profile's runs apart, give it its own
`--log-dir`. The
new directory is never deleted, nor is one whose run is still active (its lock is
held; a run builds its directory as `.<name>.tmp` and renames it only once locked),
so overlapping runs sharing `--log-dir` can briefly exceed the cap. A run directory
whose lock file exists but cannot be opened is kept and reported. A leftover
`.<name>.tmp` directory from a crash during setup is an unrelated name and stays. A run that
crashed still counts toward the cap. Prune removes only names matching the run
directory pattern and never follows or removes a symlink; other files stay. Set
`log_runs` in shared, workflow, or profile `settings`, or pass `--log-runs N`
(precedence: CLI, profile, workflow, shared, default). It has no effect without
`--log-dir`. Failing to create the run directory fails the run before jobs start;
a prune failure is reported on stderr and does not change the exit code. An entry
a concurrent run already deleted counts as pruned. Directories goodflows creates for
logs are private to the invoking user (mode 0700), and each log file is created new
with mode 0600, never following or truncating an existing entry.

**Compatibility:** earlier versions wrote flat files such as
`.goodflows-logs/001-fmt.stdout.log` and overwrote them on every run. Those paths now
sit one directory deeper, and `summary.log_dir` names the run directory. Old flat
files are left in place (they do not match the run directory pattern); delete them
by hand. Add your log directory to your project's `.gitignore`.

For agents and CI integrations:

```bash
goodflows --json > results.jsonl
goodflows validate --json
goodflows list --jobs test --json
```

`--json` produces JSON Lines only on stdout. Each event includes `schema_version: 1`
and `type`. Types are `run_started`, `job_started`, `step_started`, `step_finished`, `job_output`, `job_finished`,
`summary`, `config_valid`, `jobs`, `workflows`, `config_written`, `init_cancelled`, and `error`.
Runs with reporting jobs also emit `reports_collected`, `report_uploaded`,
`report_upload_pending` and `reporting_skipped` before the final `summary`, and
`reports upload` ends with `reports_upload_summary` (see Test timing reports).
For an instance of a partitioned job, `reports_collected`, `report_uploaded` and
`report_upload_pending` (from a run or from `reports upload`) keep `job` and add
`partition` (`{"index": i, "count": N}`); text output shows the job as
`job (i/N)`, for example `report test (2/3)/r2 uploaded`.
Events for a selected workflow include `workflow`, including the default workflow.
Discovery/aggregate-validation events and errors before selection may omit it.
An interactive init confirmation goes to stderr in JSON mode. Help/version remain
human-readable even with `--json`. Argument errors produce an `error` event.

```json
{"schema_version":1,"type":"job_output","name":"test","stream":"stdout","data":"ok\n"}
```

Events and results for an instance of a partitioned job repeat the job `name` and
add `partition` with `index` (1-based) and `count`. `job_started`, `step_started`,
`step_finished`, `job_output` and `job_finished` results all carry it, including
results for instances skipped or cancelled before they started. Use
`(name, partition.index)` as the instance key. Events for unpartitioned jobs have
no `partition` field. `run_started.jobs` and the JSON `summary` counts count
instances, so `compile` plus a 3-partition `test` reports 4 jobs. The text run
report counts configured jobs instead, so the same run reads `2 passed`.

```json
{"data":"2/3\n","name":"test","partition":{"count":3,"index":2},"schema_version":1,"stream":"stdout","type":"job_output","workflow":"default"}
```

The final `summary` contains `success`, counts for every status (`passed`,
`failed`, `timed_out`, `skipped`, `cancelled`), integer `duration_ms` (the run's
wall-clock time, including reporting), integer `execute_ms` (the part spent running
jobs, excluding reporting after them), `log_dir` (this run's log directory, such as
`logs/ci/20261104T093000Z-<run-id>`; present only with `--log-dir`, and converted
lossily when the path is not UTF-8), the optimizer fields described in
[Run history](#run-history) (`run_id`, `started_at`, `outcome`, `parallelism`,
`graph_sha256`, `max_in_flight`, `host`, and `history_file` when a history file was
written), and `results` in config order, with a partitioned job's instances in index order at its
position. Each result contains `name`, `command`, `status`, `duration_ms`,
nullable `exit_code`, nullable `error`, and `outputs`. Summary results also carry
boolean `started`: true when the instance entered execution (including one a
condition then skipped), false when it was skipped or cancelled while still
queued. Summary results also carry `blocked_ms`, `queued_ms` and, when a process
was spawned, `commands` (see [Run history](#run-history)). `started` and these
fields appear only in `summary.results`, never in `job_finished`. Stepped jobs additionally
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
omit the tail fields. Full output remains available in the events and optional raw
log files.
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

### Run report

In text mode a run ends with a report built from the `summary` event, printed after
every job and reporting line. A failed `job_finished` line shows only its status,
duration, exit code and error; the command and output tails appear once, in the
report. For a config with top-level `[[jobs]]` `fmt`, `lint`, a 2-partition `test`
that fails and a dependent `deploy`, run with `--log-dir logs` (a named workflow
shows its subdirectory, such as `Logs: logs/ci/20261104T093000Z-<run-id>`):

```text
default  FAILED  in 207ms
  PASS  fmt     203ms
  PASS  lint      2ms
  FAIL  test      3ms  2/2 failed
  SKIP  deploy         dependency did not pass
2 passed, 1 failed, 1 skipped

Failures
--- test (1/2)  FAILED  exit 1
  command: echo boom-$GOODFLOWS_PARTITION >&2; exit 1
  error: exited with 1
  stderr tail:
    boom-1

--- test (2/2)  FAILED  exit 1
  command: echo boom-$GOODFLOWS_PARTITION >&2; exit 1
  error: exited with 1
  stderr tail:
    boom-2

Logs: logs/20261104T093000Z-67f7946d-136b-4608-ab01-e1f251a09754
Rerun failed: goodflows run default --jobs='test,deploy'
```

- The header shows the workflow, the outcome (`FAILED` if any instance failed or
  timed out, otherwise `CANCELLED` if any was cancelled or a signal arrived before
  the report, otherwise `PASSED`) and the wall-clock time. The time runs from just
  before the first job until the report, so it includes reporting (collection and
  uploads) after the last job finished.
- There is one row per selected configured job, including prerequisites. A
  partitioned job's row takes its most severe instance status (failed, timed out,
  cancelled, passed, skipped) and lists nonzero non-passed instance counts. The
  duration is the longest among instances that started; jobs that never started
  show none. Skipped rows list their distinct skip reasons.
- Counts are per configured job, with zero counts omitted.
- Columns are aligned by character count, which assumes narrow characters; wide
  characters (for example CJK) in job names shift the rest of that row.
- Failures has one entry per failed or timed-out instance: the instance, its status
  and exit code (when present), then the failing step when one failed or timed
  out, then the command and error when present, then the job-level stderr and
  stdout tails (which can include output of earlier steps).
- Control characters other than tab in commands, errors, skip reasons and tails are
  shown escaped (for example `\u{1b}`), here and in live failure lines, so job
  output cannot rewrite the report.
- `Logs:` appears with `--log-dir` and names this run's log directory. `History:`
  follows it (or stands alone without `--log-dir`) only when a history file was
  written. When a job failed or timed out, the report ends
  with a shell-quoted rerun command keeping the workflow, `--env` and an explicitly
  passed `--config`, and selecting the failed jobs plus jobs skipped because a
  dependency did not pass, in row order (`--jobs` adds their dependencies). It does
  not repeat `--parallelism` or fail-fast flags. A selected job whose name contains
  a comma cannot be selected with `--jobs`, so the report explains that and
  suggests rerunning the whole workflow.

**Compatibility:** the text `Summary: N passed, ...` line and the `All jobs passed`
and `All checks succeeded (some jobs skipped)` lines were replaced by the run report.
Scripts should use `--json`, whose `summary` event only gained fields
(`duration_ms`, `execute_ms`, `log_dir`, `results[].started`, and the run history
fields), except
that `log_dir` now names the per-run log directory.

## Run history

Run history is an opt-in record of every run, small and stable enough to commit, so
an agent can compare queue wait, CPU time, and memory across commits while tuning
`parallelism`, partitions, and timeouts. Each run writes one new JSON file for that
run only; earlier files are never rewritten.

Enable it in shared, workflow, or profile settings, or per run:

```toml
[settings]
history = true                  # default false
history_dir = "goodflows-history" # default; relative to the config file's directory
history_runs = 1000             # files kept per workflow (default 1000, minimum 1)
history_bytes = 10485760        # bytes kept per workflow (default 10 MiB, minimum 1)
log_runs = 10                   # --log-dir run directories kept (default 10, minimum 1)

[environments.local.settings]
history = false                 # an explicit false beats a lower true
```

```bash
goodflows run ci --history                 # enable for this run
goodflows run ci --no-history              # disable even when settings enable it
goodflows run ci --history-dir runs        # enable, writing under ./runs
goodflows run ci --history --history-runs 50 --history-bytes 1048576 --log-runs 3
```

Precedence is the same as `parallelism`: **CLI > selected profile > workflow > shared >
default**. `--no-history` forces history off and conflicts with `--history`;
otherwise `--history` or `--history-dir` turns it on. `--log-runs`, `--history-runs`
and `--history-bytes` override the resolved settings; `0` is an argument error
(exit 2) and `0` in a config fails validation. `history_bytes` has no upper bound:
the 10 MiB value is a default, not a maximum, so set it lower or higher as needed.
`list` and `validate` (JSON `settings`, and the `History:` lines of human `list`
output) show the effective `history`, resolved `history_dir`, `log_runs`,
`history_runs` and `history_bytes` with CLI overrides applied. `validate`, `list`,
`init` and `reports upload` never write history, and `reports upload` rejects all
six flags.

**Layout.** A history file is
`<history dir>/<workflow>/YYYYMMDDTHHMMSSZ-<run-id>.json`, using the same encoded
workflow component as log directories and the same stem as the run's log directory.
Legacy top-level jobs record under `default`. The default directory is
`goodflows-history` beside the config file; a configured `history_dir` resolves from
the config file's directory, `--history-dir` from the invocation directory, and
absolute paths are used as given. A symlink at the history directory or the workflow
directory is refused. Directories goodflows creates there are private to the invoking
user (mode 0700) and history files have mode 0600; the workflow directory is opened
once and the file is written and pruned relative to it, so a path swapped for a symlink
mid-run cannot redirect the write.

**When.** A file is written exactly when the run emits a `summary`, including a run
cancelled by a signal (its `outcome` is `cancelled`). A run that errors before the
summary (for example when the log run directory cannot be created) writes none. The
file is created atomically (written to a `.tmp<random>` file, then linked to its name)
and never replaces an existing file. A write failure is reported on stderr, leaves the
exit code unchanged and omits `summary.history_file`. A prune failure after a
successful write is reported on stderr too, but `summary.history_file` still names the
new file.

**Retention.** After writing, goodflows deletes the oldest files of that workflow by
name until at most `history_runs` files and `history_bytes` bytes remain. The caps
are those of the run doing the prune and cover every file in the workflow directory,
whichever profile wrote it; to keep one profile's history apart, give it its own
`history_dir`. The new file is always kept, even when it alone exceeds
`history_bytes`. Unrelated names, other workflows and symlinks are left alone, except
that a `.tmp<random>` file older than an hour, left by a crash mid-write, is removed. A
file a concurrent run already deleted counts as pruned.

**Contents.** Each file is one compact UTF-8 JSON object with a trailing LF:

```json
{"schema_version":1,"workflow":"ci","environment":"local","requested_jobs":["test"],"run_id":"67f7946d-136b-4608-ab01-e1f251a09754","started_at":"2026-11-04T09:30:00Z","outcome":"passed","duration_ms":2310,"parallelism":4,"graph_sha256":"…","max_in_flight":3,"host":{"os":"linux","arch":"x86_64","logical_cpus":8,"mem_total_bytes":16777216000,"mem_available_bytes":9000000000},"source":{"commit":"0123456789abcdef0123456789abcdef01234567","dirty":false},"results":[{"name":"test","partition":{"index":1,"count":2},"status":"passed","exit_code":0,"command":"mix test","error":null,"duration_ms":1980,"started":true,"blocked_ms":204,"queued_ms":0,"commands":[{"kind":"command","wall_ms":1978,"cpu_user_ms":5120,"cpu_system_ms":610,"peak_rss_bytes":412000000}]}]}
```

- `outcome` is `failed` if any instance failed or timed out, otherwise `cancelled` if
  any was cancelled or a signal arrived before the summary, otherwise `passed`. The
  same snapshot sets `summary.outcome`, the text report header and the exit code.
- `environment` is the `--env` profile (omitted without one) and `requested_jobs` the
  `--jobs` selection as given (omitted when every job ran), so runs of different
  selections are not compared by accident. `source` holds the `commit` the run
  started from and whether tracked files were `dirty` (the same git capture reporting
  uses, never the branch or remotes); it is omitted outside a git checkout.
- `duration_ms` is the time spent running jobs, matching `summary.execute_ms`; it
  excludes reporting uploads after the last job, which `summary.duration_ms` includes.
  `parallelism` is the effective value; `max_in_flight` is the most instances
  executing at once. `host` is sampled at run start, before any job: `os` and `arch`
  (Rust's names, such as `linux`/`macos` and `x86_64`/`aarch64`) and `logical_cpus`
  are always present; `mem_total_bytes` and `mem_available_bytes` come from Linux
  `/proc/meminfo` and are omitted elsewhere.
- `graph_sha256` hashes the selected jobs (names, commands, dependencies, effective
  partitions, timeouts, shell flags, conditions and steps), never env values or
  paths, so equal hashes mean the same job graph.
- Per instance: `blocked_ms` is the time from the start of scheduling until every
  dependency passed (or until an earlier skip or cancel); `queued_ms` is the wait
  after that for a free slot. A job skipped because a dependency failed has `started`
  false, `queued_ms` 0 and no `commands`.
- `commands` has one entry per spawned shell condition (`kind` `condition`) or
  command (`kind` `command`), with the `step` id for steps, `wall_ms`, and, when the
  platform measures them, `cpu_user_ms`, `cpu_system_ms` and `peak_rss_bytes`. CPU
  time includes descendants the process waited for; for a timed-out or cancelled
  command it is a lower bound, because the process group is killed before the shell
  reaps its children. Peak RSS (Linux only) is the peak of samples of the summed
  resident memory of the processes still in the command's process group. One sampler
  serves the whole run, scanning `/proc` once per sample for every running command,
  on output at most every 50 ms and otherwise every 200 ms; processes that left the
  group or exited between samples are not counted, and it is omitted (never 0) when no
  sample saw a live, resident process. macOS records CPU and omits RSS.
- `command` and `error` are cut to 1024 bytes on a character boundary (the summary
  keeps them whole). In `error`, wherever it begins a whole path anywhere in the
  string, the log run directory together with anything below it (including the log
  filename) becomes `<log>`, and the workdir becomes `<workdir>` (a path below it
  keeps its tail, as in `<workdir>/src`). Only whole paths match, so workdir
  `/home/u/proj` leaves `/home/u/proj2/x` and `/x/home/u/proj` alone. Configured
  command text is stored verbatim.
- Stdout, stderr, output tails, job and step `outputs`, env values and log filenames
  are never written. `history_file` appears only on the `summary`.

**Git.** History files are designed to be committed, so a branch carries the
measurements of its own commits; the log directory and the reporting spool are never
meant to be. Before committing history, note:

- History files contain configured command text (up to 1 KiB per job) verbatim. If a
  command inlines a secret instead of reading it from the environment, do not commit
  history.
- Prefer enabling history where measurements are comparable, for example in a CI
  profile (`[environments.ci.settings] history = true`) or with `--history` in the CI
  command, rather than in shared `[settings]`, so every local run does not add a file.
- A run writes a new untracked file, so a clean-tree check after the run (such as
  `git status --porcelain` or `git diff --exit-code` with untracked files) sees it.
  Run such checks before goodflows, or point `--history-dir` outside the checkout.
- Files from different machines in one directory are not directly comparable; `host`
  identifies each, and a separate `history_dir` per machine class keeps them apart.

A typical setup that commits history:

```gitignore
# .gitignore
.goodflows-logs/
.goodflows/
```

```gitattributes
# .gitattributes
goodflows-history/** text eol=lf linguist-generated
```

To keep history local instead, ignore it too:

```gitignore
# .gitignore
goodflows-history/
```

goodflows does not edit either file. Prune only drops files from the next commit's
tree: records that were committed and later deleted stay in git history, so the caps
bound the checkout, not `.git`.

## Development and releases

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --release --locked
./target/release/goodflows
```

`Cargo.lock` is committed for reproducible dependency resolution. Edition 2021 does
not make the resolver MSRV-aware, so `.cargo/config.toml` sets
`[resolver] incompatible-rust-versions = "fallback"`; a plain `cargo update` then
prefers versions compatible with `rust-version` (1.88). Confirm with
`cargo +1.88.0 check --locked`. HTTPS uses rustls on the ring provider, so release
binaries need no OpenSSL. CI runs formatting,
Clippy, tests, a release build, and goodflows's own checks on Linux/macOS with Rust 1.88
and stable. Release builds run checks on all four native platform/architecture
combinations before packaging. Linux archives are then compiled with Rust 1.88.0
in a Debian 12 container so the binary runs on Ampcode orbs (glibc 2.36).
Workflows use GitHub's documented
[runner labels](https://docs.github.com/en/actions/reference/runners/github-hosted-runners).

The Release workflow can be dispatched manually to build artifacts without publishing
(its `mirror-dry-run` job prints the reporter splits and pushes nothing).
Pushing a `v<version>` tag matching `Cargo.toml` builds and publishes archives,
individual SHA-256 checksums, and `SHA256SUMS`, and then pushes each bundled
reporter to its read-only mirror with the same tag. No release is published merely
by running `goodflows` or pushing a regular branch.

On a tag push the jobs run in this order, and each stops the release if it fails:

1. `verify-versions`: the CLI and both reporter versions, and the tag, agree.
2. `mirror-check` (in the `reporter-mirrors` environment): both deploy keys are
   set, the tag names a commit on `main`, and each mirror's `main` is an ancestor
   of the new split (`scripts/mirror-reporters.sh <tag> --verify`). Nothing is
   pushed. `reporters` runs the same reporter drift guard as CI, alongside.
3. `build` and then `publish` create the GitHub release.
4. `mirror-reporters` (in the `reporter-mirrors` environment) repeats the checks
   and pushes each mirror's `main` and tag, only after the release is published,
   so no mirror ever has a tag without a CLI release. If it fails after publish
   (for example a network error), fix the cause and use **Re-run failed jobs**:
   a mirror that was already pushed is up to date, so the re-run only pushes the
   rest.

Releases are tagged on `main` only. A tag on any other branch fails
`mirror-check` before anything is built, because the mirrors' `main` could not
fast-forward to it later. A fix for an older release ships as the next patch
version from `main`; there are no maintenance-branch releases.

To cut a release:

1. Bump the version in `Cargo.toml` (and run `cargo update -p goodflows` to update
   `Cargo.lock`), `reporters/exunit/mix.exs` (`@version`) and
   `reporters/jest/package.json` together. The reporters are versioned in lockstep
   with the CLI; CI's `release-scripts` job fails on every commit where the three
   versions disagree, and the Release workflow's `verify-versions` job also fails
   unless the tag agrees. Check locally with `scripts/verify-versions.sh v<version>`.
2. Review CI on `main`, including the `reporters` and `release-scripts` jobs.
3. Make sure the `reporter-mirrors` environment, both mirror deploy keys and the
   `v*` tag ruleset are configured (below); a tag push fails before building when
   either secret is missing.
4. Push the `v<version>` tag from `main` (`git tag v<version> origin/main`, then
   `git push origin v<version>`).

`scripts/test-release-scripts.sh` (run in CI) tests `verify-versions.sh` and
`mirror-reporters.sh` against local bare repositories: version drift, a tag
that does not match, missing keys, first and fast-forward releases, `--verify`
and `--dry-run`, a mirror edited directly, and tags off `main` or off `HEAD`.

### Reporter mirror deploy keys

The `mirror-reporters` job pushes `reporters/exunit` to
`agoodway/goodflows-exunit-reporter` and `reporters/jest` to
`agoodway/goodflows-jest-reporter` (`scripts/mirror-reporters.sh`), after
`mirror-check` has fetched and checked both. Each mirror has
its own write-enabled deploy key, whose private half is a secret of the
`reporter-mirrors` environment in `agoodway/goodflows`. Only `v*` tags may deploy
to that environment, so no branch, pull request or `workflow_dispatch` run can
read the keys, and a tag ruleset limits who can create `v*` tags:

| Environment secret | Deploy key on (with write access) |
|---|---|
| `EXUNIT_REPORTER_DEPLOY_KEY` | `agoodway/goodflows-exunit-reporter` |
| `JEST_REPORTER_DEPLOY_KEY` | `agoodway/goodflows-jest-reporter` |

One-time setup, by a repository admin:

1. Create both mirror repositories empty, if they do not exist yet.
2. Generate one key pair per mirror, without a passphrase:

   ```bash
   ssh-keygen -t ed25519 -N "" -C "goodflows release: goodflows-exunit-reporter" -f exunit_reporter_deploy_key
   ssh-keygen -t ed25519 -N "" -C "goodflows release: goodflows-jest-reporter" -f jest_reporter_deploy_key
   ```

3. In each mirror, open **Settings → Deploy keys → Add deploy key**, paste that
   mirror's `.pub` file and check **Allow write access**.
4. In `agoodway/goodflows`, open **Settings → Environments → New environment**,
   name it `reporter-mirrors` and, under **Deployment branches and tags**, choose
   **Selected branches and tags** and add a rule with **Ref type: Tag** and name
   pattern `v*`. Add no other branch or tag rule.
5. In that environment, under **Environment secrets → Add environment secret**,
   add each private key file's full contents under the secret name above (or
   `gh secret set EXUNIT_REPORTER_DEPLOY_KEY --env reporter-mirrors -R
   agoodway/goodflows < exunit_reporter_deploy_key`, and the same for Jest). If
   either name exists as a repository secret (**Settings → Secrets and variables
   → Actions → Repository secrets**), delete it (`gh secret delete
   EXUNIT_REPORTER_DEPLOY_KEY -R agoodway/goodflows`), so the keys live only in
   the environment.
6. Delete the local key files.
7. Protect release tags: open **Settings → Rules → Rulesets → New ruleset → New
   tag ruleset**, name it `release tags`, set **Enforcement status** to
   **Active**, add the target **Include by pattern** `v*`, and enable **Restrict
   creations**, **Restrict updates**, **Restrict deletions** and **Block force
   pushes**. Under **Bypass list**, add only the role allowed to cut releases
   (for example **Repository admin**). Since a `v*` tag is what unlocks the
   deploy keys, this keeps anyone else from creating one.
8. Confirm the split without the keys: **Actions → Release → Run workflow** on
   `main`. The `mirror-dry-run` log prints `source: ... is on origin/main`, each
   mirror's split SHA and "secret ... is not set; skipping the mirror fetch"
   (that job never gets the keys), and ends with "dry run: nothing pushed". The
   keys themselves are checked by `mirror-check` on the first tag push, before
   anything is built: a missing secret fails it with an error that names each
   missing secret and links here, and a key without access fails with "cannot
   reach".

SSH trusts only GitHub's published host keys, pinned in the script. Mirrors are
read-only: when a mirror's `main` has commits that are not in the new split, the
release fails instead of overwriting them. To test the script without GitHub,
point `MIRROR_URL_BASE` at a directory of bare repositories
(`MIRROR_URL_BASE=/tmp/mirrors/` with
`/tmp/mirrors/agoodway/goodflows-{exunit,jest}-reporter.git`), as
`scripts/test-release-scripts.sh` does; `MIRROR_SOURCE_REMOTE` and
`MIRROR_SOURCE_BRANCH` (default `origin` and `main`) name the branch a tag must be
on.

To exercise packaging locally, Linux targets need Docker and produce a Debian 12
binary. macOS targets build natively:

```bash
bash scripts/package-release.sh x86_64-unknown-linux-gnu v0.5.0
```
