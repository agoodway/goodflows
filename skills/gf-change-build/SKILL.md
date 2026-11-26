---
name: gf-change-build
description: "Build an approved goodflows change (goodflows/changes/<id>/) and take its change request to a verdict, unattended: implement it, verify the code against the change in both directions, fix what verification finds, keep the spec deltas true to the code, archive the change, then push and drive the change request until it is mergeable or needs a person's review. Use for /gf-change-build, \"build this change\", \"implement <id>\", or after gf-change-write approves a change. Not for planning a change (gf-change-write), and not for merging."
---

# Build a goodflows change and ship it to a verdict

This skill takes an approved goodflows change from plan to a change request whose verdict is `mergeable`, or `needs_review` with the review packet for a person. It runs unattended: it never stops to ask a person in the middle. Everything only a person can decide goes into one final report at the end of the run.

You are the orchestrator. Every step runs in a fresh subagent from a prompt file in this skill's directory, and you keep only what the steps return. The goodflows CLI documents its own formats and commands: `goodflows agents` describes `goodflows-change/1`, `goodflows-verification/1` and the change request verdict, and every command has `--help` and `--json`.

## Rules for the whole run

- **Keep your context small.** Never read code, diffs or full reports yourself. Each subagent writes its full report to `.goodflows/build/<id>/<step>-<n>.md` (`<n>` counts that step's runs), ends that file with its status block, and returns only the status block. Hand the next subagent report paths, not their contents. Before each step, tell the user one line, such as `add-login · verify 2`.
- **Log each step.** After every step except preflight, append one line to `.goodflows/build/<id>/log.md`: `<step> | <report path> | <STATUS>`. Its last line is where a resumed run starts.
- **Hold what waits on a person in one file.** `.goodflows/build/<id>/held.md` lists what is waiting on a person, one line each: `<subject> | <kind> | <source report> | <the question> | Recommend: <option, or none>`. `subject` names the thing a person decides on, in one of these forms: the requirement name, for a requirement's behavior or text; `scenario <n>` or `scenario <n>.<step>`, for a QA scenario; `review <item id>`, for a review item; `<rule>`, or `<rule> <scenario or subject>` when the verdict reason names one, for a verdict reason; or a file path or a short noun for a deliverable outside the code, such as `release notes`. Never use a requirement name for something that is not that requirement's behavior, because sort and later rounds match on subject. `kind` is `question` (a build set-aside, a `person` row, or an unfixed row that carries a question), `not_verified` (a check that could not run), or `verdict` (a person's verdict reason or review item). Add a line only when no line has the same subject and kind. Remove a line once it is settled: a `question` once the person's answer has been applied, a `not_verified` once a later verify round runs that check without reporting the subject, and all `verdict` lines each time ship returns, replaced by the current verdict's. Close and the final report read this file, so a resumed run loses nothing.
- **Never ask in the middle.** Subagents never ask the user anything. The only question before the run is which change to build, when the user named none and more than one change qualifies.
- **The spec follows the code.** While the change is built, its deltas must describe what the code does. Who may edit which part is in "Who owns the spec" below; route every finding and question by it.
- **Never clear a verdict the wrong way.** Do not lower or edit the policy, weaken, remove or demote a scenario, mark a scenario not run, drop a feature glob, approve a scenario set, answer a review item, or merge. A person does each of those, and the final report asks them.
- **One writer per file.** Claims are written by `gs-change-request`, verify runs by the repo's `verify-<app>` skill, the required workflow run by its `run-ci-<app>` skill, and feature files by `gs-maintain-verification-skill`. No step edits a file another step owns to make an upload pass. When a writer skill is missing, that is a blocker; nobody writes its file from memory of the format.
- **Recover before you block.** A failed command is evidence: read its error, repair the cause, and run it again. After a timeout or a lost connection, check whether the operation happened before repeating it. Three attempts in a row without progress on the same failure is a blocker. Without a subagent tool, run each step's prompt yourself, one after another, and still write each report file.

## Who owns the spec

`goodflows change validate` fails any ADDED or MODIFIED requirement that no Must scenario in `qa-scenarios.md` covers, and any change to `qa-scenarios.md` needs a person to approve the intent set again on the server. So the build may change only the text of requirements the delta already has:

- The builder may change the text of an ADDED or MODIFIED requirement already in the delta where that text comes from a `by: agent` decision or a `left_to_builder` entry, or where it says nothing about a behavior the code must have, as long as no goal or anti-goal changes. A requirement whose `Refs:` cite a `by: user` decision may still be tightened where it is silent, when the edit changes nothing that decision answered. The change stays `approved`. Each such edit is recorded as a new `by: agent` decision whose `why` starts with "Found while building:", cited on the `Refs:` line of the requirement it changed. Once the change is archived, a fix edits `goodflows/specs/<capability>/spec.md` directly, which has no `Refs:` lines, and the fix report records the edit instead.
- A person decides everything else: adding a requirement, MODIFYING a durable requirement the delta does not touch yet, removing or renaming a requirement, renaming or removing a spec `#### Scenario:` that a `covers` entry names, any edit to `qa-scenarios.md`, any change to what a `by: user` decision, a goal or an anti-goal says, and, when `change.md`'s `door` field is `one_way`, any edit to a wire contract, stored data, or a public command, flag or field. The build sets that requirement aside, and the question, with the requirement text it would recommend, goes to `held.md`.
- When code and spec disagree, decide which is right first. The spec wins by default, and the code changes.

## 1. Pick the change and preflight

Use the id the user gave. Otherwise use the one change that is `approved` in `goodflows/changes/` or has reports under `.goodflows/build/<id>/`; with several, ask which, and with none, say so and stop.

Run `preflight-prompt.md`. It checks the change, the branch, the CLI and its config, finds the writer skills, saves the approved `change.md` for the bars check, and reads the step log. Keep its `CHANGE_DIR`, `BASE`, `TITLE`, `APPROVED` and `WRITERS` for the later steps; `CHANGE_DIR` changes to the archived path once close archives. A `blocked` preflight goes straight to the final report.

On a resume, continue after the step `RESUME` names. When that step is ship, or a step the verdict loop started, run ship again first, so the verdict is read fresh from the server, not from an old report.

## 2. Build

Run `build-prompt.md`. A `small` change gets one build subagent. A `project` change gets one per capability, one after another, each handed the reports of the slices before it; parallel builders would edit the same files.

Add the build's `QUESTIONS` to `held.md` as `question` lines. A slice that returns `blocked` ends the run: go to the final report, with nothing committed or pushed and the tree left as it is for a person to inspect.

## 3. Verify

Run the four checks in `verify-prompt.md` at the same time, each in its own subagent: spec to code, code to spec, decisions and bars, and the QA scenarios. They only read and drive, so they cannot conflict. Each returns finding rows. A check that returns `blocked` counts as a `not_verified` row with its reason.

## 4. Sort

Merge the rows of all four checks and drop duplicates: two rows with the same subject and the same problem are one. Check each row's route against "Who owns the spec":

- `code` and `spec` rows go to the fix step, except a row whose subject `held.md` already lists as a `question`: that waits on the person, so drop the row.
- `person` rows go to `held.md` as `question` lines, and `not_verified` rows as `not_verified` lines.
- A `not_verified` line in `held.md` whose check ran this round and did not report its subject is settled: remove it.

This is the only point where you read findings, and you read the rows, not the reports.

## 5. Fix and re-verify

When there are `code` or `spec` rows, write them to `.goodflows/build/<id>/findings-<n>.md`, each followed by the path of the verify report it came from, and run `fix-prompt.md` with that path and the row count. Add each `UNFIXED` row that carries a question to `held.md` at once. Then run `verify-prompt.md` again, scoped to the repair, and sort its rows.

Repeat until no `code` or `spec` rows remain, with two limits, counted from each time the run enters this step: a subject still in a `code` or `spec` row after three fix rounds goes to `held.md` as a `question` with the fix reports' evidence, and sort drops it from then on; after five fix rounds, every remaining row goes to `held.md` the same way. A fix that returns `blocked` ends the run: go to the final report with nothing pushed.

## 6. Close

Run `close-prompt.md`. It runs the full check gate locally, writes `verification.md` with `goodflows change verified` with every line of `held.md` under `open`, archives the change when nothing is open, and commits. `failed` returns finding rows: fix and re-verify them (step 5), then close again. `blocked` ends the run: go to the final report with nothing pushed. When it archives, set `CHANGE_DIR` to its `ARCHIVED` path.

Archiving happens here, before the first push, so the first pushed head carries the code, the verification record and the updated durable specs together. When held lines keep the change from archiving, the run still ships the unarchived change, so the verdict and the review packet exist.

## 7. Ship

Run `ship-prompt.md`. It pushes, opens the change request with the change linked when there is none, uploads the intent set if the change request has none, runs the required workflow and drives the intent set at the pushed head and uploads both, syncs features when needed, writes and uploads the claims, moves the change request to open, and waits for a verdict. It changes no code. It returns the verdict state and one row per reason. Replace the `verdict` lines in `held.md` with the person's reasons and review items of this verdict. `blocked` goes to the final report.

## 8. The verdict loop

Route each reason by who can clear it:

- **Missing input, the agent's:** `git_facts_missing`, `claims_missing`, `scenario_results_missing`, `should_not_run`, `required_workflow_missing`, `feature_index_missing`, `unmapped_area`. Run ship again with these as `VERDICT_ROWS`.
- **Failure, the agent's:** `must_failed`, `should_failed`, `required_workflow_failed`. Run `verify-prompt.md` scoped to these reasons in the ship report, then sort, fix and re-verify (steps 4 and 5), close (step 6), and ship again. When that verify returns no `code` or `spec` row for a reason, such as a scenario that fails on the server but passes locally, nothing can be fixed: add the reason to `held.md` as a `question` with the run's evidence path, and stop routing it to the agent.
- **A person's:** `scenario_set_approval`, `diff_set_weakened`, `diff_set_revision`, `one_way_door`, `undriven_feature`, `parent_not_merged`, `parent_closed`, `dependency_not_merged`, `merge_result_ungraded`, `merge_result_failed`, and every item `cr review` lists. They are the `verdict` lines in `held.md`.
- **Unknown:** a rule not listed here is newer than this skill. Read its `message`; when it names an input the ship step can supply, treat it as missing input, and otherwise it is a person's.

Handle every agent reason of one verdict in the same round, so each round costs at most one new head. The archived `verification.md` describes the code before any fix made after archiving, and `goodflows change verified` cannot run on an archived change, so the final report says so.

Stop when the verdict is `mergeable`, or when no agent reason remains, whatever the state: `needs_review`, `pending` or `blocked` with only a person's reasons. A ship round counts whether or not it pushed a new head; three ship rounds in a row that return the same agent reason stop the run with that reason as the blocker.

## 9. Final report

Write it from the status blocks and `held.md`, in this order:

1. The change request number, the verdict state, and each reason with who clears it.
2. Every line of `held.md`, most important first, each numbered and worded so "yes" accepts the recommendation when there is one: requirement or intent set edits the build needed, `by: user` conflicts, checks that could not run, unapproved scenario sets, dependencies, and the review packet's items with the answers each allows.
3. Whether the change was archived; what `verification.md` lists under `open` if not; and, when fixes landed after archiving, that its hashes predate them.
4. What the run fixed, the heads it pushed, and any blocker with its evidence.

When the person answers, apply each answer as they direct and remove its `question` line from `held.md`. An answer that changes a `by: user` decision, a requirement or `qa-scenarios.md` sends the change back to `status: proposed` until the person approves it again. Then run this skill again on the same change: it resumes from `.goodflows/build/<id>/log.md`.
