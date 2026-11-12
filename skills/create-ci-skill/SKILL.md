---
name: create-ci-skill
description: "Generate a project-local run-ci-<app> skill from a GoodFlows config. Use for /create-ci-skill, \"make a CI skill for this repo\", or when agents don't know how to run this project's goodflows jobs or read a failure."
disable-model-invocation: true
---

# Create a CI skill

GoodFlows is the check. This skill reads `goodflows.toml` or `.goodflows.toml` and writes a project-local skill (`run-ci-<app>/`) that runs those jobs with the `goodflows` CLI. The next agent reads it cold, mid-task, without having seen the repo.

Do not open any other CI system and do not mention one in the generated skill. Do not turn a Makefile, a package script, a hook, or a workflow file into jobs. GoodFlows is not a layer on top of another runner.

Write that skill in the directory the harness you are running in loads. Decide from the harness, not from which dot-directories the repo already contains:

- Amp, Cursor, OpenCode, Codex, and T3 Code: `.agents/skills/run-ci-<app>/`
- Claude Code: `.claude/skills/run-ci-<app>/`
- Grok CLI: `.grok/skills/run-ci-<app>/`

If the harness is unknown, use `.agents/skills/run-ci-<app>/`. Write one directory. Every file below is inside it.

`<app>` is a slug of the repository name (`git remote get-url origin`, else the checkout directory): lowercase, every character outside `a-z`, `0-9`, and `-` replaced with `-`, repeats collapsed, ends trimmed. One invocation writes one skill.

## 1. Read the config

Look only for a GoodFlows config. The CLI uses the first of `goodflows.toml` and `.goodflows.toml` that exists in the current directory, unless a command passes `--config`. Read that file and no other CI file.

