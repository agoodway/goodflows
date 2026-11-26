# Build prompt

Give this prompt to a fresh subagent for the build. Replace `<CHANGE_ID>`, `<CHANGE_DIR>` (from preflight), `<SKILL_DIR>` (this skill's directory), `<REPORT>` (`.goodflows/build/<CHANGE_ID>/build-<n>.md`), and `<SCOPE>`: for a `small` change, "the whole change"; for a `project` change, "the <capability> capability only, after the slices whose reports are <paths>".

```text
You implement the goodflows change <CHANGE_ID> on the current branch: <SCOPE>. Another agent verifies your work afterwards against the change, from the files, so build what the change says and make the change say what you built.

Read <CHANGE_DIR>: change.md (its YAML front matter holds the goals, non-goals, anti-goals, the decisions, each by: user or by: agent, the door field, and left_to_builder), the spec deltas under specs/, and qa-scenarios.md. Read the durable spec of each capability it lists, goodflows/specs/<capability>/spec.md, and the section "Who owns the spec" in <SKILL_DIR>/SKILL.md. Then read the code the change touches until you know how it works today.

The change names no tasks, files or functions; plan the work yourself. Names, file layout, the order of the work and the wording of output only people read are yours. Follow the repository's AGENTS.md files and existing patterns.

Build it:

- For each Must scenario in qa-scenarios.md, write an automated test that fails when that behavior breaks, and watch it fail before you write the code that makes it pass. This is the test bar every change carries in left_to_builder.
- Build every ADDED and MODIFIED requirement as its text says, remove every REMOVED behavior (read its old text in the durable spec first), and keep every RENAMED requirement's behavior.
- Keep within the change. When the work needs more scope than the change gives, or the only way to fit is to narrow, defer or simplify a specified behavior, do not absorb it: set that requirement aside and report the question.
- When the code must do something the spec does not say, such as a fallback, a limit, a refusal or a field, and a user or another system would notice it, update the delta as you go under "Who owns the spec": change the text of the requirement already in the delta that covers it, and record a by: agent decision whose why starts with "Found while building:", cited on its Refs: line. When no requirement in the delta covers the behavior, or the edit is otherwise a person's, set it aside and report the question with the requirement text you would recommend.
- Run the tests that cover what you changed, and fix what fails.

Do not commit, push, archive, or run goodflows cr commands. Do not edit qa-scenarios.md, add, remove or rename a requirement, or rename or remove a spec scenario.

Write to <REPORT>: what you built for each requirement with file paths, the tests you added and that you watched each fail first, every delta edit and the decision id it added, and each set-aside with the question and what you would recommend. End the file with the same block you return. Then end with exactly this block:

STATUS: done|blocked
REPORT: <REPORT>
BUILT: <requirement names built>
SPEC_EDITS: <decision ids added, or none>
QUESTIONS:
- <requirement name, or the behavior> | <REPORT> | <the question for a person> | Recommend: <option, or none>
BLOCKERS:
- <a failure that stops the rest of the build, with its evidence, or none>
```
