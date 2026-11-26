---
name: gf-change-merge
description: "Merge a goodflows change's change request into its target branch when a person asks: confirm the verdict is mergeable and the change is archived, take the target branch's merge lease, merge with a fast-forward or a merge commit, grade the merge result when the server asks for it, push, and report the merge to the server. Use only when the user asks to merge a change or its change request, such as \"merge validate-job-names\" or \"merge CR 12\". Not for building or shipping a change (gf-change-build), and never on its own initiative."
---

# Merge a goodflows change

This skill merges the change request of a goodflows change once its verdict is `mergeable`. It runs only when a person asks for this merge; their request is the approval. It never runs because a verdict turned `mergeable`, and gf-change-build never starts it.

The server grades and coordinates, but never merges: this skill merges on the Git host and reports the merged commit. `goodflows agents` lists the server conflicts each command can hit and the next step for each, and every command has `--help` and `--json`.

## Rules

- **Push only a mergeable verdict.** `goodflows cr merged` records a merge whatever the verdict, and marks one made without a `mergeable` verdict. Never get there: read the verdict right before pushing, and when it is not `mergeable`, stop and report its reasons.
- **Never clear a verdict the wrong way.** Do not lower or edit the policy, approve a scenario set, answer a review item, weaken a scenario, mark one not run, or force-push. A person does those, and the report asks them.
- **Fast-forward, else a merge commit.** When the target commit is an ancestor of the head, the target moves to the head itself, the commit the server graded. Otherwise create one merge commit with `git merge --no-ff`. Do not squash or rebase: both replace the graded commits with new ones.
- **Name everything explicitly.** Run every command from the main checkout's root (`<repo>`) unless a step says `<tmp>`. Pass `-n <number>` to every `goodflows cr` command, and use full SHAs, never `HEAD`: in the temporary worktree `HEAD` is whatever is checked out there, and a detached worktree has no branch for `cr` to find its change request by.
- **Keep the lease alive with a heartbeat loop.** A lease ends at its `expires_at`; a heartbeat sets it to now plus the lease's TTL. Right after acquiring, start the loop in step 3, and stop it at every way out. When the loop has died or has written `heartbeat-failed`, treat the lease as lost: the server dropped its merge-result check with it, so never push on it; restart.
- **Cleanup.** Cleanup means, in order: stop the heartbeat loop (`kill "$(cat .goodflows/merge/<number>/heartbeat.pid)"`, ignoring an error when it already exited), `goodflows cr lease release <lease id>` (ignoring `lease_not_active`), and `git worktree remove --force <tmp>` when `<tmp>` exists.
- **Before the push, every way out runs cleanup.** That covers a stop and a restart. A restart is cleanup, then step 3 again. Count restarts across the whole run, for any cause; after three, stop and report each cause with its evidence.
- **After the push, never release.** Once the target holds the merged commit, the only way out is `goodflows cr merged`, which records the merge and frees the slot. Releasing instead would record the merge as made without a lease.
- **Keep files outside the worktree.** Reports and run folders go under `<repo>` (`.goodflows/merge/<number>/` and `.verify/<app>/`), which removing `<tmp>` does not touch. `<tmp>` is a new directory from `mktemp -d`, outside `<repo>`.
- **Keep your context small.** Run each merge-result drive (step 5) in a fresh subagent that writes its report to a file and returns only its status block. Run the short commands yourself.
- **A failed command is evidence.** Quote it and its error, repair the cause when it lies in how the command was run, and run it again. After a lost connection, check whether a push or `cr merged` happened before repeating it: `git ls-remote origin <target>` and `goodflows cr status --json -n <number>` show it.

## 1. Find the change request

