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
- **updated** — the skill was edited so every command and job name matches the config, then its default scenario was run once to verify it. The result is verified with CI green, verified with CI red, or blocked.
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

- None: stop. Say this repo has no `run-ci-<app>` skill, so nothing was updated. Point at `create-ci-skill` (install it with `goodflows skills install`, then run it). Do not create a directory, a `SKILL.md`, or a partial file.
- Several: ask which one to update. Do not edit until they answer. This is the only question that is not about a config change.
- One: that is the target. Update it where it sits. Do not move it, and do not write a second copy for the harness you are running in.

## 2. Read the config

Read the GoodFlows config and the target skill. Nothing else.

The CLI uses the first of `goodflows.toml` and `.goodflows.toml` that exists in the current directory. A command with `--config PATH` uses that file instead. Read every file a scenario command selects. Compare each command to the file it selects. Do not compare a `--config` command to the default file, and do not compare a command with no `--config` to some other file. Do not search the repo for a config the commands do not name.

If a command selects a file that does not exist, that command's workflow is not in the config. Handle it in section 3. Do not judge it against a different file. If no selected file exists, stop. Leave the skill byte-identical. Say there is no GoodFlows config to compare against, so nothing was updated. Point at the [goodflows README](https://github.com/agoodway/goodflows#install): install the CLI, then from the repo root run `goodflows init`. Do not run `init`. Do not invent jobs.

Record each file:

- Which file it is, and which commands select it.
- `default` is the workflow from top-level `[[jobs]]`, or from `[[workflows.default.jobs]]` when the file has no top-level jobs. A file with both does not load (`root jobs conflict with named workflow 'default'`). Stop. Leave the skill byte-identical. Report that conflict. Do not pick one form.
- Every other `[[workflows.NAME.jobs]]` is a named workflow. `[settings]` and any `[environments.NAME]`: `parallelism`, `fail_fast`, and `settings.env`. A workflow's settings replace `[settings]` one field at a time. A field the workflow omits keeps the shared value. Compare the values the run would use.
- Each job's `name`, `command` or `steps`, `depends_on`, `workflow` (a call to another workflow), and `condition`.
- A `[sprite]` block or an `[environments]` profile is a fact. A flag nobody wrote down is not.

Never write a secret value into the skill.

## 3. Compare

A workflow, job, or command inside a section that says the change was not applied is a proposal, not a claim that the config has it. Ignore those names when deciding drift. Leave that section in place while the config still lacks it. Do not ask about it again.

The skill is already accurate when all of the following hold for every file a command selects:

- The runnable workflow names match that file. `default` is present when the file has top-level `[[jobs]]` or a workflow named `default`, and absent when it has neither.
- Every runnable job's `name`, `command` or `steps`, `depends_on`, `workflow` call, and `condition` matches that file, and the skill does not list a runnable job the file lacks.
- `parallelism`, `fail_fast`, and `settings.env` match the values the run would use. An unset setting is not written as a value.
- Every scenario command names a workflow that still exists in the file that command selects, and every `--jobs` name still exists on that workflow. A command with no `run NAME` is `default`. It matches when that file has top-level `[[jobs]]` or a workflow named `default`. `goodflows run default` is the same workflow. `--env`, `--config`, and `--sprite` on that command are unchanged and still supported by the file that command selects.
- Failure notes cite only job names and command strings the selected file still has. Wording, section order, and an example rerun line are not drift when every workflow, job, and command token in them still exists.
- The skill has nothing the migration rule below removes.

Then the outcome is **unchanged**. Ask nothing. Stop. Do not run the default scenario. Running it is not a reason to touch the file.

Otherwise the outcome will be **updated**. Change only the lines that name a workflow, job, command, flag, or failure that drifted, plus what the migration rule below changes. Leave every other line as written, including scenario sentences whose workflow still exists.

