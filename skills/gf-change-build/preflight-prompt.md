# Preflight prompt

Give this prompt to a fresh subagent before the build. Replace `<CHANGE_ID>` with the change id and `<REPORT>` with `.goodflows/build/<CHANGE_ID>/preflight-<n>.md`.

```text
You check that the goodflows change <CHANGE_ID> can be built and shipped unattended. Change nothing in the repository except your report file and .goodflows/build/<CHANGE_ID>/change-approved.md. Find every blocker, not just the first.

Check each of these:

1. goodflows is installed (command -v goodflows). Find the change: goodflows/changes/<CHANGE_ID>/, or, when that does not exist, the one directory under goodflows/changes/archive/ whose name is a YYYY-MM-DD- date followed by exactly <CHANGE_ID>. That directory is CHANGE_DIR.
   - Active: goodflows change validate <CHANGE_ID> exits 0, change.md has status: approved, and no id in its depends_on is still an active change (a directory directly under goodflows/changes/ other than archive/). When .goodflows/build/<CHANGE_ID>/change-approved.md does not exist, copy change.md there: it is the approved text the bars check compares the build against.
   - Archived: change.md has status: archived and the branch has commits after the base. An earlier run of this build archived it; the run resumes.
2. The current branch is not the default branch (git symbolic-ref --short refs/remotes/origin/HEAD names it). The base is git merge-base HEAD origin/HEAD.
3. git check-ignore confirms that .goodflows/ and .verify/ are both ignored. When either is not, it is a blocker: build reports, claims files and run folders would count as changed files and keep the tree from being clean.
4. The working tree is clean apart from paths under goodflows/changes/<CHANGE_ID>/ (gf-change-write leaves the change uncommitted, and close commits it), or .goodflows/build/<CHANGE_ID>/ holds a build-*.md report from an earlier run of this build, which then resumes. Any other dirty or untracked path without such a report is a blocker: someone else's edits would be built and verified as part of this change.
5. The goodflows config (goodflows.toml or .goodflows.toml) has a [reporting] block with a project, and the variable its token_env names (GOODFLOWS_API_KEY by default) is set. Check that it is set; never print its value.
6. goodflows cr status --json for the current branch. With no change request, record none. With one, its change_id must be <CHANGE_ID>: the link is set only when a change request is created, so a change request that links nothing or another change is a blocker. With one, also read goodflows cr scenarios list --json and record whether the intent set is missing, uploaded, or approved. An unapproved intent set is not a blocker.
7. The writer skills are installed. Search the harness's installed skills, then .agents/skills/, .claude/skills/ and .cursor/skills/ in this repository, then the user's skill directories, for: gs-change-request (writes the claims), a verify-<app> skill for this repository (drives QA scenarios), a run-ci-<app> skill for this repository (runs the goodflows workflows), and, when any verify-<app> skill has a features/ directory, gs-maintain-verification-skill. Record the path of each SKILL.md. A missing one is a blocker that names the skill and the file it writes.
8. For a resume, read the last line of .goodflows/build/<CHANGE_ID>/log.md: it names the last finished step, its report and its status.

Write what you checked and found to <REPORT>, ending with the same block you return. Then end with exactly this block:

STATUS: ok|blocked
REPORT: <REPORT>
CHANGE_DIR: <the change's directory>
STATE: active|archived
BASE: <full SHA of the merge base>
TITLE: <the change's first goal, as a short title>
APPROVED: <.goodflows/build/<CHANGE_ID>/change-approved.md, or missing>
CHANGE_REQUEST: <number or none>
INTENT_SET: missing|uploaded|approved
SIZE: <small or project, from change.md>
CAPABILITIES: <the capabilities list from change.md>
WRITERS: gs-change-request=<path>; verify=<path>; run-ci=<path>; maintain-verification=<path or not needed>
RESUME: <the last line of log.md, or none>
BLOCKERS:
- <each blocker with the command and its decisive output, or none>
```