If neither file exists, stop. Create nothing: no `run-ci-<app>` directory, no `SKILL.md`, no partial file. Tell the user this repo has no GoodFlows config, so no skill was written. Point them at the [goodflows README](https://github.com/agoodway/goodflows#install): install the CLI, then from the repo root run `goodflows init` (pass `--template rust|elixir|node|go|generic` when `init` reports more than one project file). After `goodflows.toml` exists, run this skill again. Do not run `init` for them, and do not invent commands from any other system.

When a config exists, record it:

- Which file was used.
- Top-level `[[jobs]]` are the `default` workflow. `[[workflows.NAME.jobs]]` are named workflows. `[settings]` and any `[environments.NAME]`: `parallelism`, `fail_fast`, and `settings.env`.
- Each job's `name`, `command` or `steps`, `depends_on`, `workflow` (a call to another workflow), and `condition`.
- Any `--env`, `--config`, or `--sprite` the file or the repo's GoodFlows docs already name. A `[sprite]` block or an `environments` profile is a fact. A flag nobody wrote down is not.

Never write a secret value into the skill.

## 2. Ask the user

Present that record in a few lines, then ask. A repo can use different workflows for different scenarios. The config lists the workflows. It does not say which situation each one is for.

Ask only what the record does not already answer. Three or four questions is enough. Skip a question the file already settles (a single workflow needs no "which workflow" choice; an `environments.ci` profile does not need to be asked into existence).

- Which scenario each workflow is for: a quick local check before commit, a full run before a PR, a release, a nightly run, or something else they name.
- Where each scenario runs: this machine, a Sprite (`--sprite`), an environment profile (`--env`), or another config file (`--config`).
- Which workflow the next agent runs by default, and when it should pick another.
- Whether `fail_fast` or `parallelism` should differ by scenario.

If the user is unavailable, or says to use the defaults, take the config as the map: the default workflow is the one the next agent runs, named workflows stay available as `goodflows run NAME` with no invented purpose, and settings stay as written. Say in the generated skill that the map is the config because nobody chose scenarios.

If what they want is not in the config (a release workflow that does not exist, a `--env` profile with no `[environments]` entry, a different `fail_fast`), say so and propose the `goodflows.toml` change. Do not edit the config unless they explicitly say to apply it. A proposal is not a scenario the generated skill runs.

### Commands

Local runs go through the `goodflows` CLI. Do not retype a job's `command` as the way to run it.

| Intent | Command |
|---|---|
| Default workflow | `goodflows` (same as `goodflows run`) |
| Named workflow | `goodflows run NAME` |
| Selected jobs plus their `depends_on` | the full-run command, plus `--jobs a,b` |
| Parse only, run nothing | `goodflows validate` |
| Print jobs | `goodflows list` |
| Other config file | `--config PATH` on the command above |
| Show the last local run's report again | `goodflows last` |
| Rerun the last run's failed jobs | `goodflows run --failed` |
| Start a run in the background | `goodflows run --detach` or `goodflows run NAME --detach`, with the run's other flags; prints the run id |
| Wait for a detached run | `goodflows wait RUN_ID --timeout 90`, plus the run's `--config` |
| Machine-readable result | `--json=summary`; bare `--json` also streams every output line |
| Guide to exit codes, error codes, failures, and JSON | `goodflows agents` |

`goodflows --jobs a,b` with no workflow name selects the default workflow and the default config file. A file with no default exits 2 and names the workflows you can pass. A named workflow is `goodflows run NAME --jobs a,b`. Keep `--config` and `--env` on that command when the run used them.

`goodflows agents` prints the guide to the installed CLI: exit codes, error codes, the report's `Failures` section, `last`, `run --failed`, detached runs, and the JSON events and fields. It ships in the binary, so it matches the version installed. Read it when you need those details. Section 4 says how to read each exit code when you verify the skill. An unknown job or workflow message lists the names that exist. `goodflows list` confirms names when the binary is already installed. Parsing the TOML is enough when it is not.

The binary the generated skill's commands invoke:

- `goodflows` on `PATH`, when it is already installed.
- If this checkout is the goodflows package (`Cargo.toml` `name = "goodflows"`), build this checkout and invoke `./target/release/goodflows` after `cargo build --release --locked`.
- Otherwise install with `cargo install --git https://github.com/agoodway/goodflows.git --locked` and say the repo did not pin a version.

## 3. Generate the skill

Write `SKILL.md` with YAML frontmatter. Without it the skill never registers.

```yaml
---
name: run-ci-<app>
description: "Use when running GoodFlows checks or reading a goodflows failure in <App>."
---
```

`name` matches the directory name. The description names the app in words a person would use and states when to reach for the skill. Double-quote it. No placeholders, no `TODO`, no angle-bracket tokens remain anywhere in the file.

These sections, in this order. The job facts come from the config. The scenario map comes from the answers in section 2.

- **What GoodFlows runs.** The config path, the default workflow or each named workflow, and every job `name` with the command the config stores, in `depends_on` order. Quote `parallelism` and `fail_fast` when they are set. A condition stays on the job. A skipped job is not a pass.

- **Scenarios.** A map of "for X, run Y" with the exact `goodflows` command, including `run NAME`, `--jobs`, `--env`, `--config`, or `--sprite` when that answer called for a flag the config supports. Every command uses the binary from the rule above. The default scenario is first and is marked as the default. Next to it, give the command that builds or installs the CLI when the rule above requires one, so the map shows which binary to use and how to get it. Other scenarios that the config can already run stay in the map. Do not add commands that are not in the config. When the map fell back to the config, say that. When an answer does not match the config, give the proposed `goodflows.toml` and state that it was not applied. Do not write that proposal as a scenario command.

- **Run it locally.** Exact commands from the repo root. Lead with the scenario map's default command, after its build or install line when it has one, and a table:

  | Job | Command in the config | Invocation that runs it |
  |---|---|---|

  The invocation is `goodflows`, `goodflows run NAME`, or `goodflows --config PATH …`, using that job's `name`. One invocation covers every job in that workflow. Do not also schedule the job's shell.

- **Read a failure.** The commands this repo uses to see a failure again and to rerun what did not pass, written with the scenario map's binary and the scenario's `--config`: `goodflows last` and `goodflows run --failed`. Then this one line, with the same binary: For exit codes, error codes, the `Failures` section, `last`, `run --failed`, detached runs, and the JSON fields, run `goodflows agents`, which prints the guide for the installed goodflows. Name a failure mode only when this repo's job command supports it, and do not invent a past flake.

- **Green.** The default scenario's `goodflows` command passed, as `goodflows agents` describes. State the binary path and the workflow that ran.

- **Keeping this skill accurate.** When `goodflows.toml` or `.goodflows.toml` changes, or the answers in section 2 change, the job names and the scenario map, with its default scenario and build or install line, have to match the config. End the section with this sentence: When the GoodFlows config changes, run `maintain-ci-skill` to keep this skill accurate. Install it with `goodflows skills install`.

A helper script is allowed only when a scenario command cannot be written inline. It is executable, and the scenario map shows how it is invoked.

Do not write a separate section that repeats the default scenario's commands, and do not add a note on running the scenario detached. The scenario map is the one place those commands live.

Keep only what is specific to this repo in the generated skill: its workflows, jobs, and scenario map, and the commands that run them. Do not copy how the CLI behaves: state no exit code, error code, JSON field or event name, output size limit, report label, or flag behavior beyond the commands themselves. `goodflows agents` is the one place those live, and the `Read a failure` line points at it, so the skill stays accurate when goodflows changes.

## 4. Verify the generated skill with the default scenario

Before handing the skill over, run the default scenario once from the repo root: its build or install line when the map has one, then its `goodflows` command, as the scenario map gives them. Do not run other scenarios or an unapplied proposal. The CLI has no dry run; this run is the check. Record each command, its exit code, and the verdict below.

The build or install line runs in the foreground. A `goodflows` run of a workflow may take longer than about 2 minutes, a common limit on an agent's shell command, and a shell that stops it leaves no result. Unless the workflow is clearly quicker than that, run it detached. This is how you run the check; do not write the detached form into the skill.

1. Start it as `goodflows run --detach`, or `goodflows run NAME --detach`, with the scenario map's binary and every other flag of the command kept. `--detach` is a flag of `run`: a bare `goodflows --detach` exits `2`. It returns at once and prints the run id on stdout. With `--json` or `--json=summary` it prints one `run_detached` event instead; its `run_id` is the id. A usage or config error still exits `2` here, before anything starts.
2. Run `goodflows wait RUN_ID --timeout 90` with the printed id written out, not in a shell variable, since the next command may run in a new shell. Add the run's `--config PATH`; without it `wait` looks beside the default config and exits `2` with `unknown_run`. Add `--json=summary` when the run used it.
3. While `wait` exits `75`, run the same `wait` again. `75` means the timeout passed and the run is still going: it is neither a pass nor a fail, and stopping `wait` does not stop the run. `goodflows status RUN_ID`, with the same `--config`, shows which jobs are running, passed, or failed.
4. The first `wait` that exits something other than `75` prints the run's report, `Failures` section included, and exits with the run's own exit code, the same one the foreground command would give. Record that as the command's exit code and apply the rules below. If that output was lost, `goodflows last`, with the run's `--config`, prints the report again and exits with the same code.

`--detach` rejects `--sprite`, so a `--sprite` command runs in the foreground as written. If the shell stops it before it exits, it has no exit code: report it as stopped by the shell, not as pass or fail.

Read the result. The skill is verified when the config accepted its command; whether the jobs passed is a fact about the repo, not about the skill.

- Exit `0`: skill verified, CI green.
- Exit `1`: skill verified, CI red. The command is right and the repo failed or timed out a job, or goodflows hit a runtime error. Report the failing jobs from the `Failures` section. Do not treat the skill as wrong, and do not weaken a flag, drop a job, or mark it skipped to produce a green report.
- Exit `2`: the skill's command does not match the config. Quote the `Error:` line, or the `error` event's `code` with `--json`. Fix the command in the skill (the path, `--config`, `--env`, or the workflow or job name), then run the default scenario again. Do not edit `goodflows.toml` to make it pass unless the user said to apply that change.
- `wait` exited `75`: no result yet. Run the same `wait` again. Never record `75` as the result.
- `wait` exited `1` with `stopped without a result`: the background run died, for example from SIGKILL. Record exit `1`, but do not name a failed job: report that the run stopped without a result, and quote the line with the `stderr.log` path it names.
- `wait` exited `2` with `unknown_run`: the `wait` named the wrong id or `--config`. Fix the `wait`, not the skill. The run is unaffected.
- The build or install line exited non-zero: stop. Fix the line if it was wrong and run again. If the CLI still cannot be built or installed, verification is blocked. Still write the skill, with the build or install line next to the default scenario, and say in the handoff that the binary was not produced and the skill was not verified.
- A job's command cannot start because the program it runs is missing (`command not found`, no compiler, no interpreter). That job is blocked, not passed. Name the missing program and quote the line that names it. Leave the job in the skill. Do not delete it to make the run exit 0.

A generated skill whose default scenario was never run is a draft, not a deliverable. The handoff includes each command, its exit code, and the verdict: verified with CI green, verified with CI red and the failing jobs, or blocked and why. The no-config stop in section 1 has nothing to verify. There is no skill to run.

## 5. Maintenance

Tell the user to run `maintain-ci-skill` to keep the generated `run-ci-<app>` skill accurate when the GoodFlows config changes. Install it with `goodflows skills install` when it is not already installed. The generated skill's `## Keeping this skill accurate` section ends with that same sentence.