Migration rule: a skill written by an older `create-ci-skill` has a `### Proof commands` section, a fenced block with the default scenario's build or install line and its command, often followed by a note on running that command detached with `--detach`, `wait`, and exit `75`. Remove that section and its note. In the scenario map, mark the scenario whose command the block ran as the default, and put the block's build or install line next to it when the map does not already have it. If no scenario has that command, mark the map's first scenario. Where other lines name that section or its binary, such as `Green` or `## Keeping this skill accurate`, refer to the default scenario and the scenario map instead. This makes the outcome **updated** even when every name and command already matches the config.

`## Keeping this skill accurate` may end with the current sentence or one of two older ones. The current sentence is: When the GoodFlows config changes, run `maintain-ci-skill` to keep this skill accurate. Install it with `goodflows skills install`. The first older one is the same sentence ending with: Install it with `npx skills add agoodway/goodflows --skill maintain-ci-skill`. The second older one says that section is the maintenance, and that a separate upkeep command does not exist. None of these wordings is drift. An older sentence by itself does not make the outcome **updated**. On **unchanged**, leave whichever sentence is there so the file stays byte-identical. When the outcome is **updated**, replace an older sentence with the current one if it is still there. If the current sentence is already there, leave it. Leave the rest of that section as written.

Do not ask about a workflow that already has a scenario on the file that workflow belongs to. A bare `goodflows` and `goodflows run default` both run `default`. Ask only in these two cases:

- A workflow is in a selected file and no scenario command that selects that same file runs it. Ask which scenario it is for. Ask where it runs (this machine, `--sprite`, `--env`, or `--config`) only when the skill does not already say where scenarios run. One question per such workflow. Do not invent a purpose while you wait.
- A scenario command names a workflow that is not in the file that command selects, including when that file does not exist. Ask whether that scenario should move to a workflow in a file the skill already selects, or be removed. When one workflow name disappeared and one appeared in that same file, name both in the question. Do not pair them yourself. Do not treat a workflow that still exists in a different selected file as the replacement.

A job that was added, removed, or renamed, or whose `command`, `steps`, `depends_on`, `condition`, or `workflow` call changed, is not one of those questions. Update the job table, the invocation table, failure notes that cite the old name or the old command, and any `--jobs` token that names it. Leave lines for every other selected file as written.

A rename is one removed job name and one added job name in the same workflow of the same file whose `command` (or `steps`) is the same. Replace the old name in scenario commands that select that file. If you cannot tell a rename from a removal plus an addition, replace the job table from that file and delete `--jobs` tokens the workflow no longer has. If that deletes every name in `--jobs`, drop `--jobs` and keep the scenario on that workflow.

If the user is unavailable, do not invent a scenario and do not guess a renamed workflow. Update the job and command lines that do not need an answer. For a workflow with no scenario, list its jobs and add `goodflows run NAME` with the words "no scenario chosen", plus `--config PATH` when that workflow's file is not the default file. For a scenario whose workflow is gone, mark that command as not in the config and do not run it in section 4. Say that you needed an answer and did not get one.

### When the answer does not match the config

If they name a workflow, a job, an `--env` profile, a `--sprite` run, or a `fail_fast` or `parallelism` value the file that command selects does not have, show the change to that file. Do not edit it unless they explicitly say to apply that proposal. Do not write the proposal as a scenario command. You may record it in the skill as not applied, in the same place the file already records an unapplied proposal, or in a short "Not in the config" note if it has none.

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

`goodflows --jobs a,b` with no workflow name selects `default` in the file that command reads. A named workflow is `goodflows run NAME --jobs a,b`. Keep `--config`, `--env`, and `--sprite` on that command when the skill's scenario already used them and the file that command selects still supports them.

Use the binary the skill's scenario map already uses, with the build or install line next to its default scenario. If the skill names none, use the same rule as `create-ci-skill`: `goodflows` on `PATH` when it is installed; `./target/release/goodflows` after `cargo build --release --locked` when this checkout's `Cargo.toml` has `name = "goodflows"`; otherwise `cargo install --git https://github.com/agoodway/goodflows.git --locked`.

