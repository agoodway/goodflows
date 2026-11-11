---
name: maintain-ci-skill
description: "Update an existing run-ci-<app> skill so its workflows, jobs, and goodflows commands match the current GoodFlows config. Use for /maintain-ci-skill, \"update the CI skill\", or when a run-ci skill names jobs or workflows the config no longer has."
disable-model-invocation: true
---

# Maintain a CI skill

A `run-ci-<app>` skill goes stale when the GoodFlows config changes. This skill is the upkeep pass for a skill written by `create-ci-skill`. It keeps the scenario answers already in that file, and it edits the file only where a workflow, job, `goodflows` command, or failure note no longer matches the config.

It does not generate a skill. No `run-ci-<app>` directory means stop.

Do not open any other CI system and do not mention one, in this run or in the skill. Do not turn a Makefile, a package script, a hook, or a workflow file into jobs.

## Outcomes

Pick one, and say which:

- **unchanged** — the skill already matches the config. No questions. No file write.
- **updated** — the skill was edited so every command and job name matches the config, then the `goodflows` steps were run once. Each step is pass, fail, or blocked.
- **stopped** — there is no `run-ci-<app>` skill, or there is no GoodFlows config. Nothing was written.

## Edit scope

Edit only the chosen `run-ci-<app>` directory (its `SKILL.md` and a helper script that directory already owns). Do not edit product code. Do not edit `goodflows.toml` or `.goodflows.toml` unless the user explicitly says to apply a proposal you showed them in this run.

On **unchanged**, do not write any file. Do not reformat, reorder, or rewrap. The skill file stays byte-identical.

## 1. Find the skill

`create-ci-skill` writes `run-ci-<app>` in exactly one of these directories. Search all three:

- `.agents/skills/` (Amp, Cursor, OpenCode, Codex, T3 Code, and an unknown harness)
- `.claude/skills/` (Claude Code)
- `.grok/skills/` (Grok CLI)

A candidate is a directory whose name starts with `run-ci-`, that contains `SKILL.md`, and whose frontmatter `name` equals the directory name. Do not search other directories. Do not treat `skills/create-ci-skill` or `skills/maintain-ci-skill` as a candidate.

- None: stop. Say this repo has no `run-ci-<app>` skill, so nothing was updated. Point at `create-ci-skill` (`npx skills add agoodway/goodflows --skill create-ci-skill`, then run it). Do not create a directory, a `SKILL.md`, or a partial file.
- Several: ask which one to update. Do not edit until they answer. This is the only question that is not about a config change.
- One: that is the target. Update it where it sits. Do not move it, and do not write a second copy for the harness you are running in.

## 2. Read the config

Read the GoodFlows config and the target skill. Nothing else.

The CLI uses the first of `goodflows.toml` and `.goodflows.toml` that exists in the current directory. Read that file. If every scenario command and the proof block pass the same `--config PATH`, read that path instead of the default file. If those commands name more than one `--config` path, ask which file to compare and do not edit until they answer. Do not search the repo for a config.

