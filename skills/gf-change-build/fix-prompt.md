# Fix prompt

Give this prompt to a fresh subagent when sorted findings have `code` or `spec` rows. Replace `<CHANGE_ID>`, `<CHANGE_DIR>` (from preflight, or close's `ARCHIVED` path once archived), `<SKILL_DIR>` (this skill's directory), `<FINDINGS>` (`.goodflows/build/<CHANGE_ID>/findings-<n>.md`, the rows to fix, each followed by its verify report path), `<COUNT>` (the number of rows in it), `<HELD>` (`.goodflows/build/<CHANGE_ID>/held.md`), and `<REPORT>` (`.goodflows/build/<CHANGE_ID>/fix-<n>.md`).

```text
You fix every finding in <FINDINGS> for the goodflows change <CHANGE_ID>. Expected finding count: <COUNT>. Each row is: id | severity | subject | file:line | what is wrong | the fix | route, followed by the path of the verify report it came from; read that report for detail.

Read <CHANGE_DIR> and the section "Who owns the spec" in <SKILL_DIR>/SKILL.md first. <HELD>, when it exists, lists what waits on a person: build nothing and edit nothing for a subject it lists as a question.

- route code: change the code, and add or fix the test that would have caught it.
- route spec: change the text of the requirement already in the delta, and record a by: agent decision whose why starts with "Found while building:", cited on the Refs: line of the requirement it changed. When the change is archived (<CHANGE_DIR> is under goodflows/changes/archive/), edit goodflows/specs/<capability>/spec.md directly instead, and say so in your report. Write the requirement text and the decision through the gs-unslop skill when it is installed; it changes wording only, never an id, a name or SHALL.
- When the current code or spec already does what a row asks, say so and count it as fixed.
- When a fix turns out to need an edit "Who owns the spec" gives a person, such as adding a requirement or editing qa-scenarios.md, do not make it: list the row as unfixed with the question and the text you would recommend.

Fix every row. Do not drop, defer or re-rank one, and do not weaken a test or a check to make it pass. Run the tests and checks that show each fix works. After three attempts at the same row without progress, stop on it and list it as unfixed with the evidence.

Do not commit, push, archive, or run goodflows cr commands.

Write to <REPORT> what you changed for each row, with file paths and the checks you ran, ending with the same block you return. Then end with exactly this block:

STATUS: done|unfixed|blocked
REPORT: <REPORT>
FINDINGS: <COUNT>
FIXED:
- <row id> | <subject> | <what changed, or already in place>
UNFIXED:
- <row id> | <subject> | <why, with evidence> | <the question for a person, or none> | Recommend: <option, or none>
```

`done` needs one FIXED line per row and no UNFIXED lines. When a `done` result does not account for every row, continue the same subagent with the missing ids. `blocked` means the fix cannot go on at all, such as a broken build environment it could not repair.
