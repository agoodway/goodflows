---
name: create-ci-skill
description: "Generate a project-local CI skill (run-ci-<app>) for this repository. Use for /create-ci-skill, \"make a CI skill for this repo\", or when agents don't know what CI runs, how to run it locally, or how to read a failure."
disable-model-invocation: true
---

# Create a CI skill

Agents working in a repo need a scripted account of what CI runs, in what order, how to run it on this machine, and how to read a failure. This skill writes that account as a project-local skill (`run-ci-<app>/`) the next agent can follow cold, mid-task, without having seen the repo.

Write that skill in the directory the harness you are running in loads. Decide from the harness, not from which dot-directories the repo already contains:

- Amp, Cursor, OpenCode, Codex, and T3 Code: `.agents/skills/run-ci-<app>/`
- Claude Code: `.claude/skills/run-ci-<app>/`
- Grok CLI: `.grok/skills/run-ci-<app>/`

If the harness is unknown, use `.agents/skills/run-ci-<app>/`. Write one directory. Every file below is inside it.

`<app>` is a slug of the repository name (`git remote get-url origin`, else the checkout directory): lowercase, every character outside `a-z`, `0-9`, and `-` replaced with `-`, repeats collapsed, ends trimmed. If CI is split per package and this run is about one package, slug that package's name instead. One invocation writes one skill.

## 1. Interview the repo, not the user

Answer these from the checkout. Ask the user only for a fact you cannot observe (a secret value, an external policy that is not in the repo). Never write a secret value into the skill; name the variable.

Look, then read what exists:

- **GoodFlows:** `goodflows.toml`, then `.goodflows.toml`. The CLI uses the first of those that exists, unless a command passes `--config`. Read the file. Top-level `[[jobs]]` are the `default` workflow. `[[workflows.NAME.jobs]]` are named workflows. Record each job's `name`, `command` or `steps`, `depends_on`, `workflow` (a call to another workflow), and `condition`.
- **Hosted CI:** `.github/workflows/*.yml`, and also `.gitlab-ci.yml`, `.circleci/config.yml`, `bitbucket-pipelines.yml`, `azure-pipelines.yml`, `Jenkinsfile`, `.travis.yml`, `.buildkite/`, `appveyor.yml` when present. For each workflow record the `on` triggers, job names, `needs`, matrix, `timeout-minutes`, `services`, `env`, `${{ secrets.* }}` names, cache steps, and the exact `run` lines.
- **Local checks:** `Makefile` targets, `package.json` `scripts`, `justfile`, `Taskfile.yml`, `Cargo.toml` / `mix.exs` / `go.mod` / `pyproject.toml` only to identify the toolchain those commands need.
- **Hooks and bots:** `.pre-commit-config.yaml`, `.husky/`, `lefthook.yml`, `dangerfile*`, review workflows, `CODEOWNERS`, `renovate.json`, `dependabot.yml`. Say whether a bot fails the check or only comments.
- **Required checks:** `gh api repos/<owner>/<repo>/rulesets` and branch protection for the default branch, when `gh` is authenticated. If the call fails, say the required set was not readable. Do not invent a required check.
- **No hosted CI:** when none of the hosted-CI files exist, say so. Local checks and review bots are the whole gate. Write them as such.

Also read the README sections that mention CI, tests, or goodflows. Note a cache only CI restores, a service container, and an env var a local run needs. A missing secret or service is a fact the generated skill states; it is not a reason to drop the job.

When the repo has a GoodFlows config, local runs of those jobs go through the `goodflows` CLI. Read the config for the job names. Do not retype a job's `command` as the way to run it.

| Intent | Command |
|---|---|
| Default workflow | `goodflows` (same as `goodflows run`) |
| Named workflow | `goodflows run NAME` |
| Named jobs plus their `depends_on` | `goodflows --jobs a,b` |
| Parse only, run nothing | `goodflows validate` |
| Print jobs | `goodflows list` |
| Other config file | `--config PATH` on the command above |
| Machine-readable result | `--json` (JSON Lines; the last `summary` event has `success`) |

Exit `0` means every selected job passed or was skipped. Exit `1` is a failed job, a timeout, or a config error. Exit `2` is bad arguments. `goodflows list` confirms names when the binary is already installed. Parsing the TOML is enough when it is not.

## 2. Generate the skill

Write `SKILL.md` with YAML frontmatter. Without it the skill never registers.

```yaml
---
name: run-ci-<app>
description: "Use when running checks, inspecting a pull request, or reading a CI failure in <App>."
---
```