If neither default file exists, and the skill does not pass `--config`, stop. Leave the skill byte-identical. Say there is no GoodFlows config to compare against, so nothing was updated. Point at the [goodflows README](https://github.com/agoodway/goodflows#install): install the CLI, then from the repo root run `goodflows init`. Do not run `init`. Do not invent jobs.

Record the file:

- Which file was used.
- Top-level `[[jobs]]` are the `default` workflow. `[[workflows.NAME.jobs]]` are named workflows. `[settings]` and any `[environments.NAME]`: `parallelism`, `fail_fast`, and `settings.env`.
- Each job's `name`, `command` or `steps`, `depends_on`, `workflow` (a call to another workflow), and `condition`.
- A `[sprite]` block or an `[environments]` profile is a fact. A flag nobody wrote down is not.

Never write a secret value into the skill.

## 3. Compare

A workflow, job, or command inside a section that says the change was not applied is a proposal, not a claim that the config has it. Ignore those names when deciding drift. Leave that section in place while the config still lacks it. Do not ask about it again.

The skill is already accurate when all of the following hold:

- The runnable workflow names match the config. `default` is present only when the config has top-level `[[jobs]]`.
- Every runnable job's `name`, `command` or `steps`, `depends_on`, `workflow` call, and `condition` matches, and the skill does not list a runnable job the config lacks.
- `parallelism`, `fail_fast`, and `settings.env` match what the file sets. An unset setting is not written as a value.
- Every scenario command names a workflow that still exists, and every `--jobs` name still exists on that workflow. A command with no `run NAME` is the default workflow. It matches when the config has top-level `[[jobs]]`. `--env`, `--config`, and `--sprite` on that command are unchanged and still supported by the config.
- Failure notes cite only job names and command strings the config still has. Wording, section order, and an example rerun line are not drift when every workflow, job, and command token in them still exists.

Then the outcome is **unchanged**. Ask nothing. Stop. Do not run the proof block. Running it is not a reason to touch the file.

Otherwise the outcome will be **updated**. Change only the lines that name a workflow, job, command, flag, or failure that drifted. Leave every other line as written, including scenario sentences whose workflow still exists.

Do not ask about a workflow that already has a scenario. Ask only in these two cases:

- A workflow is in the config and no scenario command runs it. Ask which scenario it is for. Ask where it runs (this machine, `--sprite`, `--env`, or `--config`) only when the skill does not already say where scenarios run. One question per such workflow. Do not invent a purpose while you wait.
- A scenario command names a workflow that is not in the config. Ask whether that scenario should move to a workflow that is in the config, or be removed. When one workflow name disappeared and one appeared, name both in the question. Do not pair them yourself.

A job that was added, removed, or renamed, or whose `command`, `steps`, `depends_on`, `condition`, or `workflow` call changed, is not one of those questions. Update the job table, the invocation table, failure notes that cite the old name or the old command, and any `--jobs` token that names it.

A rename is one removed job name and one added job name in the same workflow whose `command` (or `steps`) is the same. Replace the old name in scenario commands. If you cannot tell a rename from a removal plus an addition, replace the job table from the config and delete `--jobs` tokens the workflow no longer has. If that deletes every name in `--jobs`, drop `--jobs` and keep the scenario on that workflow.

If the user is unavailable, do not invent a scenario and do not guess a renamed workflow. Update the job and command lines that do not need an answer. For a workflow with no scenario, list its jobs and add `goodflows run NAME` with the words "no scenario chosen". For a scenario whose workflow is gone, mark that command as not in the config and leave it out of the proof block. Say that you needed an answer and did not get one.

### When the answer does not match the config

If they name a workflow, a job, an `--env` profile, a `--sprite` run, or a `fail_fast` or `parallelism` value the config does not have, show the `goodflows.toml` (or `.goodflows.toml`) change that would add it. Do not edit that file unless they explicitly say to apply that proposal. Do not put the proposal in the proof block. You may record it in the skill as not applied, in the same place the file already records an unapplied proposal, or in a short "Not in the config" note if it has none.

The rest of the update still happens: runnable names and commands match the current config.

If they explicitly say to apply the proposal, edit only the lines you showed them, re-read the config, and update the skill from the file you wrote.

### Commands

Keep the `goodflows` invocations the skill already uses. Do not retype a job's `command` as the way to run it.

| Intent | Command |
|---|---|
| Default workflow | `goodflows` (same as `goodflows run`) |
| Named workflow | `goodflows run NAME` |
| Selected jobs plus their `depends_on` | that command, plus `--jobs a,b` |
| Other config file | `--config PATH` on the command above |

`goodflows --jobs a,b` with no workflow name selects the default workflow. A named workflow is `goodflows run NAME --jobs a,b`. Keep `--config`, `--env`, and `--sprite` on that command when the skill's scenario already used them and the config still supports them.

Use the binary the skill's proof block already builds or installs. If the block names none, use the same rule as `create-ci-skill`: `goodflows` on `PATH` when it is installed; `./target/release/goodflows` after `cargo build --release --locked` when this checkout's `Cargo.toml` has `name = "goodflows"`; otherwise `cargo install --git https://github.com/agoodway/goodflows.git --locked`.

## 4. Prove the update

Run this section only when the file changed.

From the repo root, run `### Proof commands` one command at a time, in order. Then run each scenario command you added or edited, once, skipping any command the proof block already ran. Do not run an unapplied proposal.

Record the command, the exit code, and pass, fail, or blocked.

- The check ran and exited non-zero: report fail. Do not weaken a flag, drop a job, or mark it skipped to produce a green report. Do not edit product code.
- A job's command cannot start because the program it runs is missing (`command not found`, no compiler, no interpreter). That job is blocked, not passed. Name the missing program. Leave the job in the proof block. Do not delete it to make the run exit 0. Continue only with later commands that do not need it.
- The same rule applies when `goodflows` starts and a job then fails because that program is absent. Report the job as blocked and quote the line that names the missing program.
- If the CLI cannot be built or installed, the proof is blocked. Leave the build or install command in the proof block and say the binary was not produced.

A missing toolchain is blocked, not a pass. An updated skill that was never run is a draft. The handoff includes the outcome, the questions you asked (or that you asked nothing), and each command's exit code.

## 5. Maintenance

**unchanged** needs nothing further. **updated** is done once the report is in the conversation. Do not commit unless the user asks. Do not open a pull request from this skill. The next config change is another run of `maintain-ci-skill`.
