# Diff-only change requests

A project that links no goodflows change grades each change request on its diff scenario set alone. The agent opens a change request with no linked change, writes the diff set to the branch's default path next to the claims file, uploads it and its verify runs, and pushes. With the policy's `scenarios.diff_only` set to `weakening`, a person is asked only when a diff version removes or demotes a Must of its baseline: the newest accepted diff version, or v1. Approving every `diff_set_weakened` item accepts that version, which becomes the new baseline.

## Sub-features

- `diff-policy` writes a policy with `require_approved_intent = true` and `diff_only = "weakening"`.
- `diff-open` opens a change request with no linked change from a claims file.
- `diff-path` prints the default diff set path, and `scenarios upload` with no file uploads it or names it when missing.
- `diff-mergeable` grades a diff-only change request `mergeable` with no review item once a counted run passes every Must.
- `diff-weakened` flags a demoted Must as a `diff_set_weakened` item, measured from the baseline, not the previous version.
- `diff-compare-baseline` makes `scenarios diff --source diff` compare against the baseline by default.
- `diff-accept` accepts the version by answering `approve`, which makes it the baseline and shows `accepted` in `scenarios list`, the verdict and the events.
- `diff-approve-refused` makes `scenarios approve` point at `cr review` on a change request with only a diff set.
- `diff-docs-push` keeps a diff set graded after a push that changes only ignored paths.

## How to get to it (user POV)

- `goodflows policy set <file>`, `goodflows cr open`, `goodflows cr path --json`, `goodflows cr scenarios upload [<file>]`, `goodflows cr scenarios list [--source diff]`, `goodflows cr scenarios diff --source diff [--from N]`, `goodflows cr runs upload <folder>`, `goodflows cr verdict`, `goodflows cr review`, `goodflows cr answer <item> approve`, `goodflows cr scenarios approve`, `goodflows cr push`, `goodflows events`, each with `--json`, from the branch's checkout.

## Driving it with control-goodflows-cli

Preconditions: the baseline. Do not drive another recipe first in the same run: this one needs change request 1 to link no change.

Let `D=$GF_CHECKOUT/.goodflows/change-requests/feat/verify` and `HEAD` be `$C git rev-parse HEAD`.

- **Policy.** `$C cli policy-get policy get > /dev/null`, then `cp $GF_RUN_DIR/cli/policy-get.stdout $GF_CHECKOUT/policy.toml` and edit it: under `[scenarios]` set `require_approved_intent = true` and add `diff_only = "weakening"`; under `[claims]` set `min_proof_level = "stated"` so the claims add no review item. Keep the default `[evidence] ignore_paths`, which holds `docs/**`. Run `$C cli diff-policy policy set policy.toml --note Diff-only --json`. Exit `0`; `.policy.version` `1`, `.policy.policy.scenarios.diff_only` `weakening`, and `.policy.active_settings` contains `scenarios.diff_only`.
- **Open.** Write `$D/cr.md` as in [Change request lifecycle](./change-request-lifecycle.md) **Write claims** (substitute `$HEAD`) and run `$C cli diff-open cr open --json`. Exit `0`; `.created` true, `.change_request.number` `1`, `.change_request.change_id` null, `.claims.wrote_cr` true, and the verdict is `pending` with only `scenario_set_missing`. Push it: `$C cli diff-push cr push --json -- -u origin HEAD`.
- **Default path.** `$C cli diff-path cr path --json`: `.scenarios_path` is `.goodflows/change-requests/feat/verify/qa-scenarios.md` and `.scenarios_exists` false. `$C cli diff-up-missing cr scenarios upload` exits `1` naming that path.
- **Diff set v1.** Write `$D/qa-scenarios.md`, with `$SHORT` the first 7 characters of `$HEAD`:

  ```markdown
  ---
  format: qa-scenarios/2
  source: diff
  branch: "feat/verify"
  base: "main"
  revision: "$SHORT"
  review: "1"
  scenarios:
    - id: 1
      name: "User logs in"
      priority: must
    - id: 2
      name: "Wrong password is refused"
      priority: must
  regressions: []
  ---

  # QA: feat/verify

  ## Scenarios

  ### 1. User logs in (Must)

  1. Open /login and sign in.
     - Expect: The dashboard shows.

  ### 2. Wrong password is refused (Must)

  1. Sign in with a wrong password.
     - Expect: "Invalid email or password".
  ```

  Run `$C cli diff-up-1 cr scenarios upload --json`. Exit `0`; `.source` `diff`, `.version` `1`, `.file` ends with `qa-scenarios.md`, and `.digest` equals `sha256sum $D/qa-scenarios.md`.
