# goodflows

goodflows runs the jobs in a `goodflows.toml` file. A job is a shell command, a list of steps, or a call to another workflow in the same file. A job starts after every name in its `depends_on` list has passed. Ready jobs run at the same time, up to the parallelism limit. The run prints what passed, failed, timed out, or was skipped.

## Install

With Rust 1.88 or newer:

```bash
cargo install --git https://github.com/agoodway/goodflows.git --locked
```

Release archives for Linux and macOS, `x86_64` and `aarch64`, are on [GitHub Releases](https://github.com/agoodway/goodflows/releases). v0.6.0 published these names. [`.github/workflows/release.yml`](.github/workflows/release.yml) builds the same four targets:

- `goodflows-v<VERSION>-x86_64-unknown-linux-gnu.tar.gz`
- `goodflows-v<VERSION>-aarch64-unknown-linux-gnu.tar.gz`
- `goodflows-v<VERSION>-x86_64-apple-darwin.tar.gz`
- `goodflows-v<VERSION>-aarch64-apple-darwin.tar.gz`

Each archive has a `.sha256` file next to it. The release also includes `SHA256SUMS`, which is those checksum files concatenated. An archive contains `goodflows`, `README.md`, and `LICENSE` at the top level.

Run these in the directory that contains the archive and the `.sha256` file:

```bash
sha256sum -c goodflows-v0.6.0-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf goodflows-v0.6.0-x86_64-unknown-linux-gnu.tar.gz
mkdir -p ~/.local/bin
install -m 755 goodflows ~/.local/bin/goodflows
```

On macOS, `shasum -a 256 -c` checks the same `.sha256` file.

Linux archives are linked with Rust 1.88.0 in Debian 12 (`debian:bookworm-slim`). [`scripts/package-release.sh`](scripts/package-release.sh) rejects a binary whose glibc symbol versions are newer than 2.36, the glibc in Debian 12 and in Ampcode orbs. The v0.6.0 Linux archives require glibc 2.34. On both `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, the highest versioned glibc symbol is GLIBC_2.34. A later release can require a glibc up to 2.36. There is no musl archive. For musl, or for a glibc older than the archive you downloaded requires, build from source on that system:

```bash
cargo build --release --locked
install -m 755 target/release/goodflows ~/.local/bin/goodflows
```

Jobs run with `sh -c` unless the job sets `shell = false`. With `shell = false`, goodflows splits the command with shell quoting rules and executes it directly. There is no globbing and no variable expansion. Stdin is closed.

## Quick start

From the project directory:

```bash
goodflows init
goodflows validate
goodflows list
goodflows
```

`goodflows` with no subcommand is `goodflows run`. It runs the default workflow. `validate` parses the file and does not run commands. `list` prints each job, its dependencies, and the settings the run would use.

`init` writes `goodflows.toml`. It chooses a template from the files in the current directory: `Cargo.toml` (rust), `mix.exs` (elixir), `package.json` (node), or `go.mod` (go). If none of those files exist, the template is generic. If more than one exists, `init` stops and you pass `--template rust|elixir|node|go|generic`.

The generic template writes one job, `check`, whose command prints a message and exits 1. Replace that command. The rust, elixir, and go templates include a job named `test`. The elixir template sets `MIX_ENV` to `test` in `[settings.env]`. The node template reads `package.json` and adds a job for each of `format:check`, `lint`, `typecheck`, `check`, `test`, and `build` that exists. It runs them with npm, pnpm, yarn, or bun, picked from the lockfile. Conflicting lockfiles make `init` stop. With no lockfile it uses npm.

```bash
goodflows --jobs test
```

`--jobs` runs the named jobs and every job they depend on. Names are separated by commas. `--config PATH` reads or writes that file instead of `goodflows.toml`. `--workflow NAME` on `init` puts the generated jobs under `[[workflows.NAME.jobs]]`. `--force` overwrites an existing file. Without `--force`, `init` asks on a terminal and keeps the file when the answer is not `y` or `yes`.

goodflows loads `goodflows.toml`, then `.goodflows.toml`, from the current directory.

## Feature reference

### Configuration

```toml
[settings]
parallelism = 4
fail_fast = true
workdir = "."

[settings.env]
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

`parallelism` is how many command and step jobs may run at once. `0`, or leaving it out, uses the logical CPU count. `fail_fast` defaults to true. After a failure, goodflows starts no further jobs, and jobs that have already started run to completion. `workdir` is relative to the config file, must already exist, and defaults to the config file's directory.

Unknown keys are rejected. The whole file is checked before any command runs.

Environment values are literal strings. Layers merge by key, and a later layer replaces the same key: the calling process, `[settings.env]`, the workflow's `settings.env`, the selected profile's `settings.env`, the job's `env`, then the step's `env`.

goodflows sets `GOODFLOWS_RUN_ID` itself. A command that may publish outputs also receives `GOODFLOWS_OUTPUT`. A selected profile sets `GOODFLOWS_ENVIRONMENT` to the profile name. Partitioned jobs receive `GOODFLOWS_PARTITION` (1-based) and `GOODFLOWS_PARTITION_COUNT`. Reporting jobs receive `GOODFLOWS_REPORT_DIR`. Config must not set those names.

```bash
goodflows --parallelism 2 --fail-fast
goodflows --no-fail-fast --jobs fmt,test
goodflows --config ./goodflows.toml
```

| Exit code | Meaning |
|---|---|
| 0 | Every selected job passed or was skipped |
| 1 | A job failed or timed out, or a config or runtime error |
| 2 | The arguments did not parse |
| 130 | Interrupted by SIGINT |
| 143 | Interrupted by SIGTERM |

### Named workflows

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
goodflows list --workflows
goodflows run ci
goodflows run ci --jobs test
goodflows validate
goodflows validate ci --jobs test
```

Top-level `[[jobs]]` are the `default` workflow. You can also declare `[workflows.default]`. If the file has no default, `goodflows` and `goodflows list` exit 1 and name the workflows you can pass. When the file has a `[workflows]` table, `goodflows validate` with no name checks every workflow and rejects `--jobs`. `goodflows list --workflows` prints the workflows and the default name. It rejects a workflow argument and `--jobs`.

Job names and `depends_on` belong to one workflow. A workflow's settings replace `[settings]` one field at a time. A field the workflow omits keeps the shared value. `settings.env` still merges by key.

### Workflow calls

A job can run another workflow from the same file. Set `workflow` instead of `command` or `steps`.

```toml
[[workflows.ci.jobs]]
name = "fmt"
command = "cargo fmt --check"

[[workflows.ci.jobs]]
name = "test"
command = "cargo test --locked"
depends_on = ["fmt"]

[[workflows.release.jobs]]
name = "prepare"
command = "cargo build --release --locked"

[[workflows.release.jobs]]
name = "checks"
workflow = "ci"
depends_on = ["prepare"]

[[workflows.release.jobs]]
name = "publish"
command = "./scripts/publish.sh"
depends_on = ["checks"]
```

```bash
goodflows run release --jobs publish
```

That command runs `prepare`, `checks`, and `publish`. The `checks` call runs every job in `ci`. `--jobs` filters only the workflow you named.

A call job accepts `name`, `depends_on`, and `condition`. It rejects `timeout_secs`, `partitions`, `env`, `outputs`, `reports`, and `shell = false`. It publishes no outputs, so `needs.checks.outputs...` is rejected. A job has one of `command`, nonempty `steps`, or `workflow`.

`depends_on` is the only ordering. A job with no dependency path to a call can run while that call runs. The call waits until the called workflow finishes, including that workflow's reporting. The call passes when the called workflow passes. Jobs skipped inside it by a condition or a dependency do not fail the call. The call fails when a job inside it fails or times out. A false condition skips the call and does not start the workflow. A failed or skipped call skips its own dependents.

Each call is its own run. Calling `ci` twice runs `ci` twice. Nothing is cached. Called workflows share the working tree, so give them `depends_on` when their writes must not overlap.

Cycles are rejected before anything runs. That check includes workflows you did not select and calls behind a false condition. A workflow that calls itself is a cycle. A named workflow may call `default`.

The called workflow resolves settings the same way `goodflows run <workflow>` does: `[settings]`, then its own workflow settings, then the `--env` profile and the CLI flags. It does not inherit settings that exist only on the caller. The root parallelism limit counts command and step jobs in the caller and in every workflow it calls. A called workflow can set a lower limit for its own jobs. A call that is waiting does not use a slot.

When the run includes a call, live lines prefix the job with the workflow name and the first eight characters of that run's id. A called run's report names the call job and the parent run id. The root report is last. It is the only report with a rerun line.

Fail-fast, cancellation, JSON fields (`invocation_id`, `parent_run_id`, `caller_job`, `child_run_id`), log paths, and history records are specified in [`openspec/specs/workflow-calls/spec.md`](openspec/specs/workflow-calls/spec.md).

### Conditional jobs

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

A condition is checked after the job's dependencies pass. An expression must evaluate to a boolean. `false` skips the job with `condition expression was false`. A non-boolean result fails the job. There is no implicit truthiness, so the string `"false"` is not a boolean. A shell condition that exits nonzero skips the job. A skip does not fail the run. Jobs that depend on the skipped job are skipped.

Expressions can read `env.NAME`, `needs.JOB.outputs.KEY` for a job listed in `depends_on`, and `steps.ID.outputs.KEY` in a later step of the same job. `??` replaces an unset value. Operators are `!`, `<`, `<=`, `>`, `>=`, `==`, `!=`, `&&`, `||`, and parentheses. `&&` and `||` evaluate the right side only when the left side does not decide the result.

### Steps and outputs

A job has a `command` or a nonempty `steps` list, not both. Steps run in order in one parallelism slot, under the job's `timeout_secs`.

A command publishes outputs by writing one JSON object to the path in `GOODFLOWS_OUTPUT`. The file must be at most 64 KiB. Each value must be a boolean, a number, or a string. An empty file publishes nothing. A command job must not set `outputs`. The JSON object's keys are that job's outputs.

A step job publishes only the keys in its `outputs` map. The map's values are expressions, usually `steps.ID.outputs.KEY`. Step outputs stay inside the job until that map promotes them. Partitioned jobs cannot publish job outputs. Output values are not redacted.

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

`backend_changed` is a JSON boolean here. The condition reads that boolean.

### Partitioned jobs

```toml
[settings]
parallelism = 6

[[jobs]]
name = "test"
command = "MIX_TEST_PARTITION=$GOODFLOWS_PARTITION mix test --partitions $GOODFLOWS_PARTITION_COUNT"
partitions = 3
```

`partitions` is an integer from 1 to 64, or `{ count = N, strategy = "index" }`. `index` is the only strategy. goodflows starts one instance per count. Each instance has its own slot, timeout, and logs. `GOODFLOWS_PARTITION` is 1-based. The command has to split the work. Dependents wait until every instance has passed. The full rules are in [`openspec/specs/job-partitions/spec.md`](openspec/specs/job-partitions/spec.md).

### Environment profiles

A profile changes settings, and the partition count of jobs that are already partitioned, when you pass `--env NAME`. goodflows never selects a profile on its own. The profile name is not a workflow name. `goodflows --env ci` does not run a workflow named `ci`.

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
goodflows run --env orb
goodflows --env ci --parallelism 2
```

Those three commands match the file above, which has a default workflow and no workflow named `ci`. A workflow name and a profile name are separate. On a file that has a `release` workflow, `goodflows run release --env ci` runs that workflow with the `ci` profile.

`test = 6` replaces the declared count of every partitioned job named `test`. A reachable workflow that has an unpartitioned job named `test` makes that `--env` fail before any job starts, including a call hidden by a false condition. A name the workflow does not use is ignored.

Settings precedence is CLI flags, then the profile, then the workflow, then `[settings]`, then the built-in defaults. The same order merges `env` by key. CLI flags do not include an env map. Profile fields follow the same rules as [`openspec/specs/settings-environment/spec.md`](openspec/specs/settings-environment/spec.md).

### Text and JSON output

Text mode writes each output chunk as `[job stream]` followed by the chunk. `stream` is `stdout` or `stderr`. A step is `[job/step stream]`. The run ends with a report: one row per job, then counts, then a `Failures` section when something failed or timed out. The text report does not repeat output tails. The last line of a failed root run is `Rerun failed:` and a `goodflows run` command for the failed jobs and the jobs skipped because a dependency did not pass.

`--json` prints JSON Lines on stdout. Every event has `schema_version` 1. The last event of a finished run has `"type":"summary"`. Its `success` field is true when every selected job passed or was skipped. Failed and timed-out jobs include `stdout_tail` and `stderr_tail`. The report shape is specified in [`openspec/specs/run-report/spec.md`](openspec/specs/run-report/spec.md).

```bash
goodflows --json > results.jsonl
goodflows validate --json
goodflows list --json
```

`--log-dir .goodflows-logs` stores raw stdout and stderr for each run in a new directory under that path. `log_runs` (default 10) is how many of those directories to keep per workflow. The files are not redacted. Add the directory to `.gitignore`. See [`openspec/specs/run-logs/spec.md`](openspec/specs/run-logs/spec.md).

```bash
goodflows --log-dir .goodflows-logs --log-runs 5
```

### Run history

`history = true`, or `--history`, writes one JSON file per run under `goodflows-history/<workflow>/` next to the config file. `history_dir` in the config is relative to the config file. `--history-dir DIR` is relative to the current directory, and it turns history on. `--no-history` turns it off for that run. The file has timing, queue wait, CPU, peak memory, and host details for each job. It has command text. It has no output and no environment values. `--history-runs N` (default 1000) and `--history-bytes N` (default 10485760) cap what is kept per workflow.

```bash
goodflows --history
goodflows --history-dir /tmp/goodflows-history --history-runs 100 --history-bytes 1048576
```

Turn history on where the machine stays comparable from run to run, such as the `ci` profile above. A clean-tree check that runs after goodflows will see the new file. To commit the files, mark them generated:

```gitattributes
goodflows-history/** text eol=lf linguist-generated
```

Or add `goodflows-history/` to `.gitignore`. The record layout is in [`openspec/specs/run-history/spec.md`](openspec/specs/run-history/spec.md).

### Test timing reports

goodflows can collect `test-timings/1` JSON Lines and upload them to a GoodFlows server. Nothing is uploaded unless you add `[reporting]` and a job lists `reports`.

```toml
[reporting]
project = "my-project"
url_env = "GOODFLOWS_URL"
token_env = "GOODFLOWS_API_KEY"

[[jobs]]
name = "test"
command = "mix test"
reports = ["test-timings"]
```

`project` is an existing project slug. `url_env` and `token_env` default to `GOODFLOWS_URL` and `GOODFLOWS_API_KEY`. Put the server origin and the API key in those variables. Do not write the URL or the key into the file. Jobs never receive the API key. They write `*.jsonl` files in `GOODFLOWS_REPORT_DIR`.

```bash
GOODFLOWS_URL=https://flows.example.com GOODFLOWS_API_KEY=secret goodflows
```

An upload failure does not change the run's exit code. Snapshots that did not upload stay in `.goodflows/reports/<run-id>/`, which contains `manifest.json`. Add `/.goodflows/` to `.gitignore`. Retry with the run directory:

```bash
goodflows reports upload .goodflows/reports/<run-id>
goodflows reports upload .goodflows/reports/<run-id> --budget 120
```

`--budget` is the publishing time limit in seconds, from 1 to 3600. The default is 30.

The wire format is [`openspec/specs/test-timing-contract/spec.md`](openspec/specs/test-timing-contract/spec.md). Upload and spool behavior are in [`openspec/specs/test-timing-publishing/spec.md`](openspec/specs/test-timing-publishing/spec.md). Reporter packages are in [`openspec/specs/test-reporters/spec.md`](openspec/specs/test-reporters/spec.md).

Reporters are versioned on their own. Pin a tag. Every release writes `test-timings/1`, which goodflows v0.6.0 and later accept. v0.6.0 is the tag in this repository.

ExUnit (Elixir 1.18 or newer), from [agoodway/goodflows-exunit-reporter](https://github.com/agoodway/goodflows-exunit-reporter):

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

Jest (Jest 29.6 or newer, Node.js 18 or newer), from [agoodway/goodflows-jest-reporter](https://github.com/agoodway/goodflows-jest-reporter):

```js
// package.json devDependencies:
//   "goodflows-jest-reporter": "github:agoodway/goodflows-jest-reporter#v0.6.0"
// jest.config.js
module.exports = { reporters: ['default', 'goodflows-jest-reporter'] };
```

Other runners can write `test-timings/1` themselves. The suite id for the ExUnit reporter is the `:test_timing_suite_id` option, or `TEST_TIMING_SUITE_ID`.

#### Upgrading from `exunit-timings`

Change `reports = ["exunit-timings"]` to `reports = ["test-timings"]` and replace a copied `GC.TestTimingFormatter` with `Goodflows.ExUnitReporter`. The server must accept `test-timings/1`. Snapshots already stored in the old format are not uploaded.

### Sprites

`goodflows run --sprite <identity>` runs one workflow on a [Sprite](https://sprites.dev) kept for that caller identity. The run sends a verified snapshot of the Git working tree that contains the config, runs your `setup` or `resume` command, then runs the same workflow with the same goodflows version. A run without `--sprite` stays on this machine and does not call the Sprite CLI. `--env` still selects a profile.

```toml
[settings]
sprite_prefix = "myapp"

[environments.ci.settings]
parallelism = 2

[sprite]
setup = "bash scripts/sprite-setup.sh"
resume = "bash scripts/sprite-resume.sh"
pass_env = ["DATABASE_URL"]

[[jobs]]
name = "test"
command = "cargo test --locked"

[[workflows.ci.jobs]]
name = "test"
command = "cargo test --locked"
```

`sprite_prefix` is 1 to 20 characters: lowercase ASCII letters, digits, or hyphens, starting with a letter or digit. Set it only in the top-level `[settings]`. `[sprite]` is top-level only. `setup` and `resume` are required for a remote run. They are nonempty shell commands. `true` is a deliberate no-op. goodflows does not look for `.agents/setup` and does not install toolchains for you. `pass_env` lists caller variables to forward. A name that is unset fails before anything remote runs. An empty value counts as set. The default is to forward nothing.

`SPRITE_TOKEN`, `SPRITES_TOKEN`, and the reporting API key stay on the controller. `pass_env` cannot name them.

```bash
goodflows run --sprite "$THREAD_ID"
goodflows run ci --sprite "$THREAD_ID" --env ci --jobs test
goodflows sprite status "$THREAD_ID"
goodflows sprite logs "$THREAD_ID" --run <uuid>
goodflows sprite collect "$THREAD_ID" --run <uuid>
goodflows sprite cancel "$THREAD_ID" --run <uuid>
goodflows sprite destroy "$THREAD_ID" --yes
```

The identity is 1 to 256 UTF-8 bytes, case-sensitive, with no control characters. It is not the Sprite's name. The Sprite is named `<sprite_prefix>-` plus 32 hex characters derived from the prefix and the identity. `--run` is a lowercase canonical UUID.

`status`, `logs`, `collect`, `cancel`, and `destroy` accept `--config` and `--json`. They reject run flags such as `--env`, `--jobs`, `--parallelism`, `--log-dir`, and the history flags. `destroy` asks on a terminal. `--yes` skips the question, and a non-interactive caller must pass it. `destroy` refuses while a run is still active. `collect` writes a bundle under `.goodflows/sprites/<scope-digest>/<run>` in the current directory. The digest covers the organization, prefix, and identity. `--output DIR` uses that directory instead, relative to the current directory.

You need the Sprite CLI on `PATH`, a Git repository, and an organization (`SPRITE_ORG`, or the current org from `sprite org list`). A controller that is not a matching Linux build downloads the checksummed Linux release archive for this exact version. What the snapshot includes, what it drops, and how recovery works are in [docs/sprites.md](docs/sprites.md). Cold boot of a Sprite is not a supported recovery path. The execution contract is [`openspec/specs/sprite-execution/spec.md`](openspec/specs/sprite-execution/spec.md). Ownership, preparation, and exclusive use are [`openspec/specs/sprite-environments/spec.md`](openspec/specs/sprite-environments/spec.md).

[examples/sprites/goodflows.toml](examples/sprites/goodflows.toml) is one remote config for this repository. Its `setup` and `resume` scripts live in `scripts/`.

### Ampcode orbs

Commit `goodflows.toml` at the repository root. Add an executable `.agents/setup` that installs a release archive into `/usr/local/bin`, plus any toolchains the jobs need. Orbs already provide Git, Node.js, Python, `gh`, and `amp`. Tell the agent to run it from `AGENTS.md`:

```markdown
## Checks

Run `goodflows` from the repository root before finishing.
`goodflows --json` prints JSON Lines. The final `summary` event has `success`.
Fix failures and rerun until `success` is true.
```

Ampcode snapshots the disk after setup succeeds. After you change `.agents/setup`, run `amp projects snapshots delete <project>` so the next thread installs again. Leave `parallelism` unset to use the orb's CPU count. The Linux release binary is linked for Debian 12's glibc, which the orbs provide.

[`.amp/plugins/goodflows.ts`](.amp/plugins/goodflows.ts) registers two palette commands, GoodFlows: Run and GoodFlows: Cancel. Run executes the default workflow, or asks you to pick one, and sends failures to the agent. Cancel stops the goodflows process. Copy the file to a project's `.amp/plugins/` or to `~/.config/amp/plugins/`, then run plugins: reload.

## Development

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node --test tests/amp-plugin.test.mjs
```

The Node test needs Node.js 22.18 or newer, and `cargo build` first, because it runs `target/debug/goodflows`. CI on this repository also runs `./target/release/goodflows`, which executes `goodflows.toml`.

Behavior that this file does not spell out is in [`openspec/specs/`](openspec/specs/). Release steps for the CLI and for each reporter are in [docs/releasing.md](docs/releasing.md).
