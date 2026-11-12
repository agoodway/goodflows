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

If what they want is not in the config (a release workflow that does not exist, a `--env` profile with no `[environments]` entry, a different `fail_fast`), say so and propose the `goodflows.toml` change. Do not edit the config unless they explicitly say to apply it. A proposal is not a command in the proof block.

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
| Machine-readable result | `--json` (JSON Lines; the last `summary` event has `success`) |

`goodflows --jobs a,b` with no workflow name selects the default workflow and the default config file. A file with no default exits 2 and names the workflows you can pass. A named workflow is `goodflows run NAME --jobs a,b`. Keep `--config` and `--env` on that command when the run used them.

| Exit | Meaning | Next step |
|---|---|---|
| `0` | Every selected job passed or was skipped | Green |
| `1` | A job failed or timed out, or goodflows hit a runtime error after it accepted the command and config | Report it; the command is right |
| `2` | Usage or config error: bad arguments, no config, a config that does not parse or validate, an unknown workflow, job, or environment | Fix the command, then run it again |
| `130`, `143` | Interrupted by SIGINT or SIGTERM | Rerun when nothing interrupts it |

With `--json`, a run that cannot start ends with `{"type":"error","code":...}`. The `code` names the usage or config error: `usage`, `missing_config`, `config_parse`, `config_invalid`, `no_default_workflow`, `unknown_workflow`, `unknown_job`, or `unknown_environment`. `runtime` is the exit `1` case. An unknown job or workflow message lists the names that exist, and `hint` may suggest a close match. `goodflows list` confirms names when the binary is already installed. Parsing the TOML is enough when it is not.

The binary the proof invokes:

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

- **Scenarios.** A map of "for X, run Y" with the exact `goodflows` command, including `run NAME`, `--jobs`, `--env`, `--config`, or `--sprite` when that answer called for a flag the config supports. The default scenario is first. When the map fell back to the config, say that. When an answer does not match the config, give the proposed `goodflows.toml` and state that it was not applied. Do not put that proposal in the proof block.

- **Run it locally.** Exact commands from the repo root. Lead with the scenario map's default command and a table:

  | Job | Command in the config | Invocation that runs it |
  |---|---|---|

  The invocation is `goodflows`, `goodflows run NAME`, or `goodflows --config PATH …`, using that job's `name`. One invocation covers every job in that workflow. Do not also schedule the job's shell. Include the command that builds or installs the CLI, from the rule above.

  End the section with `### Proof commands` and one fenced `bash` block. That block is the default scenario, in order, copy-pasteable from the repo root: build or install the CLI when the rule above requires it, then that scenario's command. Other scenarios that the config can already run stay in the map. Do not add commands that are not in the config, and do not run a proposed workflow that was not applied.

- **Read a failure.** A failed job is not a skipped one. Skipped means a dependency failed or a condition was false. The text report has a `Failures` section. The rerun command is the `Rerun failed:` line, copied as printed. It is `goodflows run`, with the workflow name, and with `--config`, `--env`, and `--sprite` when the run used them, then `--jobs` for the failed jobs and the jobs skipped because a dependency did not pass. `--jobs` also runs `depends_on`. Do not shorten it to `goodflows --jobs <names>`: with no workflow name that selects the default workflow, and with no `--config` it reads `goodflows.toml` or `.goodflows.toml` instead of the file the run used. When the report says `Selective rerun unavailable` because a job name contains a comma, copy the `Rerun workflow:` line instead. Use the same binary the proof used, with those arguments. `--json` prints JSON Lines; the last event has `"type":"summary"`. `success` is true only when every selected job passed or was skipped. Failed jobs include `stdout_tail` and `stderr_tail`. Only name a failure mode the job's command supports. Do not invent a past flake.

- **Green.** The proof invocation exited 0. State the binary path and the workflow that ran.

- **Keeping this skill accurate.** When `goodflows.toml` or `.goodflows.toml` changes, or the answers in section 2 change, the job names, the scenario map, and the proof block have to match the config. End the section with this sentence: When the GoodFlows config changes, run `maintain-ci-skill` to keep this skill accurate. Install it with `npx skills add agoodway/goodflows --skill maintain-ci-skill`.

A helper script is allowed only when a command in the proof block cannot be written inline. It is executable, and the proof block shows how it is invoked.

## 4. Prove the generated skill before handing it over

Run `### Proof commands` from the repo root, one command at a time, in order. Record the command, the exit code, and one line that shows pass or fail.

- The `goodflows` command exited `2`: a usage or config error, so the skill's command is wrong. Quote the `Error:` line, or the `error` event's `code` with `--json`. Fix the command, the path, the `--config`, `--env`, or the workflow or job name, then run the whole block again from the first command. Do not edit `goodflows.toml` to make it pass unless the user said to apply that change.
- The `goodflows` command exited `1`: the command is right and the repo failed a job, or goodflows hit a runtime error. Stop and report that failure. Do not weaken a flag, drop a job, or mark it skipped to produce a green report.
- A build or install command before `goodflows` exited non-zero: stop. Fix the command if it was wrong, then run the whole block again; otherwise the proof is blocked (see below).
- A job's command cannot start because the program it runs is missing (`command not found`, no compiler, no interpreter). That job is blocked, not passed. Name the missing program. Leave the job in the proof block. Do not delete it to make the run exit 0. Continue only with later commands that do not need it.
- The same rule applies when `goodflows` starts and a job then fails because that program is absent. Report the job as blocked and quote the line that names the missing program.
- If the CLI cannot be built or installed, the proof is blocked. Still write the skill, with the build or install command in the proof block, and say the binary was not produced.

A generated skill that was never run is a draft, not a deliverable. The handoff includes the report: each command, its exit code, and any command left blocked. The no-config stop in section 1 is not a proof. There is no skill to run.

## 5. Maintenance

Tell the user to run `maintain-ci-skill` to keep the generated `run-ci-<app>` skill accurate when the GoodFlows config changes. Install it with `npx skills add agoodway/goodflows --skill maintain-ci-skill` when it is not already installed. The generated skill's `## Keeping this skill accurate` section ends with that same sentence.
