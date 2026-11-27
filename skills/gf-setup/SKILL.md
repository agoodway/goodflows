---
name: gf-setup
description: "Set a repository up for goodflows change requests and write its first review policy: check the goodflows config, the [reporting] project and key, and the installed skills, then derive each review-policy/1 setting from the repository (which files evidence.ignore_paths may skip, required workflows, sensitive paths, size limits, scenario and coverage gating), decide the open ones with the user, commit goodflows/review-policy.toml, and write it with goodflows policy set once the user approves. Use for /gf-setup, \"set up goodflows\", \"write a review policy\", or \"configure change requests\" in a repo. Not for tuning an existing policy from calibration data, writing CI jobs (goodflows init, create-ci-skill), or planning a change (gf-change-write)."
---

# Set up goodflows change requests

This skill gets a repository ready to land work through goodflows change requests, and writes its review policy from what the repository holds. The policy is the main output. Every other setup step is a check: when one fails, name the command or skill that owns it and let the user run it.

A review policy is a project's `review-policy/1` document on the GoodFlows server. It decides which verdict each rule produces for a change request. A project that never wrote one uses version 0, a default meant for no repository in particular. Run `goodflows schema show review-policy/1` before writing anything: it prints version 0 as a TOML template and every section, key, allowed value and glob rule. Never write a key it does not list; the server refuses unknown keys.

## Rules

- **Evidence for every setting.** Each value you propose comes with what in the repository supports it: a file and line, a command and its output, or a count from git history. A value you cannot support stays at its version 0 default, and you say so.
- **The user decides; you find facts.** Look up everything the code, config and git history can answer. Ask the user only for decisions, in rounds, each question with your recommendation worded so "yes" accepts it.
- **Write the server policy only with approval.** `goodflows policy set` changes shared server state for everyone on the project. Run it only after the user approves the final file in a message of their own. Accepting recommendations is not that approval.
- **Never print a key.** Check that the variable `token_env` names is set and starts with `sk_`; never echo its value.
- **Do not redo other tools' setup.** `goodflows init` writes the config, `goodflows skills install` installs skills, create-ci-skill writes the `run-ci-<app>` skill. Point at them; do not reimplement them.

## 1. Check the setup

Report each check as passed or failed, with the fix for a failure:

- `goodflows --version` works. Otherwise point to the install docs.
- `goodflows.toml` or `.goodflows.toml` exists at the root. Otherwise: `goodflows init`.
- Its `[reporting]` block names a `project`, and the variable its `token_env` names (default `GOODFLOWS_API_KEY`) holds an `sk_` key. Writing the policy also needs that key to belong to an account owner or admin; a plain member gets `403` from `policy set`. Say this now, so the user does not find out at the end.
- `git check-ignore -q .goodflows/ .verify/` succeeds. The change request skills write reports and run folders there, and a tracked file there dirties the tree they describe.
- `goodflows skills install` has run: the gf-change-* skills are in the harness's skills directory and `AGENTS.md` has the `<!-- goodflows:formats -->` block. A `run-ci-<app>` skill exists, or suggest create-ci-skill.

A failed check blocks only the steps that need it. Surveying the repository (step 3) needs none of them, so keep going and list the blockers in the final report.

## 2. Read the current policy

- `goodflows policy get --json`: keep `policy.version` (you pass it as `--previous-version` later), `policy.policy` (the document as written) and `policy.active_settings`.
- `goodflows policy get --effective --json`: every setting with defaults filled in.
- When `goodflows/review-policy.toml` exists, compare it with the stored document and report any difference.
- When the version is above 0, this is not a first setup. Start from the stored document, and propose only changes the survey supports, as a diff. Tuning a policy from `goodflows policy calibration` is a separate job; say so when the user asks for it.
- A setting missing from `active_settings` has no effect on this server. Do not spend a question on it; mention it once.

## 3. Survey the repository

Run the surveys below, using subagents for the wide searches. Each produces a proposed value with its evidence.

