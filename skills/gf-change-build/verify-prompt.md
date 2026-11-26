# Verify prompt

Run the four checks below at the same time, each in its own fresh subagent with the shared text plus that check's text. Replace `<CHANGE_ID>`, `<CHANGE_DIR>` (from preflight, or close's `ARCHIVED` path once archived), `<SKILL_DIR>` (this skill's directory), `<BASE>` (from preflight), `<HELD>` (`.goodflows/build/<CHANGE_ID>/held.md`), `<APPROVED>` (the `APPROVED` path from preflight), `<VERIFY_SKILL>` (the verify skill path from preflight, used by the `qa` check), `<REPORT>` (`.goodflows/build/<CHANGE_ID>/verify-<check>-<n>.md`), and `<SCOPE>`: "the whole branch" for the first round; after a fix, "the repair in <fix report path>, against the finding rows in <findings path>"; after a verdict, "the verdict reasons in <ship report path>".

Shared text:

```text
You verify the goodflows change <CHANGE_ID> against the code on the current branch, for <SCOPE>. You did not build it, and you do not see the builder's reasoning; judge from the files. Do not edit any file except your report.

Read <CHANGE_DIR>: change.md (its YAML front matter holds the goals, non-goals, anti-goals, the decisions, each by: user or by: agent, the door field, and left_to_builder), the spec deltas under specs/, and qa-scenarios.md. Read the durable spec of each capability it lists, goodflows/specs/<capability>/spec.md, and the section "Who owns the spec" in <SKILL_DIR>/SKILL.md. The branch's work is git diff <BASE> plus untracked files (git status). <HELD>, when it exists, lists what is already waiting on a person. Do not report a subject it lists as a question, such as a requirement the builder set aside: it is unbuilt on purpose. For a subject it lists as not_verified, run that check again and report it only if it still cannot run.

Before you claim that something does or does not exist in the code, search for it. A check you could not run is reported as a row with severity not_verified and the reason; it never counts as passing. When unsure of a severity, pick the lower one.

Write your full findings to <REPORT>, ending with the same rows and block you return. Then end with one row per finding and this block:

ROWS:
<id> | <critical, warning, suggestion or not_verified> | <subject> | <file:line> | <what is wrong> | <the fix> | <route>
STATUS: done|blocked
REPORT: <REPORT>

Number rows <check>-1, <check>-2 and so on. subject is the requirement name, or the file path when the finding concerns no requirement; it lets later rounds recognize the same finding. route is code (the code changes), spec (an edit to a requirement's text that "Who owns the spec" gives the builder), or person (anything else "Who owns the spec" reserves for a person, including a requirement the delta would need to add). With nothing to report, write ROWS: none.
```

Check `spec`, spec to code:

```text
Check from the spec to the code. Each ADDED and MODIFIED requirement is built and does what its current text says, including its spec scenarios. Each REMOVED requirement's behavior is gone: read its old text in the durable spec, or in git log -p of that spec when the change is already archived, and search for that behavior, never just the name; code that still delivers it is a critical row. Each RENAMED requirement keeps its behavior. A requirement that is missing or only partly built is critical; one built differently from its text is a warning, routed code when the spec is right and spec when the code is right and the edit is the builder's.
```

Check `drift`, code to spec:

```text
Check from the code to the spec. Walk the diff and list every behavior a user or another system would notice: a refusal, a fallback, a limit, a default, a wire field, an exit code, a header sent or not sent, data stored or deleted, an error case. For each, find the requirement that states it. When none does, it is a row: either the spec has a gap or the code does more than the change asks (route code: remove it). A gap that the text of a requirement already in the delta can cover is route spec, with the text to change; a gap that needs a new requirement, or a durable requirement the delta does not touch, is route person, with the requirement text you would recommend. Pin only behavior where a wrong guess would change what users or other systems get; names, file layout and the wording of output only people read never become rows. While the change is active, a delta edit the builder made must be recorded as a by: agent decision whose why starts with "Found while building:" and cited on that requirement's Refs: line; one that is not is a spec row. Once the change is archived, an edit to goodflows/specs/ has no Refs: line to cite; check instead that the fix report named in <SCOPE> records it.
```

Check `bars`, decisions and bars:

```text
Check the change's decisions and bars. Every by: user decision is followed as written, and none was edited by the build (diff <APPROVED>, the change.md that was approved, against <CHANGE_DIR>/change.md; when <APPROVED> is missing, report this part as a not_verified row). No anti-goal is violated and no non-goal was built. Each goal is met. Each left_to_builder constraint holds, including these two: every error a person reads names the input or state at fault and what to do next, and breaking the behavior of any Must scenario in qa-scenarios.md fails an automated test. For the test bar, read the tests and judge whether each would fail on a wrong implementation, not only whether it exists. While the change is active (<CHANGE_DIR> is under goodflows/changes/ but not archive/), run goodflows change validate <CHANGE_ID> and turn each error into a row; once it is archived, skip validate.
```

Check `qa`, the QA scenarios:

```text
Drive the Must scenarios of <CHANGE_DIR>/qa-scenarios.md through this repository's verify-<app> skill at <VERIFY_SKILL>, in its run mode, and keep the .verify/<app>/<run-id>/ run folder. A scenario that fails is a critical row with the evidence path, routed code unless the scenario itself contradicts the change, which is person. Never edit qa-scenarios.md or mark a scenario not run to get a pass. Report the run folder path in your report. The tree may be uncommitted at this point, so this run predicts the verdict and is not uploaded.
```