## 4. Verify the update with the default scenario

Run this section only when the file changed.

From the repo root, run the default scenario once: its build or install line when the scenario map has one, then its `goodflows` command, as the updated skill gives them. Do not run other scenarios or an unapplied proposal. The CLI has no dry run; this run is the check. Record each command, its exit code, and the verdict below.

The build or install line runs in the foreground. A `goodflows` run of a workflow may take longer than about 2 minutes, a common limit on an agent's shell command, and a shell that stops it leaves no result. Unless the workflow is clearly quicker than that, run it detached. This is how you run the check; do not write the detached form into the skill.

1. Start it as `goodflows run --detach`, or `goodflows run NAME --detach`, with the scenario map's binary and every other flag of the command kept. `--detach` is a flag of `run`: a bare `goodflows --detach` exits `2`. It returns at once and prints the run id on stdout. With `--json` or `--json=summary` it prints one `run_detached` event instead; its `run_id` is the id. A usage or config error still exits `2` here, before anything starts.
2. Run `goodflows wait RUN_ID --timeout 90` with the printed id written out, not in a shell variable, since the next command may run in a new shell. Add the run's `--config PATH`; without it `wait` looks beside the default config and exits `2` with `unknown_run`. Add `--json=summary` when the run used it.
3. While `wait` exits `75`, run the same `wait` again. `75` means the timeout passed and the run is still going: it is neither a pass nor a fail, and stopping `wait` does not stop the run. `goodflows status RUN_ID`, with the same `--config`, shows which jobs are running, passed, or failed.
4. The first `wait` that exits something other than `75` prints the run's report and exits with the run's own exit code, the same one the foreground command would give. Record that as the command's exit code and apply the rules below.

`--detach` rejects `--sprite`, so a `--sprite` command runs in the foreground as written. If the shell stops it before it exits, it has no exit code: report it as stopped by the shell, not as verified or blocked.

Read the result. The skill is verified when the config accepted its command; whether the jobs passed is a fact about the repo, not about the skill.

- Exit `0`: skill verified, CI green.
- Exit `1`: skill verified, CI red. The command is right and a job failed or timed out, or goodflows hit a runtime error. Report the failing jobs from the `Failures` section. Do not treat the skill as wrong, and do not weaken a flag, drop a job, or mark it skipped to produce a green report. Do not edit product code.
- Exit `2`: the skill's command no longer matches the config. Quote the `Error:` line, or the `error` event's `code` with `--json` (`unknown_job`, `unknown_workflow`, `config_invalid`, and so on). Fix the command in the skill, then run the default scenario again. Do not edit `goodflows.toml` to make it pass.
- `wait` exited `75`: no result yet. Run the same `wait` again. Never record `75` as the result.
- `wait` exited `1` with `stopped without a result`: the background run died, for example from SIGKILL. Record exit `1`, but do not name a failed job: report that the run stopped without a result, and quote the line with the `stderr.log` path it names.
- `wait` exited `2` with `unknown_run`: the `wait` named the wrong id or `--config`. Fix the `wait`, not the skill. The run is unaffected.
- The build or install line exited non-zero: stop. Fix the line if it was wrong and run again. If the CLI still cannot be built or installed, verification is blocked. Leave the build or install line next to the default scenario and say in the handoff that the binary was not produced and the skill was not verified.
- A job's command cannot start because the program it runs is missing (`command not found`, no compiler, no interpreter). That job is blocked, not passed. Name the missing program and quote the line that names it. Leave the job in the skill. Do not delete it to make the run exit 0.

An updated skill whose default scenario was never run is a draft. The handoff includes the outcome, the questions you asked (or that you asked nothing), each command and its exit code, and the verdict: verified with CI green, verified with CI red and the failing jobs, or blocked and why.

## 5. Maintenance

**unchanged** needs nothing further. **updated** is done once the report is in the conversation. Do not commit unless the user asks. Do not open a pull request from this skill. The next config change is another run of `maintain-ci-skill`.