A landing is one first-parent commit of the default branch: a merge commit, or a commit pushed straight to it. Many repositories have few merges, so measure landings, never only `--merges`. `replay.py`, next to this file, reads them with rename detection, as the change request's Git facts are: run `python3 <this skill's directory>/replay.py --help`. It needs Python 3.11 or later. Use it instead of writing your own `git log` parsing; a hand-written parser easily drops renamed files or cuts their paths.

### evidence.ignore_paths

This is the setting most likely to be wrong, and the most dangerous to get wrong. A verify run on an earlier commit still counts for the head when every later commit changes only ignored files, and ignored files are left out of the feature coverage mapping. So an ignored file that the build reads lets code reach the target branch untested. It does not change the size limits or `paths` rules: ignored files still count and still match.

The question for every candidate glob is: can these files change without changing what a verify run tested?

1. List tracked files with `git ls-files` and group them by top-level directory and extension. The candidates are documentation and repository metadata: `docs/`, `*.md`, `CHANGELOG*`, `LICENSE*`, `.github/ISSUE_TEMPLATE/**`, `.github/*.md`, editor config.
2. For each candidate, search for anything that reads those files at build, test or run time. A match rules the glob out, or narrows it. Look for:
   - Embedding: Rust `include_str!`/`include_bytes!` and `#![doc = include_str!(...)]`, Go `//go:embed`, Elixir `@external_resource` and `File.read!` at compile time, `import x from "./x.md"` or `?raw` in bundlers, Python `importlib.resources` and `package_data`.
   - Loading at run time: `priv/`, `static/`, `public/`, templates, files a Dockerfile `COPY`/`ADD` puts where the app reads them.
   - Testing: doctests over a README, snapshot or golden files with a documentation extension, tests that read docs, link checkers or doc builds that run as a required workflow.
   - Agent and skill files: `SKILL.md`, `AGENTS.md`, prompt files. When the product loads them, they are code. A verify skill's own files decide how a verify run drives the app, so keep them out too.

   A file that is only packaged, such as a README or LICENSE that a release archive, `package.json` `files`, `MANIFEST.in` or a Hex `package` copies, changes no behavior a verify run checks and may be ignored. Name it as packaged in the evidence, so the user can decide otherwise.
3. Version 0 is `["docs/**", "**/*.md"]`. Check both globs against the repository. `**/*.md` is wrong for any repository that embeds or ships a Markdown file; when that is the case, replace it with the specific Markdown paths that are safe, such as `README.md` (unless it is a doctest) and `CHANGELOG.md`.
4. Write the result explicitly, even when it equals version 0, so the stored version records the decision.

Show the user each proposed glob with the files it matches and the searches that cleared it, and each rejected glob with the file and line that ruled it out.

### evidence.required_workflows

List the workflows in the goodflows config. A workflow belongs here only when a run of it is uploaded for every change request head; `goodflows history list --workflow <name>` shows whether runs arrive and on which branches. Requiring a workflow nobody uploads leaves every change request `pending`. Propose the workflows that run the project's checks, such as `ci`, and say which uploads you saw.

### paths

Sensitive paths depend on the language and layout. Look for database migrations, CI definitions (`.github/workflows/**`, the goodflows config itself), auth, permissions and crypto modules, payment and billing code, release and deploy scripts, infrastructure definitions, and dependency lockfiles. Always propose `goodflows/review-policy.toml` with `needs_review`, so a policy change goes through a person.

Recommend `needs_review` for each; `blocked` only when the user asks for it. For each glob, say how often it would fire; the replay in step 5 counts it per landing. Check every glob against the syntax `schema show` prints: no `[ ] { } \`, and `**` only as a whole segment.

### size

Run `replay.py --sizes --branch origin/<default branch>`. It prints files and lines (additions plus deletions) per landing at several percentiles, and how many landings some limits would flag. Propose limits near the 90th percentile, rounded up, so ordinary changes pass and outliers get a person, and say how many past landings your limits flag next to version 0's count. With fewer than 30 landings, keep version 0 and say why.

### scenarios

Opt in only when the repository uses goodflows changes: `goodflows/changes/` exists and the gf-change-write and gf-change-build skills are installed. Then propose `require_approved_intent = true` and ask about `spec_gaps = "needs_review"`. Otherwise keep the defaults; turning gating on in a repository that never links changes holds every change request `pending`.

### coverage

Keep both settings `ignore` unless a verify skill has feature files with `paths` and `goodflows features sync` has run. Without a feature index, any other value makes every change request `pending` with `feature_index_missing`. When an index exists, propose `needs_review` for `unmapped_areas` and ask about `undriven_features`.

### claims and merge

Keep the defaults unless the user asks. Mention what each does in one line.

## 4. Decide with the user

Play back the survey in one message: each section, its proposed value, and the evidence, with version 0 next to it where they differ. Then ask the open decisions as one numbered round, each with a recommendation. Recompute after each answer and ask the next round until none is left. Decide wording and file layout yourself.

## 5. Write and check the file

Write `goodflows/review-policy.toml`, starting with comment lines that name what each non-default value is based on. Then check it before anything leaves the machine:

- It parses as TOML, and every section and key appears in `goodflows schema show review-policy/1`.
- Globs follow the syntax rules, are distinct, and stay within 100 `ignore_paths` and 100 `paths` entries.
- **Replay it against history.** Run `replay.py goodflows/review-policy.toml --branch origin/<default branch>`. It lists the last 50 landings each `size` and `paths` rule flags, the landings an earlier verify run would cover because they change only `ignore_paths` files, and the landings version 0 would cover but this policy does not. Read every landing in the second list: one that changed something a build reads means a glob is wrong, so fix it and replay again. Show the third list to the user; it is the evidence for each glob that version 0 had and this policy drops.

## 6. Commit and set it

Show the final file and the replay, and ask for approval to write it. After the user approves:

1. Commit `goodflows/review-policy.toml` with a conventional message, such as `chore: add goodflows review policy`. Push only when the user asks.
2. Run `goodflows policy set goodflows/review-policy.toml --previous-version <version from step 2> --note "<one line: why>"`. A `409` means another write landed first: read the policy again, show the user what changed, and redo the decision.
3. Run `goodflows policy get` and show the version written and its active settings.

## 7. Report

Say which checks failed and who fixes them, the policy version written (or that it was not, and why), each setting that differs from version 0 with its one-line reason, and the settings left at version 0 that the user may want to revisit once the project has merged change requests.
