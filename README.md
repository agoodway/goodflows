# goodflows

goodflows runs the jobs in a `goodflows.toml` file. A job is a shell command, a list of steps, or a call to another workflow in the same file. A job starts after every name in its `depends_on` list has passed. Ready jobs run at the same time, up to the parallelism limit. The run prints what passed, failed, timed out, or was skipped.

## Install

With Rust 1.88 or newer:

```bash
cargo install --git https://github.com/agoodway/goodflows.git --locked
```

Release archives for Linux and macOS, `x86_64` and `aarch64`, are on [GitHub Releases](https://github.com/agoodway/goodflows/releases). v0.7.0 publishes these names. [`.github/workflows/release.yml`](.github/workflows/release.yml) builds the same four targets:

- `goodflows-v<VERSION>-x86_64-unknown-linux-gnu.tar.gz`
- `goodflows-v<VERSION>-aarch64-unknown-linux-gnu.tar.gz`
- `goodflows-v<VERSION>-x86_64-apple-darwin.tar.gz`
- `goodflows-v<VERSION>-aarch64-apple-darwin.tar.gz`

Each archive has a `.sha256` file next to it. The release also includes `SHA256SUMS`, which is those checksum files concatenated. An archive contains `goodflows`, `README.md`, and `LICENSE` at the top level.

Run these in the directory that contains the archive and the `.sha256` file:

```bash
sha256sum -c goodflows-v0.7.0-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf goodflows-v0.7.0-x86_64-unknown-linux-gnu.tar.gz
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

Unknown keys are rejected. The whole file is checked before any command runs. `goodflows validate` reports every problem it finds in one pass. A file that is not valid TOML, or that has an unknown key, reports only that first error, because parsing stops there.

Environment values are literal strings. Layers merge by key, and a later layer replaces the same key: the calling process, `[settings.env]`, the workflow's `settings.env`, the selected profile's `settings.env`, the job's `env`, then the step's `env`.

goodflows sets `GOODFLOWS_RUN_ID` itself. A command that may publish outputs also receives `GOODFLOWS_OUTPUT`. A selected profile sets `GOODFLOWS_ENVIRONMENT` to the profile name. Partitioned jobs receive `GOODFLOWS_PARTITION` (1-based) and `GOODFLOWS_PARTITION_COUNT`. Reporting jobs receive `GOODFLOWS_REPORT_DIR`. Config must not set those names.

```bash
goodflows --parallelism 2 --fail-fast
goodflows --no-fail-fast --jobs fmt,test
goodflows --config ./goodflows.toml
```

| Exit code | Meaning | What to do |
|---|---|---|
| 0 | Every selected job passed or was skipped | Nothing |
| 1 | A job failed or timed out, or goodflows hit a runtime error after the command and config were accepted | Fix the code, or report the failure |
| 2 | A usage or config error: bad arguments, no config, a config that does not parse or validate, or an unknown workflow, job, or environment | Fix the command or the config, then run it again |
| 75 | Only from [`wait`](#detached-runs): the timeout passed and the run is still going | Run `wait` again |
| 130 | Interrupted by SIGINT | |
| 143 | Interrupted by SIGTERM | |

With `--json`, a run that cannot start ends with one `error` event. Its `code` is stable, so a script can branch on it instead of the message:

| `code` | Exit | Meaning |
|---|---|---|
| `usage` | 2 | The arguments did not parse, or combine flags that do not go together |
| `missing_config` | 2 | No `goodflows.toml` or `.goodflows.toml`, or the `--config` file cannot be read |
| `config_parse` | 2 | The file is not valid TOML, or has an unknown key or a wrong type |
| `config_invalid` | 2 | The file parsed but failed validation; `problems` lists every problem |
| `no_default_workflow` | 2 | The file has no default workflow and the command named none |
| `unknown_workflow` | 2 | The workflow name is not in the file |
| `unknown_job` | 2 | A `--jobs` name is not in the selected workflow |
| `unknown_environment` | 2 | The `--env` name is not in the file |
| `no_last_run` | 2 | [`last` or `run --failed`](#last-run-and-rerunning-failures) found no previous local run, or `run --failed` named a different workflow or environment |
| `no_failed_jobs` | 2 | `run --failed` found nothing to rerun: the last run had no failed or timed-out job |
| `unknown_run` | 2 | [`status` or `wait`](#detached-runs) found no detached run with that id for the config |
| `runtime` | 1 | Anything else that stopped goodflows, including `history` and `reports upload` request errors and the `--sprite` and `sprite` command checks |

`message` is the same text that text mode prints after `Error:`. An unknown workflow, job, or environment message lists the names that exist. An optional `hint` suggests a fix, such as `did you mean 'test'?` for a close match. Text mode prints it on a `Hint:` line.

```json
{"type":"error","code":"unknown_job","message":"unknown job 'tset' (available: fmt, test)","hint":"did you mean 'test'?","workflow":"default","schema_version":1}
```

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

Top-level `[[jobs]]` are the `default` workflow. You can also declare `[workflows.default]`. If the file has no default, `goodflows` and `goodflows list` exit 2 and name the workflows you can pass. When the file has a `[workflows]` table, `goodflows validate` with no name checks every workflow and rejects `--jobs`. `goodflows list --workflows` prints the workflows and the default name. It rejects a workflow argument and `--jobs`.

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

Call-aware JSON events carry `invocation_id`, `parent_run_id`, `caller_job`, and `child_run_id`. Fail-fast, cancellation, log paths, and history records for calls are pinned by [`tests/workflows.rs`](tests/workflows.rs).

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

`partitions` is an integer from 1 to 64, or `{ count = N, strategy = "index" }`. `index` is the only strategy. goodflows starts one instance per count. Each instance has its own slot, timeout, and logs. `GOODFLOWS_PARTITION` is 1-based. The command has to split the work. Dependents wait until every instance has passed.

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

Settings precedence is CLI flags, then the profile, then the workflow, then `[settings]`, then the built-in defaults. The same order merges `env` by key. CLI flags do not include an env map. A profile's `settings` take the same fields as a workflow's. `env` values are literal strings with no interpolation, and an empty string sets the variable to empty.

### Text and JSON output

`--output` picks how much a run prints in text mode. `--output=failures` prints one status line per job and no job output while the run goes. `--output=stream` also prints step lines, and writes each output chunk as `[job stream]` followed by the chunk. `stream` is `stdout` or `stderr`. A step is `[job/step stream]`. Without the flag, a run uses `failures` when stdout is not a terminal, such as when it is piped to a file, or when a coding agent variable is set (`CLAUDECODE`, `CODEX_*` other than `CODEX_HOME`, `AGENT`, `AI_AGENT`, `CURSOR_AGENT`, `GEMINI_CLI`, or `OPENCODE`). Otherwise it uses `stream`. `--json` ignores `--output`.

The run ends with a report: one row per job, then counts, then a `Failures` section when something failed or timed out. In both modes each `Failures` entry shows the last 4 KiB of the instance's stdout and stderr, and the paths of its log files when `--log-dir` is set. The last line of a failed root run is `Rerun failed:` and a `goodflows run` command for the failed jobs and the jobs skipped because a dependency did not pass.

`--json` prints JSON Lines on stdout. Every event has `schema_version` 1. The last event of a finished run has `"type":"summary"`. Its `success` field is true when every selected job passed or was skipped. `--json=summary` leaves out the events in between, such as one `job_output` event per output chunk. A run then prints only `run_started`, any `error` event, and the `summary`, plus `sprite_unresolved` when a Sprite run cannot report one. Other commands print the same events as with `--json`.

Each failed or timed-out job in the summary has:

- `output_head` and `output_tail`: the first and last 4 KiB of its stdout and stderr together, in the order goodflows read them. `output_truncated` is true when output between them was left out.
- `stdout_tail` and `stderr_tail`: the last 4 KiB of each stream. `stdout_truncated` and `stderr_truncated` say whether earlier output was left out.

With `--log-dir`, each job that ran has `stdout_log` and `stderr_log`, the paths of its log files. A failed root run's summary has `rerun_command`, the command on the report's `Rerun failed:` line, or on its `Rerun workflow:` line when a job name contains a comma.

```bash
goodflows --json=summary > results.jsonl
goodflows --json > events.jsonl
goodflows validate --json
goodflows list --json
```

`--log-dir .goodflows-logs` stores raw stdout and stderr for each run in a new directory under that path. `log_runs` (default 10) is how many of those directories to keep per workflow. The files are not redacted. Add the directory to `.gitignore`.

```bash
goodflows --log-dir .goodflows-logs --log-runs 5
```

### Last run and rerunning failures

Every local run replaces one record of its results at `.goodflows/last/<config file name>.json`, next to the config file. It is written whether or not history is on. It keeps each invocation's `summary`, including the output tails of failed jobs and the `--log-dir` path. Add `/.goodflows/` to `.gitignore`.

```bash
goodflows last
goodflows last --json
goodflows run --failed
```

`goodflows last` prints the last run's report again without running anything: each job's status, the Failures section with the command, error, log file paths, and output tails, and the `Rerun failed:` hint. A run that called workflows shows each called workflow's report before the root's, as the run did. With `--json`, it emits the run's `summary` events unchanged. The log files exist only if that run used `--log-dir`, and only until `log_runs` prunes them.

`goodflows run --failed` reruns the jobs that the last run's `Rerun failed:` hint names, plus the jobs they depend on. It uses the last run's workflow and `--env`. Those are the failed and timed-out jobs and the jobs skipped because a dependency did not pass. A workflow or `--env` given with `--failed` must match the last run's. `--failed` rejects `--jobs` and `--sprite`. Other run flags, such as `--log-dir` and `--history`, apply as usual. A job whose name contains a comma is rerun too, though the hint cannot select it.

When there is no record, both commands exit 2 with `no_last_run`. `run --failed` exits 2 with `no_failed_jobs` when the last run has nothing to rerun. A run that stops before its summary, such as one with a config error, leaves the previous record in place. `run --sprite` writes no record.

### Detached runs

Coding agents often stop a shell command after a couple of minutes, and a full run can take longer. `goodflows run --detach` starts the run in the background and returns at once. It prints the run id on stdout. With `--json` or `--json=summary`, it prints one `run_detached` event with `run_id`, `pid`, and `run_dir`. The background run always records every event, so `status` can show its progress. The run id is the `run_id` of the run's root `summary`.

```bash
id=$(goodflows run --detach)
goodflows status "$id"
goodflows wait "$id" --timeout 90
```

`--detach` checks the command line and the config first, so usage and config errors still exit 2 before anything starts. It takes the same run flags as a foreground run, except `--sprite`.

`goodflows status <run_id>` prints the run's state and which jobs are running, passed, failed, timed out, skipped, or cancelled. With `--json` it prints one `run_status` event with `state` (`running`, `finished`, or `lost`), `exit_code` once finished, and a list of job names for each status. `lost` means the background process died without a result, for example from SIGKILL. `status` exits 0 whenever it can report.

`goodflows wait <run_id> --timeout <secs>` blocks until the run finishes. It then prints the run's report, or its `summary` events with `--json`, exactly as the foreground run would have, and exits with the run's exit code. If the timeout passes first, it prints the same progress as `status` and exits 75. Call `wait` again until the exit code is not 75. Without `--timeout` it waits as long as the run takes. A `lost` run exits 1. Stopping `wait` does not stop the run. To cancel a detached run, send its process SIGTERM; the pid is in `run_dir/pid`.

`status` and `wait` find the run beside the config, so pass the same `--config` the run used. Each detached run keeps its events, stderr, record, and exit code in `.goodflows/runs/<run_id>/` next to the config file. Nothing removes these directories, so delete old ones when you no longer need them. A detached run also replaces the [last run](#last-run-and-rerunning-failures) record when it finishes.

### Run history

`history = true`, or `--history`, writes one JSON file per run under `goodflows-history/<workflow>/` next to the config file. `history_dir` in the config is relative to the config file. `--history-dir DIR` is relative to the current directory, and it turns history on. `--no-history` turns the file off for that run. The file has timing, queue wait, CPU, peak memory, and host details for each job. It has command text. It has no output and no environment values. `--history-runs N` (default 1000) and `--history-bytes N` (default 10485760) cap what is kept per workflow.

```bash
goodflows --history
goodflows --history-dir /tmp/goodflows-history --history-runs 100 --history-bytes 1048576
```

Turn history on where the machine stays comparable from run to run, such as the `ci` profile above. A clean-tree check that runs after goodflows will see the new file. To commit the files, mark them generated:

```gitattributes
goodflows-history/** text eol=lf linguist-generated
```

Or add `goodflows-history/` to `.gitignore`. The record layout is defined in [`src/history.rs`](src/history.rs).

#### Uploading run history

`history = true` inside the [`[reporting]`](#test-timing-reports) block uploads a run record to the GoodFlows server for every invocation: the root run and each called workflow, each under its own run id. No job has to list `reports`. The default is `false`.

```toml
[reporting]
project = "my-project"
history = true
```

The upload and the local file are separate settings. The `history` setting in `[settings]`, a workflow, or a profile, and `--history`, `--no-history`, and `--history-dir`, control only the file. A run with `--no-history` writes no file and still uploads.

The record holds the same fields the history file would hold: outcome, timings, host, `source.commit` and `dirty`, and for each job its command text, exit code, and error. It adds three keys that the file never has:

- `source.branch`, when git reports a branch. A detached checkout, such as a GitHub Actions pull request, has none.
- `ci`, when the caller's environment has `GITHUB_ACTIONS=true`. It is built from `GITHUB_EVENT_NAME`, `GITHUB_RUN_ID`, `GITHUB_RUN_ATTEMPT`, `GITHUB_REF`, `RUNNER_NAME`, `GITHUB_SERVER_URL`, and `GITHUB_REPOSITORY`. A value that fails its rule is left out. Settings and job `env` never supply these values.
- `graph`, the job graph that `graph_sha256` hashes. It lists every selected job with its command, step commands, and condition text, each cut to 1,024 bytes. That includes jobs that were skipped or never started, so the server receives command text for jobs that did not run. A workflow-call job names its target and the target's hash. The target's jobs are in the called workflow's own record.

The key and URL variables must be set when the run starts. When either is unset, the run prints `run history for <workflow> upload skipped: credentials unavailable`, keeps nothing, and sends nothing. Leave the key unset on machines that should not upload, such as laptops. A URL that is not a valid endpoint gives `invalid url` instead. A record that breaks the server's rules, such as a job name longer than 64 bytes, is not sent: the line ends `upload skipped: invalid record at <path>`, where `<path>` is a field path such as `results[3].name`.

The upload runs after the test reports, before the final report, and uses what is left of the same 30-second budget. It never changes the exit code. A record that was not stored stays in the spool, and [`reports upload`](#test-timing-reports) sends it again. That includes a record refused with HTTP 422 by a server that does not accept it yet.

`history list` and `history show` read stored runs with the same `[reporting]` project, URL, and key. Text the server redacts for a `pk_` key, such as commands and errors, prints as `-`.

```bash
goodflows history list
goodflows history list --workflow ci --branch main
goodflows history list --outcome failed --limit 20 --json
goodflows history show <run-id>
```

`list` prints one row per run, newest first: start, outcome, workflow, environment, commit (12 characters, `*` when dirty), branch, CI event, duration, and run id. When there is another page it ends with `next: --cursor <cursor>`. The filters are `--workflow`, `--environment`, `--outcome` (`passed`, `failed`, or `cancelled`), `--commit` (a full hash), `--branch`, `--event`, `--graph-sha256`, `--limit` (1 to 200), and `--cursor`. `--branch` and `--event` match `source.branch` and `ci.event` exactly. A run uploaded without a branch or CI context prints `-` in the BRANCH or EVENT column and does not match those filters. `show` prints the run's fields, one row per result, and its linked test reports. A bad filter or run id exits 2 without a request. A missing `[reporting]` block, key, or URL, a server error, or an unknown run exits 1. `--json` prints one `history_runs` or `history_run` event. The commands reject run flags such as `--env`, `--jobs`, and the history file flags, and exit 2.

### Test timing reports

goodflows can collect `test-timings/1` JSON Lines and upload them to a GoodFlows server. Nothing is uploaded unless you add `[reporting]` and a job lists `reports`, or the block sets [`history = true`](#uploading-run-history).

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

With [`[reporting] history = true`](#uploading-run-history), and the key and URL set, each invocation's run directory is created even when no job lists `reports`. It holds `run-record.json`, the exact run record to send, and the manifest is version 3. Without a run record the manifest is version 1, or version 2 when a job is partitioned. `reports upload` sends the reports first, then the run record within the same budget. When the server stores the record, it writes `run-record.ack.json` and later replays send nothing for it. HTTP 400, 409, and 415 write `run-record.rejected.json` and the record is not sent again. HTTP 413 and 422 leave it pending, so the next `reports upload` sends it again. `reports upload` exits nonzero while the record is pending, rejected, or invalid, and its text output ends with `Run history: acknowledged`, `pending`, `rejected`, or `invalid`. A tampered, missing, or symlinked `run-record.json`, or one whose run id, workflow, or environment differs from the manifest, counts as invalid. Older goodflows releases refuse a version 3 manifest as an unsupported version.

The wire format is defined in [`src/reporting/contract.rs`](src/reporting/contract.rs). Each reporter's setup and behavior are in its README: [ExUnit](reporters/exunit/README.md) and [Jest](reporters/jest/README.md).

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

You need the Sprite CLI on `PATH`, a Git repository, and an organization (`SPRITE_ORG`, or the current org from `sprite org list`). A controller that is not a matching Linux build downloads the checksummed Linux release archive for this exact version. What the snapshot includes, what it drops, and how recovery works are in [docs/sprites.md](docs/sprites.md). The same document covers ownership, preparation, exclusive use, and execution. Cold boot of a Sprite is not a supported recovery path.

[examples/sprites/goodflows.toml](examples/sprites/goodflows.toml) is one remote config for this repository. Its `setup` and `resume` scripts live in `scripts/`.

### Ampcode orbs

Commit `goodflows.toml` at the repository root. Add an executable `.agents/setup` that installs a release archive into `/usr/local/bin`, plus any toolchains the jobs need. Orbs already provide Git, Node.js, Python, `gh`, and `amp`. Tell the agent to run it from `AGENTS.md`:

```markdown
## Checks

Run `goodflows` from the repository root before finishing.
`goodflows --json=summary` prints JSON Lines. The final `summary` event has `success`.
Fix failures and rerun until `success` is true.
```

Ampcode snapshots the disk after setup succeeds. After you change `.agents/setup`, run `amp projects snapshots delete <project>` so the next thread installs again. Leave `parallelism` unset to use the orb's CPU count. The Linux release binary is linked for Debian 12's glibc, which the orbs provide.

[`.amp/plugins/goodflows.ts`](.amp/plugins/goodflows.ts) registers two palette commands, GoodFlows: Run and GoodFlows: Cancel. Run executes the default workflow, or asks you to pick one, and sends failures to the agent. Cancel stops the goodflows process. Copy the file to a project's `.amp/plugins/` or to `~/.config/amp/plugins/`, then run plugins: reload.

## Agent skills

[`create-ci-skill`](skills/create-ci-skill/SKILL.md) reads a repository's GoodFlows config and writes a project-local `run-ci-<app>` skill for the next agent. The generated skill runs those jobs with `goodflows`. A repository with neither `goodflows.toml` nor `.goodflows.toml` gets no skill.

```bash
npx skills add agoodway/goodflows --skill create-ci-skill
```

[`maintain-ci-skill`](skills/maintain-ci-skill/SKILL.md) updates that `run-ci-<app>` skill when the GoodFlows config changes. It keeps the scenario answers already in the skill and asks again only about a new, renamed, or removed workflow. It does not write a skill when none exists.

```bash
npx skills add agoodway/goodflows --skill maintain-ci-skill
```

`goodflows skills install` writes `create-ci-skill` and `maintain-ci-skill` into the directory the current coding agent loads project skills from, one directory per skill, such as `.claude/skills/create-ci-skill/SKILL.md`. The skills are built into the binary, so they match the goodflows you run. Rerunning replaces the installed copies with the built-in ones.

```bash
goodflows skills install
goodflows skills install --harness claude
```

| `--harness` | Directory | Detected from |
| --- | --- | --- |
| `amp` | `.agents/skills` | `AGENT=amp`, `AI_AGENT=amp`, or `AMP_CURRENT_THREAD_ID` |
| `claude` | `.claude/skills` | `CLAUDECODE`, or `AI_AGENT` starting with `claude-code` |
| `codex` | `.agents/skills` | any `CODEX_*` variable except `CODEX_HOME` |
| `cursor` | `.agents/skills` | `CURSOR_AGENT` |
| `gemini` | `.agents/skills` | `GEMINI_CLI` (Gemini CLI reads `.agents/skills` as an alias of `.gemini/skills`) |
| `grok` | `.grok/skills` | `GROK_AGENT` or `GROK_SESSION_ID` |
| `opencode` | `.agents/skills` | `OPENCODE` |
| `t3` | `.agents/skills` | `T3CODE_PROJECT_ROOT` or `T3CODE_WORKTREE_PATH` |

A variable counts only when it is set and not empty. `--harness` skips detection. With no harness detected, the skills go in `.agents/skills`. When the environment names harnesses that load different directories, such as `CLAUDECODE` together with `AGENT=amp`, the command writes nothing and exits 2 with `ambiguous_harness`; pass `--harness`. Harnesses that share a directory, such as T3 Code running Codex, are not ambiguous. With `--json` it prints one `skills_installed` event with `directory`, `harnesses`, `source` (`flag`, `environment`, or `default`), and `files`.

## MCP server

`goodflows mcp` serves goodflows to a coding agent as a [Model Context Protocol](https://modelcontextprotocol.io) server over stdin and stdout. Register it with the harness, for example `claude mcp add goodflows -- goodflows mcp`, and start it from the project directory. Each tool runs this goodflows binary in the server's working directory, so it finds `goodflows.toml` or `.goodflows.toml` there unless the call or `goodflows mcp --config PATH` names a config.

| Tool | Parameters | Returns |
| --- | --- | --- |
| `run` | `workflow`, `config`, `env`, `jobs`, `failed`, `parallelism`, `fail_fast` | Waits for the run. Whether it passed, its exit code and summary counts, and each job that did not pass |
| `last` | `config` | The last local run, as `goodflows last` shows it, in the same form as `run` |
| `validate` | `workflow`, `config`, `env`, `jobs` | The `config_valid` event |
| `list` | `workflow`, `config`, `env`, `workflows` | The `jobs` or `workflows` event |

`run` and `last` never return job output events. The result is a status line (`GoodFlows checks passed.` or `GoodFlows checks failed (exit code N).`), then the root `summary` without `results`, plus `passed`, `exit_code`, and `error`, and a `results` list of the jobs that did not pass with `name`, `status`, `exit_code`, `command`, and `error`. Jobs of called workflows that did not pass are in the list too, with `called_run` naming that run's `workflow`, `run_id`, `caller_job`, and `parent_run_id`. A last text block holds each of those jobs' stdout and stderr tails, at most 4 KiB per stream and less when many jobs failed. `passed` is true only when goodflows exited 0, reported no error, and every root job passed or was skipped. The limits match the Amp plugin's.

When goodflows cannot do what a tool asks, such as for a config or usage error, the tool result is an error (`isError`) with the exit code and goodflows's message. Cancelling a call sends its goodflows process SIGTERM, which cancels the run's jobs as Ctrl-C would. `goodflows mcp` rejects `--json` and the run-only flags; pass them as tool parameters instead.

## Development

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
node --test tests/amp-plugin.test.mjs
```

The Node test needs Node.js 22.18 or newer, and `cargo build` first, because it runs `target/debug/goodflows`. CI on this repository also runs `./target/release/goodflows`, which executes `goodflows.toml`.

### Cursor cloud agents

[`.cursor/environment.json`](.cursor/environment.json) is the cloud agent environment for this repository. The image installs Rust 1.88.0 with rustfmt and clippy, Elixir 1.18.6 on Erlang/OTP 27.3.4.19, and Node.js 24.22.2. [`.cursor/install.sh`](.cursor/install.sh) fetches Cargo, Mix, and Jest fixture dependencies when an environment build runs.

To change a toolchain, edit the version and checksum in [`.cursor/Dockerfile`](.cursor/Dockerfile) and keep them aligned with `Cargo.toml` (`rust-version`), `scripts/sprite-setup.sh`, `reporters/exunit/mix.exs`, and the versions in `.github/workflows/ci.yml` and `.github/workflows/reporters.yml`. The next environment build uses the updated file. Nothing in this repo needs a process kept running, so the environment sets no `start` command.

Behavior that this file and [`docs/`](docs/) do not spell out is defined by the code and pinned by the tests in [`tests/`](tests/). Release steps for the CLI and for each reporter are in [docs/releasing.md](docs/releasing.md).
