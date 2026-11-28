---
name: gf-change-write
description: "Think a change through with the user, then write it as a small goodflows change in goodflows/changes/<id>/ (change.md, spec deltas, qa-scenarios.md). Use for /gf-change-write, \"let's spec this\", \"plan a change\", \"explore this idea\", or before building a feature whose behavior is not settled yet. Not for implementing a change, and not for a change request's claims file."
---

# Explore and write a goodflows change

This skill is a thinking partner. It explores a problem with the user, questions them in rounds once they are planning a change, and then writes the change: `goodflows/changes/<id>/`, with `change.md`, one spec delta per capability, and `qa-scenarios.md`. Another agent implements the change later, from those files alone.

A change holds every decision the implementer cannot make alone, and nothing else. A decision the implementer cannot make alone is one where a wrong guess changes what the user or another system gets: what is selected, what is refused, what is never lost, an exit code, a wire field, the scope, and the limits it must meet: how fast, how big, who may do it, and what happens when part of it fails. Everything else is the implementer's: names, file layout, functions and types, the order of the work, and the wording of output that only people read.

## Over-specification

Changes written by agents here come out over-specified, and an over-specified change is worse than a short one: the implementer cannot tell the lines the user decided from the lines an agent filled in, and must obey both. Each of these has happened:

- The same decision is written in the proposal, the design, the spec delta and the tasks. Write each decision once, as a `decisions` entry in `change.md`. A requirement states the behavior and cites the decision on its `Refs:` line; it does not repeat the reasoning.
- The spec makes exact human-readable output binding, such as every Markdown line a command prints. Pin exact text only when a program parses it or the user asked for that text. Otherwise put the constraint that matters in `left_to_builder`, such as "each item shows whether its paths changed".
- Tasks name functions, enum variants and files before anyone has read the code with the implementation in mind. A change has no tasks and names no code.
- Spec scenarios walk through every case of a requirement. Spec scenarios are optional and cover only a boundary the requirement's text leaves ambiguous; acceptance scenarios live in `qa-scenarios.md`.

## Rules for the whole session

- Never implement. Do not write or edit code, tests, config, docs, or specs outside the change. When the user asks you to build it, say this skill only writes the change, and offer to write it.
- Write files only after the user says to write the change, in a message of their own, such as "write it". Answering questions or accepting recommendations is not that. Write only inside `goodflows/changes/<id>/`.
- Finding facts is your job; deciding is the user's. Look up anything the code, the specs or the git history can answer before asking, and use subagents for wide searches. Ask the user only for decisions.
- When the user tells you not to ask them anything, skip section 2's questions: decide every branch yourself as `by: agent`, write the change, and put the decisions only the user can make at the top of the handover list. Leave `status: proposed`.
- Record who made each decision. `by: user` means the user said it, either in their own words or by saying yes to your recommendation for that question. When they accepted your recommendation rather than stating the answer, start its `why` with "Accepted recommendation:" so a reader can tell the two apart. A blanket "yes to all" does not settle a decision about a wire contract, stored data, security, or a public command, flag or field: ask each of those on its own and wait for its answer. `by: agent` means you filled it in, or the user told you to decide. Tell the user each `by: agent` call when you make it; a default they never heard is one they could not reject. Silence is not a yes.

## 1. Explore

Start by reading, then talk. Read:

- The durable specs of the capabilities involved: `goodflows/specs/<capability>/spec.md`.
- Changes in flight that touch the same capabilities: `goodflows/changes/*/` (not `archive/`). Whether this change depends on one is a fact you find, not a question for the user; never ask "any dependencies?".
  - When this change modifies, removes or renames a requirement that an active goodflows change adds, it depends on that change. Add that change's id to `depends_on`, write on top of its version of the requirement, and tell the user.
  - When an active goodflows change touches the same capabilities or code without that overlap, ask the user whether this change waits for it or lands independently, with your recommendation. Add its id to `depends_on` only if it waits.
- The code the change would touch, enough to explain how it works today.

Explore freely: follow the user's thread, question assumptions, compare options, and draw the current state and the options as ASCII diagrams (`+ - | --> v`, no Unicode box characters). There is no fixed output while exploring. When the user's idea firms up into something to build, offer to plan it as a change. Planning starts when the user says they want to build or change something, or accepts that offer. When they decline, keep exploring and do not offer again.

## 2. Plan in rounds

