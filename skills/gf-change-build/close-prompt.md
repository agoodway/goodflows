# Close prompt

Give this prompt to a fresh subagent once the fix loop leaves no `code` or `spec` rows. Replace `<CHANGE_ID>`, `<CHANGE_DIR>` (from preflight, or the `ARCHIVED` path of an earlier close), `<RUN_CI_SKILL>` (the run-ci skill path from preflight), `<HELD>` (`.goodflows/build/<CHANGE_ID>/held.md`), `<VERIFY_REPORTS>` (the paths of the latest verify reports), and `<REPORT>` (`.goodflows/build/<CHANGE_ID>/close-<n>.md`).

```text
You close the build of the goodflows change <CHANGE_ID>: run the full check gate, record what was verified, archive the change when nothing is open, and commit. Do not push and do not run goodflows cr commands.

When <CHANGE_DIR> is under goodflows/changes/archive/, the change is already archived and this close follows a fix the verdict forced: run only steps 2 and 5, and say in your report that the archived verification.md predates this fix, because goodflows change verified does not run on an archived change.

1. Run goodflows change validate <CHANGE_ID>. An error is a finding row (below); stop there.
2. Run the repository's full check gate locally through the skill at <RUN_CI_SKILL>. Each failure is a finding row; stop there. Never skip, weaken or reconfigure a check to make it pass.
3. Run goodflows change verified <CHANGE_ID>. It writes <CHANGE_DIR>/verification.md with the hashes of the deltas and of every file the branch changed, and keeps an existing open list and body. Then edit only its open list and its body:
   - open lists each line of <HELD> as one string, or is open: [] when <HELD> does not exist or is empty;
   - the body is a short summary of what each verifier checked, from <VERIFY_REPORTS>, and every check reported not_verified with its reason.
   Editing these does not change the recorded hashes; goodflows change verified keeps them on a later run.
4. When open is empty, run goodflows change archive <CHANGE_ID>. It merges the deltas into goodflows/specs/ and moves the change to goodflows/changes/archive/<date>-<CHANGE_ID>/. When it refuses, report its error as a blocker; do not edit verification.md hashes by hand. When open is not empty, do not archive.
5. Commit everything the build changed, the change directory and goodflows/specs/ included, with a conventional commit whose subject names the change, such as "feat: add login (add-login)". Leave out .goodflows/, .verify/ and anything .gitignore covers. Add no AI attribution.

Write what you ran and its results to <REPORT>, ending with the same block you return. Then end with exactly this block:

STATUS: done|failed|blocked
REPORT: <REPORT>
ARCHIVED: <the goodflows/changes/archive/... path, or no: open findings, or no: already archived>
COMMIT: <sha> <subject, or none>
ROWS:
<id> | <severity> | <subject> | <file:line> | <what failed> | <the fix> | code
BLOCKERS:
- <evidence, or none>
```

`failed` means the gate or validate produced rows (number them `close-1`, `close-2` and so on, with the file path as the subject when no requirement applies); nothing is committed, and the rows go through fix and re-verify before close runs again. With no rows, write `ROWS: none`.
