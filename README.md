# goodflows

A small, config-driven CI runner for local development, agent environments, and
Ampcode orbs. Define shell commands in TOML; goodflows runs ready jobs in parallel,
respects dependencies, and reports exactly what passed, failed, or was skipped.

## Install

From source (Rust **1.88 or newer**):

```bash
cargo install --git https://github.com/agoodway/goodflows.git --locked
```

Or download a release archive for Linux or macOS (x86_64 or aarch64) from
[GitHub Releases](https://github.com/agoodway/goodflows/releases):

```bash
shasum -a 256 -c goodflows-v<VERSION>-<TARGET>.tar.gz.sha256
tar -xzf goodflows-v<VERSION>-<TARGET>.tar.gz
install -m 755 goodflows ~/.local/bin/goodflows
```

Linux binaries need glibc 2.36 or newer (Debian 12, Ampcode orbs); build from
source for older glibc or musl. Jobs run through `sh` unless `shell = false`.

## Quick start

From your project's root:

```bash
goodflows init          # Detect the project and write goodflows.toml
goodflows validate      # Check the config without running anything
goodflows list          # Show jobs and dependencies
goodflows               # Run every job (same as `goodflows run`)
goodflows --jobs test   # Run test and its prerequisites
```

`init` detects Rust, Elixir, Node, and Go projects. Choose explicitly with
`--template rust|elixir|node|go|generic`, write elsewhere with `--config PATH`,
generate a named workflow with `--workflow NAME`, and overwrite without asking
with `--force` (use `--force` in scripts).

## Configuration

```toml
[settings]
parallelism = 4       # Max concurrent jobs; 0 or omitted = logical CPU count
fail_fast = true      # Stop launching jobs after the first failure (default)
workdir = "."         # Relative to the config file

[settings.env]        # Environment defaults for every job
CARGO_TERM_COLOR = "always"

[[jobs]]
name = "fmt"
command = "cargo fmt --check"

[[jobs]]
name = "test"
command = "cargo test --locked"
depends_on = ["fmt"]
timeout_secs = 300
env = { RUST_BACKTRACE = "1" }

[[jobs]]
name = "lint"
command = "cargo clippy --locked --all-targets -- -D warnings"
depends_on = ["fmt"]
```

goodflows looks for `goodflows.toml`, then `.goodflows.toml`, in the current
directory (or pass `--config`). The whole file is validated before anything runs.

- Commands run with `sh -c`. With `shell = false` they are split with shell
  quoting and executed directly, without expansion.
- Jobs start as soon as all their `depends_on` jobs pass. A failed or skipped job
  skips its dependents.
- Stdin is closed, so commands must be noninteractive.
- Environment is merged by key, lowest to highest: the caller's environment,
  `[settings.env]`, the workflow's `settings.env`, the `--env` profile's
  `settings.env`, job `env`, step `env`. Values are literal strings.

Useful flags: `--parallelism N`, `--fail-fast` / `--no-fail-fast`,
`--jobs a,b`, `--config PATH`.

| Exit code | Meaning |
|---|---|
| `0` | All selected jobs passed or were skipped |
| `1` | A job failed or timed out, or a config/runtime error |
| `2` | Invalid command-line arguments |
| `130` / `143` | Interrupted by SIGINT / SIGTERM |

## Named workflows

Keep several independently runnable job sets in one file:

```toml
[[workflows.ci.jobs]]
name = "fmt"
command = "cargo fmt --check"

[[workflows.ci.jobs]]
name = "test"
command = "cargo test --locked"
depends_on = ["fmt"]

[workflows.release.settings]
parallelism = 1

[workflows.release.settings.env]
CARGO_PROFILE_RELEASE_LTO = "true"

[[workflows.release.jobs]]
name = "build"
command = "cargo build --release --locked"
```

```bash
goodflows list --workflows     # Discover workflows
goodflows run ci               # Run one workflow
goodflows run ci --jobs test   # Run test plus fmt
goodflows validate             # Validate every workflow
```

Top-level `[[jobs]]` form the `default` workflow. Without a default, `goodflows` and
`goodflows list` ask you to name a workflow. Job names and dependencies are scoped
to their workflow. Workflow settings override `[settings]` field by field.

## Workflow calls

A job can run another workflow from the same file instead of a command or steps:

```toml
[[workflows.release.jobs]]
name = "prepare"
command = "cargo build --release --locked"

[[workflows.release.jobs]]
name = "checks"
workflow = "ci"              # Runs every job in the ci workflow
depends_on = ["prepare"]

[[workflows.release.jobs]]
name = "publish"
command = "./scripts/publish.sh"
depends_on = ["checks"]
```

- A job has exactly one of `command`, `steps`, or `workflow`. A call job supports
  `name`, `depends_on`, and `condition`. It rejects `timeout_secs`, `partitions`,
  `env`, `outputs`, `reports`, and `shell = false`, and it publishes no outputs, so
  `needs.checks.outputs...` is rejected.
- Only `depends_on` orders a call. File order adds no ordering, so a job with no
  dependency path to a call can run alongside it.
- A call waits for its workflow to finish, including that workflow's reporting.
  It passes when the workflow passes (skipped jobs inside it are fine). It fails
  when any job inside fails or times out, and it is cancelled by a signal. A failed
  call skips its dependents like any failed job. A false call condition skips the
  call without running the workflow.
- Every call is a separate run. Calling the same workflow twice runs it twice, and
  each run sees the workspace as it is then. Nothing is cached, deduplicated, or
  isolated. Called workflows share the filesystem the way concurrent jobs do, so
  order calls with `depends_on` when their side effects must not overlap.
- `--jobs` selects only in the workflow you run. `goodflows run release --jobs
  publish` runs `prepare`, `checks`, and `publish`, and `checks` still runs all of
  `ci`.
- Targets and call cycles are checked across the whole file before anything runs,
  including unselected workflows and calls behind false conditions. A workflow
  that calls itself is a cycle. A named workflow may call `default`, the root jobs.
  There is no nesting limit beyond rejecting cycles.

**Settings.** A called workflow resolves its own settings exactly as
`goodflows run <workflow>` would: `[settings]`, then its own workflow settings, then
the `--env` profile and CLI flags, merged per key. A setting the called workflow
omits comes from `[settings]`, never from the caller's workflow settings. Its
workdir and env are its own. Profile `partitions` entries are global by job name:
`test = 6` applies to every reachable workflow that partitions a job named `test`.
A reachable workflow with an unpartitioned `test` makes that profile fail before
anything runs, even behind a false call condition.

**Parallelism.** The root workflow's parallelism caps running command and step jobs
across every workflow it calls. A called workflow's own (lower) parallelism caps its
part of the run too. A call waiting for its workflow holds no slot, so nested calls
finish even with `--parallelism 1`. A call's shell condition takes a slot only while
it runs. Partition instances take one slot each, and a stepped job takes one.

**Failures and signals.** A failure stops new work in every enclosing workflow that
has `fail_fast` enabled, including work inside its other calls. Running jobs
finish. A called workflow with `fail_fast = false` does not override an enabled
caller. A called workflow with `fail_fast = true` stops only its own remaining work
when the caller's is disabled. A called workflow cut short by a caller's fail-fast
shows its unstarted jobs as skipped with `fail_fast: previous job failed`. None of
its own jobs failed, so it and its call report passed, while the run whose job
failed reports failed and the root run exits 1. A call whose shell condition
finishes after such a stop is skipped with the same reason and starts no workflow.
SIGINT and SIGTERM cancel every running job at every level, start no new calls, and
exit 130 or 143.

**Output.** Each called workflow run gets its own run id, summary, log directory
(`<log-dir>/<workflow>/<run>`), history file, and reporting run. In JSON mode,
every event of a run that calls workflows carries `invocation_id`, the run id of
the workflow run it belongs to. Events from a called run also carry
`parent_run_id` and `caller_job`. A called run that fails during setup, for
example because its log directory cannot be created, has a `run_started` event but
no summary. Its call fails with the error and records the called run's
`child_run_id`. Text lines show the workflow and the first 8
characters of the run id, such as `→ ci [3f2a9c1e] test`. Each called run prints its
own report naming its caller. The root report comes last, and it is the only one
with a rerun hint. A call is one row in its caller's results and counts, with
`workflow` (the target) and `child_run_id` once started. The rerun hint reruns the
failed call through the root workflow, so its prerequisites run again first.
History records add `parent_run_id` and `caller_job` to a called run, and
`workflow` and `child_run_id` to the call. A called run's record never copies the
root `--jobs`. A call's own costs are only its shell condition. Each run's
`max_in_flight` counts command and step jobs, and call shell conditions while they
run, in that run and the workflows it calls.

## Conditional jobs

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

A condition is checked once its dependencies pass. If it is false (or the shell
condition exits nonzero), the job is skipped. Skips don't fail the run, but they do
skip dependents.

Expressions support `env.NAME`, `needs.JOB.outputs.KEY` (for jobs in
`depends_on`), and `steps.ID.outputs.KEY` (inside step conditions). They also
support `! < <= > >= == != && || ??` and parentheses. Types are strict: `"false"`
is a string, not a boolean. Use `??` for a default when a variable is unset.

## Steps and outputs

A job has either a `command` or a list of `steps`. Steps run in order in one
parallelism slot under one timeout. A command publishes outputs by writing a JSON
object to the file named in `$GOODFLOWS_OUTPUT`.

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

[[jobs]]
name = "backend-tests"
depends_on = ["detect"]
condition = { expression = "needs.detect.outputs.backend_changed" }
command = "cargo test"
```

Output values must be booleans, numbers, or strings (64 KiB max). Step outputs
stay inside the job unless the job's `outputs` mapping promotes them. Outputs are
not redacted, so don't publish secrets.

## Partitioned jobs

Split a test suite across several scheduled instances:

```toml
[settings]
parallelism = 6

[[jobs]]
name = "test"
command = "MIX_TEST_PARTITION=$GOODFLOWS_PARTITION mix test --partitions $GOODFLOWS_PARTITION_COUNT"
partitions = 3
```

Each instance gets `GOODFLOWS_PARTITION` (1-based) and `GOODFLOWS_PARTITION_COUNT`.
It takes its own parallelism slot, timeout, and logs. Dependents wait for every
instance to pass. The command must split the work itself. Partitioned jobs can't
publish outputs. Counts range from 1 to 64.

## Environment profiles

Share one config across machines and change runner settings per machine:

```toml
[settings.env]
MIX_ENV = "test"

[environments.local.settings]
parallelism = 8

[environments.local.partitions]
test = 6

[environments.orb.settings]
parallelism = 4

[environments.ci.settings]
fail_fast = false
history = true

[environments.ci.settings.env]
CI = "true"

[[jobs]]
name = "test"
command = "MIX_TEST_PARTITION=$GOODFLOWS_PARTITION mix test --partitions $GOODFLOWS_PARTITION_COUNT"
partitions = 3
```

```bash
goodflows --env local
goodflows run ci --env orb
goodflows --env ci --parallelism 2   # CLI flags still win
```

A profile can override any `settings` field, including `env`. It can also override
the count of an already-partitioned job. A profile is applied only when you pass
`--env`; goodflows never picks one automatically. Commands see the profile name as
`GOODFLOWS_ENVIRONMENT`. Profile names don't select workflows, so `--env ci`
doesn't select a workflow named `ci`.

Settings precedence: **CLI > profile > workflow > shared > defaults**.

## Output

Human output prefixes each line with its job and stream (`[test stdout] ...`). A run
ends with a report:

```text
default  FAILED  in 207ms
  PASS  fmt     203ms
  FAIL  test      3ms  2/2 failed
  SKIP  deploy         dependency did not pass
1 passed, 1 failed, 1 skipped

Failures
--- test (1/2)  FAILED  exit 1
  command: ./run-tests
  error: exited with 1
...
Rerun failed: goodflows run default --jobs='test,deploy'
```

For agents and scripts, `--json` prints JSON Lines on stdout. The final `summary`
event includes `success`, counts per status, and per-job `results`. Failed jobs
include stdout/stderr tails, which the text report leaves out because the output
already streamed live.

```bash
goodflows --json > results.jsonl
goodflows validate --json
goodflows list --json
```

To keep raw logs, pass `--log-dir .goodflows-logs`. Each run writes its own
directory, and the newest `log_runs` (default 10) are kept. Add the directory to
`.gitignore`. Logs aren't redacted.

## Run history

With `history = true` (or `--history`), each run writes a small JSON file to
`goodflows-history/<workflow>/`. The file records timing, queue wait, CPU, peak
memory, and host details per job, so you can compare tuning changes across commits.
It contains no output or env values, but it does include command text. Files are
pruned to `history_runs` (default 1000) and `history_bytes` (default 10 MiB) per
workflow. Enable it where measurements are comparable, such as a CI profile. A
clean-tree check that runs after goodflows will see the new untracked file.

To commit history, mark it generated:

```gitattributes
goodflows-history/** text eol=lf linguist-generated
```

Or keep it local by adding `goodflows-history/` to `.gitignore`.

## Test timing reports

goodflows can collect per-test timing reports (`test-timings/1` JSON Lines) and
publish them to a GoodFlows server. It's opt-in:

```toml
[reporting]
project = "my-project"           # Existing GoodFlows project slug
url_env = "GOODFLOWS_URL"        # Variable holding the server origin (default)
token_env = "GOODFLOWS_API_KEY"  # Variable holding the API key (default)

[[jobs]]
name = "test"
command = "mix test"
reports = ["test-timings"]
```

```bash
GOODFLOWS_URL=https://flows.example.com GOODFLOWS_API_KEY=... goodflows
```

Reporting jobs get `GOODFLOWS_REPORT_DIR` and write `*.jsonl` files there.
goodflows never passes the API key to jobs. Uploads are best-effort and never
change the run's result. Reports that fail to upload stay in
`.goodflows/reports/<run-id>/` (add `/.goodflows/` to `.gitignore`), and you can
retry them:

```bash
goodflows reports upload .goodflows/reports/<run-id>
```

Reporters, versioned on their own (pin the latest tag; every release writes
`test-timings/1`, which goodflows v0.6.0 and later accept):

- **ExUnit** (Elixir ~> 1.18), from
  [`agoodway/goodflows-exunit-reporter`](https://github.com/agoodway/goodflows-exunit-reporter):

  ```elixir
  # mix.exs
  {:goodflows_exunit_reporter,
   github: "agoodway/goodflows-exunit-reporter", tag: "v0.6.0", only: :test}

  # test/test_helper.exs
  ExUnit.start()
  Goodflows.ExUnitReporter.configure()
  ```

- **Jest** (Jest 29.6+, Node 18+), from
  [`agoodway/goodflows-jest-reporter`](https://github.com/agoodway/goodflows-jest-reporter):

  ```js
  // package.json devDependencies:
  //   "goodflows-jest-reporter": "github:agoodway/goodflows-jest-reporter#v0.6.0"
  // jest.config.js
  module.exports = { reporters: ['default', 'goodflows-jest-reporter'] };
  ```

Other runners can write the format directly. See the
[`test-timing-contract` spec](openspec/specs/test-timing-contract/spec.md).

### Upgrading from `exunit-timings`

Change `reports = ["exunit-timings"]` to `reports = ["test-timings"]` and replace a
copied `GC.TestTimingFormatter` with the ExUnit reporter. The suite option is now
`:test_timing_suite_id` / `TEST_TIMING_SUITE_ID`. The server must support
`test-timings/1`. Pending snapshots in the old format are never resent.

## Ampcode orbs

1. Commit `goodflows.toml` at the repository root.
2. Add an executable `.agents/setup` that installs goodflows from a release
   archive into `/usr/local/bin`, plus any toolchains your jobs need. Orbs ship
   with Git, Node.js, Python, `gh`, and `amp`.
3. Tell the agent to use it in `AGENTS.md`:

   ```markdown
   ## Checks

   Run `goodflows` from the repository root before finishing.
   `goodflows --json` prints JSON Lines; the final `summary` event reports `success`.
   Fix failures and rerun until `success` is true.
   ```

Ampcode snapshots the disk after setup succeeds. After you change
`.agents/setup`, run `amp projects snapshots delete <project>` so the next thread
picks up the change. Leave `parallelism` unset to use the orb's CPU count.

### Amp plugin

[`.amp/plugins/goodflows.ts`](.amp/plugins/goodflows.ts) adds **GoodFlows: Run** to
Amp's command palette. It runs the default workflow (or lets you pick one) and
hands any failures to the agent to fix and rerun. **GoodFlows: Cancel** stops a
run. To install it, copy the file into a project's `.amp/plugins/` or
`~/.config/amp/plugins/`, then run **plugins: reload**.

## Development and releases

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node --test tests/amp-plugin.test.mjs   # Node 22.18+, after cargo build
```

Detailed behavior is specified in [`openspec/specs/`](openspec/specs/). The release
processes for the CLI and for each reporter are in
[docs/releasing.md](docs/releasing.md).