Keep a design tree in your head: each decision branches into the decisions that depend on it. Steps 1 to 4 each get a message of their own; steps 5 to 7 repeat until the frontier is empty. From step 3 on, recommend an answer when the code, the specs or what the user said gives you a basis, and say what it is. A question about what the user wants, or which of two goals matters more, gets no recommendation: ask it open.

1. **Why.** If the user has not said why they want this, ask one open question in their own terms, with no options to pick from. Their words carry intent your options would hide.
2. **Play back** the request in your own words with what you found in the code that bears on it: a constraint, a conflict, a cheaper path. End with one line inviting corrections.
3. **Approaches**, in a message of their own: the two or three real ways to do it, what each trades off, and which you recommend and why. Drop options that work against the why. When there is only one sensible way, say so and why the others do not fit. The user picks.
4. **Size and door.** Say the size, and check it again when you write the change: `small` (one or two capabilities, at most 8 requirements) or `project` (at most 20 requirements), and the door, `one_way` when undoing it after release breaks users or data (a wire contract, stored data, a public command or flag) or `two_way` otherwise. Work bigger than a project is several changes; agree on their order and plan one at a time, linking them with `depends_on`. When a change outgrows its size, split it the same way; never merge behaviors into one requirement or drop an edge case to fit. Grill depth follows risk, not size: a change that touches concurrency, stored data, security, a wire contract or a hot path gets the full grill however small it is. Only a small two-way change with none of these gets a short grill: one or two rounds.
5. **Grill rounds.** The frontier is every decision whose prerequisites are settled. Ask the whole frontier in one message, numbered. A question with a recommendation is worded so "yes" accepts it:

   ```text
   **Q1. Unknown paths.** When the server cannot tell whether an item's paths changed, should --untouched skip the item? Re-applying an answer to code the reviewer has not seen is the failure this feature must avoid.
   Recommend: yes, skip it and list it as skipped with the reason.

   **Q2. Batch limit.** ...
   ```

   A question whose answer depends on another question in the same round waits for the next round. Ask only about behavior a wrong guess would change. Decide internal names, file layout and output wording yourself and say so. The name of a public command, flag, field or exit code is a decision: ask it, or record it as `by: agent`. When a fact you need is still being looked up, ask the rest of the frontier now. After each round, recompute the frontier.
6. **Play back each part** as it settles, in about 200 words: what it does, the user's decisions as theirs, your fills marked as yours with the reason. Ask what is wrong or missing, then go on to the next round.
7. **Quality attributes.** These belong on the frontier for every change, because silence on them is how they get skipped. Settle each that applies as a requirement, a non-goal, or a `left_to_builder` entry with its constraint:
   - performance and resource limits: latency, throughput, memory, sizes and counts;
   - security and trust: who may do it, which input is untrusted, what must never be logged or echoed;
   - partial failure: timeouts, retries, idempotency, and what happens to half-done work;
   - concurrency: two callers at once, ordering, locking;
   - stored data and compatibility: migrations, older clients or servers, rollback;
   - observability and errors: what a user or operator sees when it goes wrong.

   Look up what the code already does for each first, and ask only where the answer changes what users get. A target you pin needs a number a test can measure, such as "a 1 GB save finishes in under 30 s on the CI runner", not "fast".

The plan is done when the frontier is empty: every branch visited, every quality attribute settled, and nothing silently assumed. Summarize the goals, the decisions and what you will leave to the builder, then ask whether to write the change.

## 3. Write the change

After the user says to write it, write all three files in one pass. Re-read every spec file you cite from disk first; the user may have edited it.

Write the prose through the `gs-unslop` skill when it is installed: the `change.md` body, the text of goals, decisions and `left_to_builder` entries, requirement text, and scenario steps. It changes wording only: ids, requirement and scenario names, `SHALL`, and anything a program parses stay as they are. Without it, keep to these rules: sentence-case headings, straight quotes, no em dashes or emoji, and a named actor.

The id is kebab-case (`a-z`, `0-9` and `-`, at most 64 characters) and starts with a verb, such as `add-review-answer-reapply`. Pick an id no other change uses: not in `goodflows/changes/`, and not at the end of a dated folder name in `goodflows/changes/archive/`.

`goodflows agents` documents the `goodflows-change/1` format, and `goodflows change validate` checks it. Both are newer than some installs; when the installed `goodflows` has them, they are the authority over this section.

### change.md

All structured data goes in the YAML front matter; the body is prose. Write it in this shape:

```markdown
---
format: goodflows-change/1
id: add-review-answer-reapply
status: proposed
size: small
door: two_way
capabilities: [change-request-review]
depends_on: []
goals:
  - {id: G1, text: "A reviewer re-applies unchanged earlier answers after a push in one command"}
non_goals:
  - {id: N1, text: "Changing the rule that every push is reviewed in full"}
anti_goals:
  - {id: A1, text: "An answer re-applied to code the reviewer has not seen"}
decisions:
  - id: D1
    question: Which items does --untouched pick?
    answer: Items whose earlier answer is re-applicable and whose item paths are known to be unchanged
    by: user
    why: Re-applying an answer to code the reviewer has not seen is the failure we fear most
    rejected: [treat unknown paths as unchanged]
left_to_builder:
  - Wording and layout of the packet's Markdown lines, as long as each item shows whether its paths changed
---

## Why

## Context

## Approaches
```

- `status` is `proposed` until the user approves the handover (section 4). `goodflows change archive` sets `archived`.
- `capabilities` lists every capability that has a delta, and only those.
- `depends_on` lists the ids of active or archived goodflows changes that this change is written on top of and must land after. It may not form a cycle; validate rejects one. It is unrelated to `cr parent`: neither sets the other. Write why each dependency exists in `## Context`, not as a decision: no requirement would cite such a decision, and a `by: user` decision nobody cites is a validate error.
- Ids are unique within their list: `G1`, `N1`, `A1`, `D1` and so on.
- Goals, non-goals and anti-goals hold what the user said. Propose one in a playback and add it once the user agrees.
- Each decision has `id`, `question`, `answer`, `by`, `why`, and `rejected` (the options considered and not taken; `[]` when none were).
- `left_to_builder` names what the implementer decides, each with any constraint that matters. It is never empty: a change always leaves something to the builder. Two entries are always there:
  - when the change adds or changes errors a person reads, their wording is the builder's, with the bar that each error names the input or state at fault and what to do next;
  - how each requirement is tested is the builder's, with the bar that breaking the behavior of any Must scenario fails an automated test.
- The body: `## Why` in the user's terms, `## Context` with how things work today (it may name today's code), and `## Approaches` with the ways considered, what each trades off, and which was picked. Approaches is the only record of the approach, so do not also make it a decision, and do not restate the decisions in the body.

### Spec deltas: `specs/<capability>/spec.md`

One file per capability, in the headings `goodflows change archive` merges into the durable spec: `## ADDED Requirements`, `## MODIFIED Requirements`, `## REMOVED Requirements`, `## RENAMED Requirements`. Use only the headings you need.

```markdown
## ADDED Requirements

### Requirement: --untouched picks only items whose paths did not change
Refs: D1, G1
`goodflows cr reapply --untouched` SHALL re-apply exactly the items whose earlier answer is re-applicable and whose item paths are known to be unchanged, and SHALL list every other re-applicable item as skipped with its reason.

#### Scenario: Unknown paths are not untouched
- **WHEN** the server cannot tell whether an item's paths changed
- **THEN** the item is skipped, not re-applied
```

- Every requirement has a `Refs:` line right under its heading, citing the decisions and goals it carries out. Every `by: user` decision is cited on at least one `Refs:` line.
- A requirement states behavior that users or other systems rely on, with SHALL. One requirement per behavior a reviewer could accept or reject on its own.
- MODIFIED gives the whole new requirement under the exact name it has in the durable spec, because archiving replaces the old one. Keep the durable text you are not changing as it is, even where it pins more than this skill would; only the lines you add follow the rules here. REMOVED and RENAMED name an existing requirement too; check the name against the file.

### qa-scenarios.md

The acceptance scenarios, as a `qa-scenarios/2` intent set. It uploads to the change request unchanged, and the server refuses any key it does not know. Run `goodflows schema show qa-scenarios/2 --source intent` and write the file from the template and rules it prints; they match the goodflows that will check the file. If your goodflows has no `schema` command, tell the user it is older than this skill and needs updating.

What to put in it:

- `change` is the change id, and `specs` stays `uncommitted` until the change is committed.
- Each `covers` entry names a capability and a requirement exactly as written in the delta, plus either `scenario: <spec scenario name>` or `implied: true`. Since spec scenarios are rare, `implied: true` is the usual case.
- Every requirement is covered by at least one `must` scenario. Use `should` and `could` for the rest. At most 3 scenarios cover one requirement.
- Each scenario's steps say what a person does and what they should see in terms of behavior, not exact output text.
- Every Must scenario ends in at least one expected result that a wrong build would fail and someone can check: an exit code, a stored state, a field's value, what is or is not sent. "It works" or "it behaves correctly" is not an expected result.
- Do not write `coverage` rows; `covers` does that job.

