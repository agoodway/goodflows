# Builder check prompt

Give this prompt to a fresh subagent once the change is written and validated. Replace `<CHANGE_DIR>` with the change's directory, such as `goodflows/changes/add-review-answer-reapply`, and `<CODE_PATHS>` with the files and directories the change touches, as you found them while exploring. Send nothing else from the conversation: the check works only if the subagent knows what the implementer will know.

```text
You are the engineer who will implement a change to this repository. You have its change files and the code, and you cannot talk to the people who wrote the change.

Read every file in <CHANGE_DIR>: change.md (its front matter holds the goals, the decisions and what is left to you), the spec deltas under specs/, and qa-scenarios.md. Read the durable spec of each capability the change lists, in goodflows/specs/<capability>/spec.md. Then read the code the change touches, starting with <CODE_PATHS>.

List only the places where you would have to guess before you could build this, and where a wrong guess would change what users or other systems get: a case no requirement settles, two lines that disagree, a requirement the code makes impossible or much harder than the change assumes, a failure mode the change implies but never decides. For each one, give the question, the file and line it comes from, and what goes wrong if you guess wrong. Rank them, most important first.

Read left_to_builder too, and report an entry that hides a decision users would notice, or whose constraint you could not meet or could not check. Also report a quality attribute the change implies but settles nowhere, such as a size or time limit, who may call something, what happens on a timeout or a retry, or two callers at once. Otherwise skip what left_to_builder lists, and skip anything you could reasonably decide yourself without changing what users get (names, file layout, code structure, wording of output only people read), and comments on the writing.

Do not edit any file. If nothing important is left, say "nothing important left".
```
