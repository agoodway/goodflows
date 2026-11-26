# Change request lifecycle

An agent opens a change request for its branch, before code or from a `change-claims/2` claims file, keeps the server's Git facts and claims in step with HEAD as it commits and pushes, moves the change request between draft, open and closed, and reads its verdict.

## Sub-features

- `cr-path` prints the default claims file for the branch.
- `cr-open-before-code` creates a draft linked to a goodflows change without a claims file.
- `cr-open-claims` reuses the branch's draft, reports Git facts, writes the number into `cr` and uploads the claims (the `gf-change-request --approved` create path).
- `cr-claims` reports facts and uploads claims for a new commit (the update path).
- `cr-push` runs `git push` and reports facts for the pushed head.
- `cr-states` moves the change request through ready, draft, close and reopen.
- `cr-status-verdict` shows the record, its history and the verdict.

## How to get to it (user POV)

- `goodflows cr path`, `goodflows cr open [--claims <file>] [--change <id>] [--title <t>]`, `goodflows cr claims [<file>]`, `goodflows cr push [-- <git push args>]`, `goodflows cr ready|draft|close|reopen`, `goodflows cr status`, `goodflows cr verdict`, each with `--json`, from inside the branch's checkout.

## Driving it with control-goodflows-cli

Preconditions: the baseline.

- **Path.** Run `$C cli path cr path --json`. Exit `0`; `.path` is `.goodflows/change-requests/feat/verify/cr.md` and `.exists` is false.
- **Open before code.** Run `$C cli open-before-code cr open --change add-login --title "Add login" --json`. Exit `0`; `.created` is true, `.change_request.number` is `1`, `.state` `draft`, `.change_id` `add-login`, and `.claims.status` `missing`. Second view: `$C api cr-1 GET /api/v1/projects/{project}/change-requests/1` returns `head` null and verdict `pending` with a `git_facts_missing` reason.
- **Write claims.** Write `$GF_CHECKOUT/.goodflows/change-requests/feat/verify/cr.md`:

  ```markdown
  ---
  format: change-claims/2
  cr: null
  head: $HEAD
  door: two_way
  door_reason: "Adding a page is easy to revert"
  reach: "The login page only"
  safe_because:
    - {fact: "Login renders", location: "lib/login.ex:1", proof: stated}
  left_open: []
  questions: []
  decisions: []
  ---

  # Add login

  ## What changed

  A login page.
  ```

- **Open from claims.** Run `$C cli open-claims cr open --json`. Exit `0`; `.created` is false and the number is `1` (the draft was reused); `.git_facts.status` `reported` with `.git_facts.head` equal to `$HEAD`; `.claims.wrote_cr` true; `.verdict.state` and `.change_request.verdict.state` are both `needs_review` (the `proof: stated` fact is below the default `min_proof_level` `ran`). The file's line now reads `cr: 1` (`grep '^cr:'`). Second views: `$C api facts GET /api/v1/projects/{project}/change-requests/1/git-facts/$HEAD` has `totals.files` `1` and one commit whose `sha` is `$HEAD`; `$C api claims GET /api/v1/projects/{project}/change-requests/1/claims/$HEAD` has `document` equal to the file byte for byte (`jq -j .data.document | cmp - <file>`).
- **Text output.** Run `$C cli open-claims-text cr open` once more. It prints `Using change request #1 of this branch`, `Git facts: unchanged …`, `Claims: unchanged …` and `Verdict: needs_review` with a `proof_below_minimum` reason.
- **Update after a commit.** Commit a change (`echo two >> lib/login.ex`, `$C git commit -qam more`), set the file's `head:` to the new HEAD (`sed -i "s/^head: .*/head: $($C git rev-parse HEAD)/" <file>`), and run `$C cli claims cr claims --json`. Before updating `head:`, `$C cli claims-stale cr claims` exits `1` saying the file describes the old head. Exit `0`; `.git_facts.status` `reported` for the new head and `.claims.status` `uploaded`. `cr-1` read again has `head` equal to the new HEAD and `description` equal to the file.
- **Push.** Run `$C cli push cr push --json -- -u origin HEAD`. Exit `0`; stdout is one JSON line; stderr holds git's `[new branch]` line. `$C git ls-remote origin feat/verify` prints the new HEAD.
- **States.** Run `$C cli ready cr ready`, `$C cli draft cr draft`, `$C cli close cr close`, `$C cli reopen cr reopen`, then `$C cli ready-again cr ready`. Each exits `0` and its `#1 <state>` line names `open`, `draft`, `closed`, `draft`, `open`. `$C cli ready-twice cr ready` exits `0` and records no new history entry. `cr reopen` without `-n` finds the branch's one closed change request.
- **Status and verdict.** Run `$C cli status cr status` and `$C cli verdict cr verdict`. The status lists five history lines, `draft -> open`, `open -> draft`, `draft -> closed`, `closed -> draft`, `draft -> open`, with real states (not `-`). The verdict line names the head and `policy v0`.

## Gotchas

- Claims describe one head: `cr claims` refuses a file whose `head` is not HEAD before sending anything. Update `head:` after every commit.
- The server stores one set of claims per head. A second, different claims file for the same head is a `409`; commit first.
- `cr open` fetches `origin <target>` before computing facts. In the scratch checkout `origin` is the bare repo, so this works offline; pass `--no-fetch` only when a recipe says so.
- `cr open` with `--change add-export` on this branch fails, because the draft links `add-login` and no route changes the link. That is expected, not a failure of this feature.
