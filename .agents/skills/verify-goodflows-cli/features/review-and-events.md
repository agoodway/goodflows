# Review and events

When the policy flags something for a person, the change request's review packet lists each item with the answers it allows. A person (or an agent acting for one) answers items at the current head, the answers open tasks for the agent, and the agent marks them done. An agent waits for a verdict state with `cr wait`, follows project events with `events`, takes the merge slot with `cr lease`, and reports the merge with `cr merged`.

## Sub-features

- `review-packet` shows the packet in Markdown and JSON.
- `review-answer` answers an item; a stale head is refused with a clear message.
- `review-tasks` lists tasks and marks one done.
- `cr-wait` waits for a verdict state and exits `75` on timeout.
- `events-page` reads events and resumes from the integer cursor.
- `lease` acquires, heartbeats and releases the merge slot, or explains why not.
- `cr-merged` reports the merge.

## How to get to it (user POV)

- `goodflows cr review`, `goodflows cr answer <item> <approve|reject|choose|request_change> [--option o] [--note n] [--head sha]`, `goodflows cr tasks [--state s]`, `goodflows cr tasks done <id>`, `goodflows cr wait --until <states> [--timeout s]`, `goodflows events [--after c] [--wait s] [--follow]`, `goodflows cr lease acquire|heartbeat|release`, `goodflows cr merged --commit <rev>`, each with `--json`.

## Driving it with control-goodflows-cli

Preconditions: builds on [Change request lifecycle](./change-request-lifecycle.md) through **Push** and **States** (change request 1 is `open` with claims whose only fact is `proof: stated`, below the default `min_proof_level` `ran`), under the default policy.

- **Packet.** `$C cli review cr review` and `$C cli review-json cr review --json`. Exit `0`; `.packet.items` has one `unproven_claim` item for rule `proof_below_minimum` with `answers` `["approve", "request_change"]`, and the Markdown prints a `goodflows cr answer -n 1 <id> … --head <HEAD>` line per answer.
- **Answer.** Take the item's id (`jq -r '.packet.items[0].id'`) and run `$C cli answer cr answer <id> request_change --note Add-a-test --json`. Exit `0`. `$C cli answer-stale cr answer <id> approve --head 0000000000000000000000000000000000000000` exits `1` with `change request #1 moved to head <HEAD> since the answer's head; read goodflows cr review again`.
- **Tasks.** `$C cli tasks cr tasks --json` lists one task (`.tasks[0].state` `open`, `answer` `request_change`); `$C cli task-done cr tasks done <task id>` exits `0`, and `$C cli tasks-done cr tasks --state done` lists it.
- **Wait.** `$C cli wait cr wait --until mergeable --timeout 3 --interval 1`. Exit `75` with `Timed out before the verdict reached mergeable.` while it is not. `$C cli wait-now cr wait --until needs_review,blocked,pending --timeout 3` exits `0` at once.
- **Events.** `$C cli events events --limit 5 --json`; it holds five events and `next_cursor` is the integer `5`. `$C cli events-next events --after <that cursor> --limit 200 --json` returns the later events, starting at id `6`. `$C cli events-wait events --after <latest cursor> --wait 2` returns after about two seconds with no events and prints `next cursor: <same>`.
- **Lease refused.** `$C cli lease-refused cr lease acquire`. While the verdict is not `mergeable` it exits `1` with `change request #1 is not mergeable (verdict <state>); see goodflows cr verdict`, and no lease is held.
- **Lease.** Make the verdict `mergeable`: upload passing runs for both sets at HEAD (as in [Verify runs](./verify-runs.md), one run folder per set, both results `passed`), answer the packet item `approve`, and check `$C cli verdict-ready cr verdict` reads `mergeable` with `review: 1/1 items cleared`. `$C cli lease cr lease acquire --json` returns `.lease.id` with `state` `active`; `$C cli heartbeat cr lease heartbeat <id>` moves `expires_at` later; `$C cli release cr lease release <id>` prints `state: released`; releasing it again exits `1` with `lease <id> is no longer active`.
- **Merged.** On `main` in the scratch checkout, `$C git merge --no-ff -qm merge feat/verify`, `$C git push -q origin main`, then `$C cli merged cr merged -n 1 --commit HEAD`. Exit `0` printing `#1 merged`; `cr-1` read through `api` has `state` `merged` and `merge.merged_commit` equal to the merge commit (`merged_without_lease` true when no lease was held). Check out `feat/verify` again afterwards.

## Gotchas

- Answers are per head: any commit after answering makes the answers stale and the packet asks again.
- A second `cr lease acquire` by the same change request returns its own active lease. `slot_taken` needs a second change request on another branch targeting `main`.
- `--note` values above contain no spaces only to keep the commands copyable; spaces work when quoted.
- `events --follow` never exits by itself; drive it with `timeout 5 …` inside `$C cli` only when a recipe needs it, and expect exit `124`.
- A `410` from `events --after` means the cursor fell out of retention; the CLI says so and suggests starting without `--after`.
