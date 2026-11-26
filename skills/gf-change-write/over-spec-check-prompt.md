# Over-spec check prompt

Give this prompt to a fresh subagent after the review check's fixes are applied and the change validates again, at the same time as the builder check. Replace `<CHANGE_DIR>` with the change's directory, such as `goodflows/changes/add-review-answer-reapply`. Send nothing else from the conversation.

```text
You review a change to this repository before anyone implements it. The change should hold every decision the implementer cannot make alone, and nothing else. Your job is the "nothing else": find the lines that do not earn their place.

Read every file in <CHANGE_DIR>: change.md (its front matter holds the goals, the decisions, each marked by: user or by: agent, and left_to_builder), the spec deltas under specs/, and qa-scenarios.md. Read the durable spec of each capability the change lists, in goodflows/specs/<capability>/spec.md, and the code the change touches as far as you need to judge a line.

Report two kinds of line:

1. Pins: a line that fixes something an implementer could decide without changing what users or other systems get. Examples: names of functions, types, files or flags no one asked for; the order of the work; the exact wording or layout of output that only people read and no program parses; a spec scenario that walks through a case the requirement's text already settles.
2. Restatements: a line that says again what another line in the change already says, such as a requirement that repeats a decision's reasoning, or a body paragraph that repeats the decisions.

For each one, quote the line with its file, say which kind it is, and propose a fix: move it to left_to_builder, keeping any constraint that matters (write the entry), or delete it. Do not report a line that a MODIFIED requirement keeps unchanged from the durable spec, the name of a public command, flag, field or exit code that a decision records, or a limit, target or failure behavior (a size, a time, a retry, who may call something, the bar errors must meet). Those are decisions, not pins, even when they are exact numbers. When a line pins something a by: user decision asked for in so many words, say so instead of proposing a fix.

Do not report missing decisions or ambiguities; another reviewer covers those. Do not edit any file. If every line earns its place, say "nothing to cut".
```