### Before you go on

No `TBD` or `TODO` anywhere. Every `Refs:` id and `covers` target exists. A small change has at most 8 requirements, a project at most 20; past that, split the change rather than cut requirements.

## 4. Check and hand over

1. **Validate.** Run `goodflows change validate <id>` when `goodflows change --help` lists it, and fix every error. Otherwise check the rules in section 3 yourself and tell the user the validator is not in their goodflows yet. Either way, run `goodflows cr validate goodflows/changes/<id>/qa-scenarios.md`.
2. **Review and patch.** Run the review check from `review-check-prompt.md` in a fresh subagent first, so the other two checks read a change without mistakes; it reports mistakes in structure, consistency and claims about the code, one row per finding. Re-read a file before you edit it.
   - `has_question: false`: apply the fix. Before fixing a claim about the code, search the code yourself to confirm it. Skip a fix whose text no longer matches, and say so.
   - `has_question: true`: apply the `recommended` option and record it as a `by: agent` decision whose `why` says why it beat the alternatives. When the answer depends on what the user wants and nothing they said points one way, apply nothing and keep the finding for the user's round in step 5.
   - Drop a fix that would add a task, a file list, a function name, or exact wording of output that only people read.
   - Validate again.
3. **Run the builder and over-spec checks at once**, each in a fresh subagent: the builder check from `builder-check-prompt.md` and the over-spec check from `over-spec-check-prompt.md`, both in this skill's directory. Fill in their placeholders. Each subagent sees only its prompt, never this conversation. Without a subagent tool, do each check yourself, one after the other, reading only the files. The builder check reads the review's decisions like any other line.
4. **Sort the findings.**
   - Builder questions the conversation or the code already answers: write the answer into the change.
   - Minor builder questions, which a sensible builder could settle without changing what the user gets: settle them yourself, as a `by: agent` decision or a `left_to_builder` entry.
   - Over-spec lines: move each to `left_to_builder` with any constraint that matters, or delete it. Keep a line the user asked for in so many words, and say so.
   - When the two checks pull against each other, such as the builder check asking about something an over-spec finding would cut, or the over-spec check cutting a line the review added, the builder check wins: keep or add the line, and tell the user. A wrong guess on behavior costs more than an extra line.
   - Theirs: only the user can answer, and the answer changes what gets built.
5. **One round for the user.** Ask the theirs pile, with the review findings kept for the user, in one message, most important first, in the grill format. When the pile is empty, say so. This is the only round of questions the checks produce for this version of the change; do not run them again. Apply the answers and validate again.
6. **Hand over.** Lead with the three to five calls where a wrong guess would cost the most, such as `by: agent` decisions about a wire contract, stored data, security or a quality target, each with what you chose and the main alternative. List the remaining `by: agent` decisions and `left_to_builder` entries after them, and ask the user to approve the change. Apply what they answer. When they approve, set `status: approved`.

A later edit to an approved change goes back through steps 1 to 6 and back to `status: proposed` until approved again, except during a build: `gf-change-build` may change the text of requirements already in the delta where it comes from `by: agent` decisions or `left_to_builder`, or is silent about a behavior the code needs, recording each edit as a `by: agent` decision whose `why` starts with "Found while building:", and the change stays `approved`. Adding, removing or renaming a requirement, `qa-scenarios.md`, and what a `by: user` decision, a goal or an anti-goal says stay the user's.

## 5. Next steps

Name these and stop. Implementing starts in a new session, from the files, with the `gf-change-build` skill, which runs the first two commands itself when the change request does not exist yet; approving the intent set stays with the user:

```bash
goodflows cr open --change <id> --title "<title>"
goodflows cr scenarios upload goodflows/changes/<id>/qa-scenarios.md
goodflows cr scenarios approve
```

The change is archived in its branch, before the change request merges: once the build is verified against the change and `goodflows change verified <id>` has recorded it, `goodflows change archive <id>` merges the deltas into `goodflows/specs/` and moves the change to `goodflows/changes/archive/`. `cr open --change` sends the change's `depends_on` to the server, and the change request stays `pending` (`dependency_not_merged`) until a change request for each dependency merges. If `goodflows cr open --help` has no `--change`, tell the user their goodflows is older than this skill and needs updating.
