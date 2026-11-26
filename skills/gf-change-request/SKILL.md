---
name: gf-change-request
description: "Write the description file for a goodflows change request, the replacement for a pull request on any Git host, with change-claims/2 YAML front matter that goodflows parses and a body a reviewer can follow without reading the diff first. By default it writes the file only. With --approved it also creates or updates the change request through the goodflows CLI. It never pushes, opens a pull request, or calls a host API itself. Use when the user asks to write, create, or update a change request, a CR description, or the change claims for a branch, asks to describe a branch for goodflows, says the change request is already approved, asks to commit and write or create a change request, runs /gf-change-request, /gf-change-request --approved, or /gf-change-request --commit, or names gf-change-request."
---

# Change request

Write the description of a goodflows change request for the current branch. A change request replaces a pull request and works on any Git host. Write it for a reader who did not make the change. That reader may be another engineer, a reviewer, or a teammate who will operate the result.

The file has two parts. YAML front matter holds the claims goodflows parses: the commit they describe, the door, the reach, the facts the change is safe because of, the open risks, the questions for the reviewer, and the hunks the reviewer should judge. The Markdown body says the same things for a person.

By default this skill only writes the file. With `--approved`, it then hands the file to the goodflows CLI, which creates or updates the change request. See [Create it](#create-it). Either way, do not push, do not open a pull request, and do not call a host API through `gh`, `glab`, `curl`, or anything else yourself. Whatever reaches the host goes through the goodflows CLI. Do not commit unless the request includes `--commit`, and never stage or commit the file itself.

### Commit first

Read `--commit` from the request's prose. When the request includes `--commit`, or plainly asks to commit the changes before writing the change request, commit the working tree before anything else:

- Use the `gs-git-commit` skill when it is installed. Skip its last step, which asks whether to push, because this skill never pushes.
- Without it, stage the files that belong to the change by name, and leave out secrets such as `.env` and `credentials.*`, build output, editor and OS files, anything `.gitignore` covers, and anything under `.goodflows/` or `.verify/`. Make one commit for each unrelated change, with a conventional message, `type: what changed`, and no AI attribution footer. Ask the user before staging a file you are unsure of.

When the tree was already clean, there is nothing to commit, so go on. `--commit` never skips the clean-tree rule in Before writing. When the commit leaves files you chose not to stage, name them and stop, because the claims would describe a commit that is not the code on disk. `--commit` never creates the change request. Only `--approved` does.

## Before writing

Run these as separate shell calls.

```bash
git status --porcelain
git branch --show-current
git rev-parse HEAD
git symbolic-ref --short refs/remotes/origin/HEAD
git fetch origin
git log --oneline origin/<base>..HEAD
git diff origin/<base>...HEAD --stat
```

The base is the remote's default branch. The `symbolic-ref` call prints it as `origin/<base>`. Use `git remote show origin` when that call fails, and `main` when both fail. Compare against `origin/<base>`, not the local branch, which may be behind the remote. `git fetch` only reads from the remote, so it is allowed.

The claims describe one commit, so the working tree must be clean. When `git status --porcelain` prints anything, stop and tell the user to commit first, with the `gs-git-commit` skill when it is installed, or to run this skill again with `--commit`. Without `--commit`, do not commit for them. A caller that already forbids questions treats a dirty tree as blocked and does not commit, unless it passed `--commit`.

Read `GLOSSARY.md` when the repo has one, and use those names in the description.

Read enough of the diff to say what changed, who it affects, and the facts that make the change safe. Cite a real `file:line`. A search that finds no caller is still an answer. Do not invent a caller, a test result, or an API.

## Where the file goes

Write the file to `.goodflows/change-requests/<branch>/cr.md` at the repository root, where `<branch>` is the branch name exactly as `git branch --show-current` prints it, so each `/` in it becomes a directory. Every branch gets its own directory, and the file inside it always has the same name. Git never lets branches `a` and `a/b` exist together, so no branch's file can sit where another branch needs a directory, even when a deleted branch left its file behind. On a detached HEAD, use `.goodflows/change-requests/.detached/<short SHA>/cr.md`. No branch name can start a component with `.`, so that path never collides with a branch. Create the directories when they are missing.

The file must stay out of the commits it describes. Committing it would make a new commit, so the `head` it names would be stale the moment it landed. Keep it untracked without editing a tracked file: run `git rev-parse --git-path info/exclude`, and add the line `.goodflows/` to that file when it is not there. Do not edit `.gitignore`, because that dirties the tree the claims describe.

Do not write the file into a goodflows change directory, `goodflows/changes/<id>/`, even when the branch has one. That directory is tracked, and archiving the change moves it. Name the goodflows change in What changed instead.

## One file per branch, rewritten per head

The claims are per head. When the file already exists, read its front matter first, and keep its `cr` value.

- When the old `head` is the current HEAD, update the claims you can now prove better, and leave the rest.
- When the head changed and the change did not, such as after a rebase with no conflicts, a reworded commit, or a squash, keep the body and every claim, and rewrite only `head`. Keep each `evidence` entry too. The `run.md` it points to names the commit it was built from. The change is the same when `git diff <old-base>...<old-head> | git patch-id --verbatim` and `git diff origin/<base>...HEAD | git patch-id --verbatim` print the same ID, where `<old-base>` is `git merge-base origin/<base> <old-head>`. Use `--verbatim`, not `--stable`, which ignores whitespace and so misses a change inside a string or to indentation. When the old head is no longer in the repository, treat the change as changed. Even when the IDs match, check that each `decisions` hunk still appears in the new diff as written.
- When the change itself changed, write every claim again for the new head. Copy nothing from the old file except `cr`, and recheck each fact and its proof.

## The file

Run `goodflows schema show change-claims/2` and write the file from the template and rules it prints: the front matter's keys and values, quoting, and the body's headings. They match the goodflows that checks the file. If your goodflows has no `schema` command, tell the user it is older than this skill and needs updating, and stop.

Write the prose through the `gs-unslop` skill when it is installed. Without it, keep to these rules: sentence-case headings, straight quotes, no em dashes or emoji, a named actor, and no sentence that would still be true of any other change. Do not add an AI attribution footer.

### What goes in the front matter

The schema says what each key holds; these rules say how to fill it for this branch:

- `cr`: keep the value from the existing file, or take it from the user. Never guess it, and never ask a host for it. Before the change request exists it is `null`, and the goodflows CLI writes the number in when it creates the change request.
- `head`: what `git rev-parse HEAD` prints now.
- `safe_because`: at least one fact, even for a trivial change, such as "No code reads this file" with `proof: stated`. Give each fact the proof you reached, on the scale in What could break, not the one you meant to reach. A `ran` fact names at least one `evidence` entry, and every entry exists on disk; without evidence a fact is `walked` at most. Use a verify run only when the rule in Verify runs says it applies. Set `covers` as Spec scenarios no one can see says.
- `questions`: what the reviewer must decide that you could not settle from the code, the specs, or the request.
- `decisions`: at most three hunks a reviewer should judge: a tradeoff, a rejected alternative, a behavior that changes, or an edge case you resolved. Leave routine edits out. Copy each `hunk` from `git diff origin/<base>...HEAD -- <path>`, cut to the lines that carry the decision, with no line edited. Write each `note` as one sentence on why the hunk changed or what the reviewer should check.
- The body says the same things for the reviewer. When you change one, change the other.

### Title

The `#` heading is a conventional commit subject, `type: what changed`. Types are `feat`, `fix`, `chore`, `docs`, `refactor`, `test`, and `style`.

### What changed

Two or three sentences. Say what the user, operator, or system will do differently after merge. When the branch implements a goodflows change, name it here.

### How it works

Add the smallest picture that makes the point. One picture is enough. Add a second only when the first hides the point.

Choose and draw the picture with the `gs-show-me` skill when it is installed. The body holds Markdown only, so do not write an HTML file. Without that skill, pick one of these: pseudocode for a rule or algorithm, a call tree for the path a request takes, a component tree for UI structure, a shallow file tree when the layout is the change, a Mermaid sequence or flowchart when data moves between parts, or a diff sketch when the point is what was added or removed.

Keep only the calls, files, and states a reader needs. Put the picture next to the sentence it explains.

### Evidence

Show a before and after when one exists. A screenshot is the right evidence for a visible UI change. Otherwise show the command, the test, or the output. Name the exact check that failed before and passes now.

When nothing was run, write `Not run` and name the check that would show the change works. Do not invent output.

#### Verify runs

A project verification skill writes each run to `.verify/<app>/<run-id>/`, with a `run.md` index that names the commit SHA the app was built from. Look for these indexes before writing Evidence.

```bash
ls .verify/*/*/run.md
git rev-parse HEAD
git log -1 --format=%H origin/<base>..HEAD -- <changed files>
```

A run built from a dirty working tree tested code that no commit holds, so it never applies, whatever its commit. Among clean runs, a run applies when its commit is HEAD. It also applies when its commit is on the branch and comes after the last commit that touched the changed files, so the run built the same code. Check that with `git merge-base --is-ancestor <last-touching-commit> <run-commit>` and `git merge-base --is-ancestor <run-commit> HEAD`. Ignore runs for any other commit, and say nothing about them. When no run applies, leave verify runs out of the file.

When a run applies and its features cover the change, use it in Evidence:

- Say which features the run drove, by feature ID and entry point, and which passed or failed.
- Inline short text proof from the run's artifacts, such as a command and its output or a response snippet, in a fenced block.
- Put each screenshot in Evidence as a Markdown image with its path under `.verify/` and alt text that says what it shows. Leave the path as it is. The goodflows CLI uploads what the file references. Look at each screenshot first, and leave out one that shows a secret, a token, or a real person's data.

Never commit, stage, or push evidence files. They stay in `.verify/`, which the verification skill keeps in `.gitignore`.

### What could break

- Door. One-way when merge cannot be walked back, such as a destructive migration, sent mail or money, a published API, or deleted data. Two-way when a revert is enough.
- Reach. Name the callers, the data, or the people affected if the change is wrong.
- Safe because. State each fact that makes the change safe, with `file:line`, and how far you proved it, on the scale in the `gs-blast-radius` skill. `stated` means you only said it. `pointed` means you pointed at the line. `walked` means you walked the failure step by step and showed it cannot happen. `ran` means you ran code that proves it. Give a fact the proof you reached, not the one you meant to reach. In the body, write `unproven` beside a fact you did not run, and name the evidence beside a fact you did.
#### Spec scenarios no one can see

When the branch implements a goodflows change, look for that change's intent QA set: `qa-scenarios.md` in `goodflows/changes/<id>/`, or in `goodflows/changes/archive/<date>-<id>/` once the change is archived, or the file the user names, with `source: intent` and the same `change` in its front matter. Each `coverage` row with `status: no_check` is spec behavior a tester cannot observe, so a QA run never proves it. Add one `safe_because` fact for each such row, with `covers` set to the row's `capability`, `requirement`, and `scenario`, copied exactly, and the proof you actually reached. The row's `reason` says what the fact has to prove. A `qa-scenarios/1` set marks the same rows with a `no_check` key instead of `status`. When there is no intent set, skip this.

When a QA run proves a fact, such as a passed scenario that shows a guest cannot reach an admin page, the fact is `ran` and its `evidence` points at that scenario in the run's `run.md`. A failed or not run scenario proves nothing.

- Left open. Each remaining risk says how it breaks, the `file:line`, and who is affected. Write `None found.` when none remain.

### How to check

Give the smallest steps a reviewer can run to see the change.

### Questions and decisions

Under "Questions for the reviewer", give each question with its number, its options, and your recommendation marked. Under "Decisions to judge", give each decision as a `diff` block under a `### [id]. [path]` heading, with its note under the block. Write `None.` under a heading with nothing in it.

Before the file is final, read it again, including quoted code and command output. Remove secrets, tokens, absolute local paths, and private details. Check that the front matter parses as YAML, that `head` is the current HEAD, and that every `hunk` appears in the diff as written. When `goodflows cr validate --help` runs, also run `goodflows cr validate <file>`, and fix every error and warning it lists until it lists none. It checks the fields the CLI reads, not every claim the server checks.

## Create it

Read `--approved` from the request's prose. When the request includes `--approved`, or plainly says the change request is already approved, create or update the change request after the file is final. Without it, skip this section and go to Finish.

1. Run `command -v goodflows`. When it prints nothing, the CLI is not installed. Keep the file, say so, and go to Finish.
2. Run `goodflows cr validate <file>`, and fix every problem it lists before you create or update anything. `goodflows cr open --claims <file>` creates the change request, or reuses the branch's one draft or open change request, and writes its number into `cr`. `goodflows cr claims <file>` updates the change request whose number `cr` holds. `goodflows cr --help` describes both.
3. When `cr` is `null`, run `goodflows cr open --claims <file>`. When `cr` holds a number, run `goodflows cr claims <file>` instead, so a second change request is never made.
4. Read the file again. When the CLI wrote the change request number into `cr`, keep it. When `cr` is still `null` and the CLI printed the number, write that number into `cr` yourself, and change nothing else.

The CLI may push the branch or call the host as part of its own work. That is its job, not this skill's. Do not run a push or a host command to help it.

When a goodflows command fails, quote the command and its error. Read the file again before you retry. When `cr` now holds a number, the change request was created, so retry with `goodflows cr claims`, not `goodflows cr open`. Do not retry the same failing command unchanged.

## Finish

Print the path of the file, the `head` it describes, and whether you wrote new claims or only rewrote `head`. With `--commit`, also print each commit you made, or that the tree was already clean. With `--approved`, also print the change request number and any URL the CLI printed, or why the change request was not created. When the change request exists and the branch builds a goodflows change, say that the `gf-change-build` skill takes it the rest of the way, through scenario sets, verify runs, and the verdict. Without `--approved`, say that `/gf-change-request --approved` creates or updates the change request through the goodflows CLI, and do not run it. When `cr` is still `null`, say that the goodflows CLI writes it into the file when it creates the change request.
