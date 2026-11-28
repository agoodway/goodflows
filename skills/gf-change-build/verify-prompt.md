# Verify prompt

Run the checks below at the same time, each in its own fresh subagent with the shared text plus that check's text. Run the `review` check only when preflight's `REVIEW_SKILL` is a path; otherwise run the other four. Replace `<CHANGE_ID>`, `<CHANGE_DIR>` (from preflight, or close's `ARCHIVED` path once archived), `<SKILL_DIR>` (this skill's directory), `<BASE>` (from preflight), `<HELD>` (`.goodflows/build/<CHANGE_ID>/held.md`), `<APPROVED>` (the `APPROVED` path from preflight), `<VERIFY_SKILL>` (the verify skill path from preflight, used by the `qa` check), `<REVIEW_SKILL>` (the `REVIEW_SKILL` path from preflight, used by the `review` check), `<REPORT>` (`.goodflows/build/<CHANGE_ID>/verify-<check>-<n>.md`), and `<SCOPE>`: "the whole branch" for the first round; after a fix, "the repair in <fix report path>, against the finding rows in <findings path>"; after a verdict, "the verdict reasons in <ship report path>".

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
Check the change's decisions and bars. Every by: user decision is followed as written, and none was edited by the build (diff <APPROVED>, the change.md that was approved, against <CHANGE_DIR>/change.md; when <APPROVED> is missing, report this part as a not_verified row). No anti-goal is violated and no non-goal was built. Each goal is met. Each left_to_builder constraint holds, including these two: every error a person reads names the input or state at fault and what to do next, and breaking the behavior of any Must scenario in qa-scenarios.md fails an automated test. For the test bar, read the tests and judge whether each would fail on a wrong implementation, not only whether it exists. A test's assertion must cover the same class of inputs the requirement names: a test that checks a narrower class, such as ASCII control bytes (is_ascii_control) where the requirement says control characters, which also include C1 (U+0080 to U+009F), misses a wrong implementation and is a warning routed code, even when a manual probe found the code correct. While the change is active (<CHANGE_DIR> is under goodflows/changes/ but not archive/), run goodflows change validate <CHANGE_ID> and turn each error into a row; once it is archived, skip validate.
```

Check `qa`, the QA scenarios:

```text
Drive the Must scenarios of <CHANGE_DIR>/qa-scenarios.md through this repository's verify-<app> skill at <VERIFY_SKILL>, in its run mode, and keep the .verify/<app>/<run-id>/ run folder. A scenario that fails is a critical row with the evidence path, routed code unless the scenario itself contradicts the change, which is person. Never edit qa-scenarios.md or mark a scenario not run to get a pass. Every evidence file the run lists must be non-empty: an empty capture, such as a server log no request reached, is not evidence, so cite a file that shows the result and say in the reason that the other was empty. Then run goodflows cr validate <run folder>; a refusal is a not_verified row naming the verify skill and the error, because ship would be refused the same way. Report the run folder path in your report. The tree may be uncommitted at this point, so this run predicts the verdict and is not uploaded.
```

Check `review`, production risk:

```text
Review the branch for what will break in production, with the gs-review skill at <REVIEW_SKILL>, given <BASE> as its base ref so it covers committed and uncommitted work. Follow it through its report, but write the report to <REPORT> instead of printing it, and keep its own read-only rule: apply no fix. In a round whose scope is a repair or verdict reasons, still review against <BASE>, and turn into rows only the findings in code the repair changed or the reasons name, plus earlier review rows in the findings file that still hold. Turn each finding that survived its verification into one row: a confirmed Blocker is critical, a confirmed Should fix is a warning, and a plausible finding is one level lower. subject is the requirement whose behavior the finding is about, else the file path. route is code when the fix keeps every requirement's text true; when the fix needs a requirement's text to change, route it by "Who owns the spec". Each test in its Tests to remove list is one suggestion row routed person, with subject test <file>::<test name>, gs-review's reason as what is wrong, and "delete it" as the fix: deleting a test can break the bars check's test bar, so a person decides. Its Comments to remove list is not rows; leave it in the report only. When gs-review's Merge Danger names a one-way door and change.md's door field is not one_way, that is a person row with subject door. A failed or unavailable second-opinion provider is not a failure: gs-review falls back on its own, and you report the provider it used.
```