`name` matches the directory name. The description names the app in words a person would use and states when to reach for the skill. Double-quote it. No placeholders, no `TODO`, no angle-bracket tokens remain anywhere in the file.

These sections, in this order, each filled from the interview:

- **What CI runs, and when.** Every hosted workflow that exists: its file, its triggers, and its jobs in `needs` order. Name the matrix cells (OS, language version). Separate tag-only or dispatch-only workflows from pull-request jobs. If there is no hosted CI, say that in the first sentence and describe the local checks and bots that stand in for it. When a GoodFlows file exists, name the file, the workflow, and each job `name` with the command the config stores. A hosted step whose shell matches a GoodFlows job is that job, not a second check.

- **Run it locally.** Exact commands from the repo root. When a GoodFlows config exists, this section leads with `goodflows` and a table:

  | Job | Command in the config | Invocation that runs it |
  |---|---|---|

  The invocation is `goodflows`, `goodflows run NAME`, or `goodflows --config PATH …` with that job's `name`. One invocation covers every job in that workflow. When the hosted shell is the same command or a weaker subset of the config command, do not also schedule that shell; quote both so the difference is visible. When the hosted shell is stricter or different, keep the `goodflows` invocation and add the hosted command the config does not cover. Any other hosted step gets its own row and its own command, copied from the workflow. Include the command that builds or installs `goodflows` when CI does that, or when the binary is not on `PATH`. If CI runs `./target/release/goodflows`, the local command is that path after the same build CI runs. If the repo documents an install, use it. If nothing does, install the CLI the way the goodflows README states: `cargo install --git https://github.com/agoodway/goodflows.git --locked`, and say the repo did not pin a version.

  End the section with `### Proof commands` and one fenced `bash` block. That block is the merge gate, in order, copy-pasteable from the repo root: toolchain install the way CI does it, the `goodflows` invocation, then hosted steps that are not GoodFlows jobs. Tag-only release jobs, secret-gated publishes, and matrix cells that are a different OS stay in prose above the block, with the reason they are not in it. They are not deleted from the skill.

- **CI status on a pull request.** The exact `gh` commands for this repo (`gh pr checks`, `gh run list --branch`, `gh run view <id> --log-failed`). Include the Actions URL `https://github.com/<owner>/<repo>/actions`. If `gh` is not authenticated, say status was not queried from here.

- **Read a failure.** How to tell a failed job from a skipped one. For GoodFlows, the text report's `Failures` section and the final `Rerun failed:` line, and the `--json` summary's `stdout_tail` / `stderr_tail`. Name the rerun: `goodflows --jobs <failed-names>` (it also runs `depends_on`). For every other check, the command and the first thing its failing output means (a formatter diff, a lint warning treated as an error, a timeout). Only name a failure mode the command or a real log supports. Do not invent a past flake.

- **Green before merge.** The pull-request jobs, plus required checks you actually read. A tag-only workflow is not part of green. State the OS and toolchain this file's proof ran, and the matrix cells it did not.

- **Keeping this skill accurate.** When `goodflows.toml`, `.goodflows.toml`, the workflow files, the Makefile, package scripts, or the pre-commit config change, re-read them and edit the job names and the proof block so they match. Then run the proof block again. There is no `maintain-ci-skill` command to run; this section is the maintenance.

A helper script is allowed only when a command in the proof block cannot be written inline. It is executable, and the proof block shows how it is invoked.

## 3. Prove the generated skill before handing it over

Run `### Proof commands` from the repo root, one command at a time, in order. Record the command, the exit code, and one line that shows pass or fail.

- The check ran and exited non-zero: stop. Fix the skill if the command, the path, or the job name was wrong, then run the whole block again from the first command. If the repo itself fails its real check, report that failure. Do not weaken a flag, drop a job, or mark it skipped to produce a green report.
- The command cannot start: a missing executable, a secret that is unset, or a `services:` dependency this host does not have. Write the exact missing piece in the report and in the skill. That command is blocked, not passed. Continue only with later commands that do not need its outputs.
- Do not claim a matrix cell you did not run. The proof runs the block once on this machine.

A generated skill that was never run is a draft, not a deliverable. The handoff includes the report: each command, its exit code, and any command left blocked.

## 4. Maintenance

Point the user at the generated skill's `## Keeping this skill accurate` section. That note is how the skill stays true when CI config changes. `maintain-ci-skill` is a separate skill and is not part of this one; do not tell the user to run it.