The user names a change id, a change request number, or neither (the current branch's). Check that `goodflows` is installed, that the config has a `[reporting]` block with a project, that the variable its `token_env` names is set with an `sk_` key (never print it), and that `git check-ignore -q .goodflows/ .verify/` succeeds, so this skill's files never dirty the tree.

- With a change id, use the current branch when its change request links that id, else a local or remote branch named after the id when its change request links it. When neither does, ask the user for the branch or the change request number.
- `goodflows cr status --json -n <number>` (without `-n` only to find the current branch's number) must show `.change_request` open with a `change_id`. With no linked change, this skill does not apply: say so, and point to the `gs-change-request-ship` skill's merge step. Keep `number`, `change_id`, `head` (the head SHA), `source_branch` and the target branch.
- Check out `source_branch`. The working tree must be clean and its HEAD must be the head SHA. Fetch first; when the local branch is behind `origin/<source_branch>` at that head, fast-forward it. Any other difference stops the merge: the server grades the head it was told about.

## 2. Check that it can merge

- `goodflows cr verdict --json -n <number>`: `.verdict.state` must be `mergeable`. Otherwise stop and report each of `.verdict.reasons` with who clears it: missing input and failures are gf-change-build's (run it again on the change), and approvals, review items and dependencies are a person's.
- The change must be archived at the head: `goodflows/changes/<id>/` must not exist, and a `goodflows/changes/archive/<date>-<id>/` must. An unarchived change would merge code without its durable specs; stop and say to run gf-change-build, which archives in the branch.

## 3. Take the lease

From the branch checkout, `goodflows cr lease acquire --json -n <number>` fetches the target and takes its merge slot for the head. Keep `.lease.id`, `.lease.base` (the target commit you merge onto) and `.lease.target_branch`. A retry by the same key for the same head returns the lease it already holds.

- `slot_taken`: another change request holds the slot until the time the message names. Wait until then and acquire again; after three tries, stop and report the holder.
- `not_mergeable`, `stale_head`, `not_open`: the state changed since step 2. Go back to step 1 once; if it repeats, stop and report the message.

Start the heartbeat loop at once. Its interval is a third of `.lease.expires_at` minus `.lease.acquired_at`, in whole seconds, at least 30. Start it detached from your shell, so it outlives the tool call that started it: with `setsid nohup`, as below, or with the harness's own way to keep a background process running.

```bash
mkdir -p .goodflows/merge/<number>
rm -f .goodflows/merge/<number>/heartbeat-failed
setsid nohup bash -c 'while sleep <interval>; do
  goodflows cr lease heartbeat <lease id> >/dev/null 2>&1 || { sleep 5; goodflows cr lease heartbeat <lease id> >/dev/null 2>&1; } || { date > .goodflows/merge/<number>/heartbeat-failed; exit 1; }
done' >/dev/null 2>&1 &
echo $! > .goodflows/merge/<number>/heartbeat.pid
```

The loop is alive when `kill -0 "$(cat .goodflows/merge/<number>/heartbeat.pid)"` succeeds and `heartbeat-failed` does not exist. Check that whenever a step below says to. A loop left running holds the slot with no limit, which is why cleanup stops it.

## 4. Merge locally

Make `<tmp>` with `mktemp -d` and work there, so the branch checkout stays as it is: `git worktree add --detach <tmp> <base>`.

- When `git merge-base --is-ancestor <base> <head sha>` succeeds, the merged commit is the head SHA: `git -C <tmp> checkout --detach <head sha>`, so `<tmp>` holds what will land. Nothing new is built.
- Otherwise, in `<tmp>`, `git merge --no-ff -m "Merge change request #<number>: <title> (<change id>)" <head sha>`, and the merged commit is the new `HEAD` of `<tmp>` (read its full SHA once). A conflict stops the merge: `git -C <tmp> merge --abort`, cleanup, and report the conflicting paths. Resolving them changes the branch, which this skill does not do: a person, or an agent they ask, merges the target into the branch and pushes it with `goodflows cr push`, then gf-change-build verifies and ships the new head.

Either way, `git -C <tmp> rev-parse HEAD` must now be the merged commit.

## 5. Grade the merge result when the server asks

Check that the loop is alive (otherwise restart), then read `goodflows cr verdict --json -n <number>` again. The server asks for a merge-result run whenever the lease base differs from the base the head was graded on, which can happen on a fast-forward too, such as after a stacked parent merged:

- `merge_result_ungraded` is the only rule in `.verdict.reasons`: drive each set whose `.verdict.scenarios.<source>.graded` is true (`intent`, `diff`, or both) on the merged commit, one fresh subagent per set, one after another, with the prompt below.
- `mergeable` with no reasons: skip to step 6.
- Anything else: cleanup, then stop and report it.

Fill in the placeholders, including `<repo>` (the main checkout's absolute path), `<source>` and `<app>`:

```text
Drive the goodflows <source> scenario set of change request #<number> on a merge result and upload the run. The merged commit is <merged sha>, checked out clean in the worktree <tmp>, made on the target commit <base> of branch <target>. A heartbeat loop keeps the merge lease alive; do not touch the lease.

1. Find this repository's verify-<app> skill (the harness's installed skills, then .agents/skills/, .claude/skills/ and .cursor/skills/ in <repo>, then the user's skill directories) and read it.
2. In <tmp>, do the setup its run mode needs that git does not carry, such as installing dependencies or copying environment files from <repo>. When the app cannot be launched, return blocked with the error and upload nothing: a launch failure is not a result.
3. From <repo>, get the set the run grades: goodflows cr scenarios list --json -n <number> | jq -j '.sets.<source>.current.content' writes its exact bytes (jq -j, because jq -r adds a newline). Its sha256 must equal .sets.<source>.current.digest; when it does not, or content is null, return blocked. This file is the run's scenarios.md.
4. Follow the verify skill's run mode from <tmp> against that set. Change no code, spec, test or scenario, never mark a scenario not run, and list only non-empty evidence files.
5. The verify skill writes run.md. Its commit must be <merged sha> and its dirty must be false: when either is not, return blocked with the value it recorded and, for dirty, the paths git status --porcelain shows in <tmp>. Never edit commit or dirty. Then add only these to its front matter: merge_result: {base: "<base>", commit: "<merged sha>"}, and set branch: "<target>". Move the run folder to <repo>/.verify/<app>/ when it was written under <tmp>.
6. From <repo>, goodflows cr validate <run folder>, then goodflows cr runs upload <run folder> -n <number>.

Write every command and result to <repo>/.goodflows/merge/<number>/merge-result-<source>.md, ending with this block, and return only the block:

STATUS: done|blocked
REPORT: <report path>
RUN: <run folder>
RESULTS:
- <scenario id> | passed|failed|not_run | <evidence path>
BLOCKERS:
- <evidence, or none>
```

A `blocked` drive is a cleanup and a stop. After the drives, check that the loop is alive; when it is not, the server may have dropped its merge-result check, so restart. Then read the verdict again. `mergeable` goes on to step 6. `merge_result_failed`, or any other reason, is a cleanup and a stop, reported with the failing scenarios and the run's evidence. The branch then needs the target merged into it before gf-change-build can verify and fix it, which a person decides, because the failure is in how the branch combines with the target, not in the branch alone.

## 6. Push and report the merge

Right before pushing, check all three, in this order: the loop is alive, `goodflows cr lease heartbeat <lease id>` succeeds (otherwise restart for either), and `.verdict.state` of `goodflows cr verdict --json -n <number>` is `mergeable` (otherwise cleanup and stop). The heartbeat gives at least a full TTL for the push. Then push the merged commit without force: `git push origin <merged sha>:refs/heads/<target>`.

- Rejected because the target moved (non-fast-forward): restart.
- Rejected by branch protection, such as a rule that the target only takes pull requests: cleanup, then stop and report the host's message. Merging then needs the Git host's own merge flow or a person.

Once the push lands, run `goodflows cr merged -n <number> --commit <merged sha> --target <target> --lease <lease id>`. It records the merge and frees the slot; repeating the same report is safe, so on a failure retry it with the loop still running. When it still fails after three tries, stop the loop without releasing and report that the merge landed but is not recorded, with the exact command to run.

After it succeeds, stop the loop and remove the worktree. Do not delete the change's branch unless the user asked for that.

## 7. Report

Say, in this order:

1. The change request number, the change id, and the merged commit on the target, with how it merged (fast-forward or merge commit).
2. The verdict at merge, and each merge-result run when step 5 ran.
3. Anything that stopped the merge, with the command and its decisive output, and who acts next. When the stop came after the lease was taken, say whether the lease was released, and how many restarts the run took.
4. That the target branch's CI now runs on the merged commit. Watch it when the harness can follow CI, and report a failure with its job and log line.
