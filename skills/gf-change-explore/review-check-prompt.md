# Review check prompt

Give this prompt to a fresh subagent once the change is written and validated, before the builder and over-spec checks. Replace `<CHANGE_DIR>` with the change's directory, such as `goodflows/changes/add-review-answer-reapply`. Send nothing else from the conversation.

```text
You review a change to this repository for mistakes before anyone implements it. The change holds every decision the implementer cannot make alone, and nothing else: it names no functions, files or tasks, and pins exact output text only when a program parses it.

Read every file in <CHANGE_DIR>: change.md (its YAML front matter holds the goals, the decisions, each marked by: user or by: agent, and left_to_builder), the spec deltas under specs/, and qa-scenarios.md. Read the durable spec of each capability the change lists, in goodflows/specs/<capability>/spec.md. List the other changes in flight, goodflows/changes/*/ without its archive/ directory, and read the ones that touch the same capabilities. Then explore the code the change touches.

Check two things.

1. Structure and consistency. Each requirement uses SHALL, sits under the right ADDED, MODIFIED, REMOVED or RENAMED heading, and has a Refs: line whose ids exist. A MODIFIED, REMOVED or RENAMED requirement uses the exact name it has in the durable spec, or in the delta of a change listed in depends_on. Every covers entry in qa-scenarios.md names a requirement exactly as the delta writes it, and every requirement has a must scenario covering it. The decisions, requirements and scenarios do not contradict each other, the durable specs, or another change in flight.
2. The code. Every claim the change makes about how things work today is true. Nothing it asks for already exists. Nothing it needs is missing from the code or from another change it does not list in depends_on. An error case, an edge case, or a compatibility break with released versions that the change implies is decided somewhere.

Do not propose tasks, file lists, function names, or exact wording of output that only people read; the implementer decides those.

Return one row per finding:

severity | file:line | finding | suggested_fix | has_question: true/false | recommended: <option or n/a>

severity is critical (an implementer would build the wrong thing, or the change cannot be validated or archived), warning (should be fixed before implementing), or suggestion.

Set has_question: false when the fix has one verifiable answer (a wrong requirement name, a stale claim about the code, a broken Refs id, a missing covers entry) and give the exact edit in suggested_fix. Set has_question: true when the fix needs a decision about what users or other systems get, or when several fixes are valid; then recommended is the one option you would pick, in enough detail to apply. When unsure, set has_question: true. Before claiming that something does or does not exist in the code, search for it.

Do not edit any file. If you find nothing, say "no findings".
```
