# Ship prompt

Give this prompt to a fresh subagent after close commits. Replace `<CHANGE_ID>`, `<CHANGE_DIR>` (from preflight, or close's `ARCHIVED` path), `<TITLE>` and `<WRITERS>` (from preflight), `<VERDICT_ROWS>` (the agent's missing-input reasons from the last verdict, or `none` on the first round), and `<REPORT>` (`.goodflows/build/<CHANGE_ID>/ship-<n>.md`).

```text
You take the committed build of the goodflows change <CHANGE_ID> through its goodflows change request until the server returns a verdict. The server grades what you report against the project's policy and never merges. goodflows agents and each command's --help describe the commands; pass --json to read results.

Writer skills, from preflight: <WRITERS>. Each file has one writer: the claims come from gf-change-request, verify runs from the verify skill, the required workflow run from the run-ci skill, and feature files from gs-maintain-verification-skill. Find each SKILL.md at its path, read it, and follow its write or run mode for that step. Never write one of those files yourself or edit one to make an upload pass.

Change no code, spec, test or scenario. A failing scenario or workflow is a result, not something to fix here: upload it and report it; the next round fixes it. Upload every run you drive, passed or failed. Drive the intent set at most once per head: when .verify/ already holds a run folder whose run.md names HEAD as its commit, and no upload of it succeeded, upload that folder instead of driving again; when the server refuses it as already uploaded, read the refusal as uploaded. Never lower or edit the policy, weaken or remove a scenario, mark a scenario not run, drop a feature glob, run goodflows cr scenarios approve, run goodflows cr answer, merge, or take a merge lease. A person does those.

This round must also supply: <VERDICT_ROWS>.

1. The tree must be clean and HEAD must hold the build. When the branch has no change request, first run goodflows cr open --change <CHANGE_ID> --title "<TITLE>". Push with goodflows cr push -- <git push arguments, such as -u origin HEAD>; a plain git push leaves the server on the old head.
2. When goodflows cr scenarios list shows no intent set, upload <CHANGE_DIR>/qa-scenarios.md with goodflows cr scenarios upload <file>.
3. When the policy requires a workflow (goodflows policy get --effective), run it at HEAD through the run-ci skill with reporting history on, so the server receives the run for this head.
4. From the clean tree at HEAD, drive the intent set through the verify skill's run mode, unless a run folder for HEAD already exists (above). The run lists only non-empty evidence files; an empty capture is not evidence. goodflows cr validate <run folder>, then goodflows cr runs upload <run folder>.
5. When the branch changed files under a feature map's directory, or the verdict reports unmapped_area or feature_index_missing, follow gs-maintain-verification-skill, then goodflows features validate and goodflows features sync. A feature file change is committed and pushed with goodflows cr push, and then steps 3 and 4 run again for the new head.
6. Run gf-change-request for HEAD, then goodflows cr validate on the claims file. While its cr is null, goodflows cr open writes the number in; once cr is set, goodflows cr claims uploads it.
7. goodflows cr ready when the change request is a draft.
8. goodflows cr wait --until mergeable,needs_review,blocked --timeout 1800. Exit 75 means the timeout passed with the verdict still pending; that is a result, not a failure. Then read goodflows cr verdict --json.
9. When the state is needs_review, read goodflows cr review --json and copy each item with the answers it allows into your report.

A failed command is evidence: quote it and its error, fix the cause when it lies in how the command was run, and run it again. After a lost connection, check whether the push or upload happened before repeating it. When a cr open fails, read cr in the claims file first: a number there means the change request exists.

Write every command and result to <REPORT>, ending with the same block you return. Then end with exactly this block:

STATUS: done|blocked
REPORT: <REPORT>
CHANGE_REQUEST: <number>
HEAD: <sha pushed>
VERDICT: pending|blocked|needs_review|mergeable
REASONS:
- <rule> | <subject> | <message>
REVIEW:
- <item id> | <what a person must decide> | <the answers it allows>
BLOCKERS:
- <evidence, or none>
```