- **Run and grade v1.** Write run folder `.verify/web/diff-1` as in [Verify runs](./verify-runs.md) **Write the run folder**, but copy `$D/qa-scenarios.md` as `scenarios.md`, set `set.path` to `.goodflows/change-requests/feat/verify/qa-scenarios.md`, and pass both results with no evidence. Run `$C cli diff-run-1 cr runs upload .verify/web/diff-1 --json`, then `$C cli diff-verdict-1 cr verdict --json` and once without `--json`. The verdict is `mergeable` with no `diff_only_set`, `diff_set_weakened` or `scenario_set_approval` reason, `.verdict.scenarios.diff` has `version` `1`, `accepted` false and `baseline_version` `1`, and the text line reads `diff set v1 <digest>  passed 2, failed 0, not run 0`.
- **v2 demotes a Must.** In the file, change scenario 2 to `priority: should` and its heading to `(Should)`. Upload it as `diff-up-2` (version `2`), write and upload run folder `diff-2` for it (both passed), then `$C cli diff-verdict-2 cr verdict --json`: `needs_review` with one `diff_set_weakened` reason, `subject` `["diff", "2"]`.
- **v3 edits a Must step.** Change scenario 1's Expect line to `The dashboard shows the user's name.`, upload as `diff-up-3` (version `3`), and upload passing run folder `diff-3`. `$C cli diff-verdict-3 cr verdict` still has the `diff_set_weakened  [diff, 2]` reason (the baseline is still v1), `review: 0/1 items cleared`, and the line `diff set v3 <digest>  baseline v1  passed 2, failed 0, not run 0`.
- **Compare against the baseline.** `$C cli diff-compare cr scenarios diff --source diff` prints `version 3 against 1 (the baseline)`, a `! 2  demoted  must -> should  (weakening)` line and a `~ 1  changed` line. `$C cli diff-compare-prev cr scenarios diff --source diff --from 2` prints `version 3 against 2` with only the `~ 1` line: the previous version alone hides the weakening.
- **Refused approve.** `$C cli diff-approve cr scenarios approve` exits `1` with `change request #1 has only a diff set; a diff set is accepted by approving its diff_set_weakened items: goodflows cr review`.
- **Review packet.** `$C cli diff-review cr review` and `$C cli diff-review-json cr review --json`. `.packet.items` has one item of kind `diff_set_weakened` with `answers` `["approve", "reject"]` and `data` with `id` `2`, `change` `demoted`, `from_priority` `must`, `to_priority` `should`, `version` `3`, `baseline.version` `1` and `edited_musts` `["1"]`. The Markdown prints the `goodflows cr answer -n 1 <id> approve --head <HEAD>` command.
- **Accept.** `$C cli diff-answer cr answer <id> approve --json`. Exit `0`. `$C cli diff-verdict-accepted cr verdict --json` is `mergeable` with no reasons, `.verdict.scenarios.diff` `accepted` true and `baseline_version` `3`; the text line reads `diff set v3 <digest>  accepted  passed 2, …`. `$C cli diff-list cr scenarios list --source diff` lists `v3 … current, accepted, baseline`, `v2 …` and `v1 …` with no marks. `$C cli diff-events events --limit 200 --json` has a `scenario_set_approved` event with `data.source` `diff` and the v3 digest.
- **Docs-only push.** `mkdir -p docs && echo usage > docs/usage.md`, `$C git add docs` and `$C git commit -qm docs`, update `cr.md`'s `head:` to the new HEAD, then `$C cli diff-claims cr claims --json` and `$C cli diff-push-docs cr push --json`. Leave the diff set's `revision` as it is. `$C cli diff-verdict-docs cr verdict --json` is `mergeable` with no `diff_set_revision` reason and `.verdict.scenarios.diff.graded` true, and `counted_runs` still holds run `diff-3`'s id.

## Gotchas

- `scenarios.diff_only` applies only while `require_approved_intent` is true. With it false no set is required and nothing in this recipe asks for a person.
- Under the default `diff_only = "needs_review"` every head gets a `diff_only_set` item instead; that is the server's default, not a CLI failure.
- A change request linked to a goodflows change never gets `diff_set_weakened`: it waits on its intent set. Open this recipe's change request without `--change`.
- The diff set's `revision` is in its bytes, so every rewrite is a new version and a new digest. A run counts only for the digest in its `scenarios.md`: upload a run folder for each version.
- A `diff_set_weakened` answer is tied to the diff digest that was current when it was given. Uploading another version supersedes it, even at the same head.
- `cr answer` sends only the head, so an answer given after another diff version was uploaded at the same head applies to that version. Read `cr review` again after any upload before answering.
