# Scenario sets

An agent uploads the `qa-scenarios/2` intent set for a change request before code, a person approves or rejects it, the agent uploads a new version after an edit and compares versions by scenario ID, and later uploads a diff set for the branch. The CLI never reformats a set: the server stores the bytes it was given, and the set digest and per-scenario section digests are computed from them.

## Sub-features

- `scenarios-upload-intent` uploads an intent set exactly as written.
- `scenarios-approve` approves the current intent version by digest.
- `scenarios-reject` rejects a version by file, with a note.
- `scenarios-diff` compares two intent versions by scenario ID.
- `scenarios-upload-diff` uploads a diff set whose `revision` names HEAD, and refuses a `+dirty` one before sending.

## How to get to it (user POV)

- `goodflows cr scenarios upload <file>`, `goodflows cr scenarios approve [<file>|--digest <sha256>]`, `goodflows cr scenarios reject [<file>|--digest <sha256>] --note <why>`, `goodflows cr scenarios diff [--source intent|diff] [--from N] [--to N]`, each with `--json` and `-n <number>`, from the branch's checkout.

## Driving it with control-goodflows-cli

Preconditions: builds on [Change request lifecycle](./change-request-lifecycle.md) up to **Open before code** (change request 1 is a draft linked to `add-login`). The rest of that recipe may have run too.

- **Write the intent set.** Write `$GF_CHECKOUT/goodflows/changes/add-login/qa-scenarios.md`:

  ```markdown
  ---
  format: qa-scenarios/2
  source: intent
  change: "add-login"
  specs: "uncommitted"
  review: "1"
  scenarios:
    - id: 1
      name: "User logs in"
      priority: must
      covers:
        - {capability: "login", requirement: "Sign in", implied: true}
    - id: 2
      name: "Wrong password is refused"
      priority: should
      covers:
        - {capability: "login", requirement: "Sign in", implied: true}
  ---

  # QA: add-login

  ## Scenarios

  ### 1. User logs in (Must)

  Why: Nobody can use the app without it.

  1. Open /login and sign in.
     - Expect: The dashboard shows.

  ### 2. Wrong password is refused (Should)

  Why: A guessed password must not get in.

  1. Sign in with a wrong password.
     - Expect: "Invalid email or password".

  ## Regression checks

  - None.
  ```

- **Upload.** Run `$C cli scen-up cr scenarios upload goodflows/changes/add-login/qa-scenarios.md --json`. Exit `0`; `.source` `intent`, `.version` `1`, `.duplicate` false, and `.digest` equal to `sha256sum` of the file. Second view: `$C api sets GET /api/v1/projects/{project}/change-requests/1/scenario-sets` lists intent version 1 with that digest, and `$C api set-1 GET /api/v1/projects/{project}/change-requests/1/scenario-sets/intent/versions/1` has `content` equal to the file byte for byte and two `sections`, ids `1` and `2`. Check a section digest independently: the SHA-256 of the file's bytes from `### 1.` up to (not including) `### 2.`.
- **Upload the same bytes again.** `$C cli scen-up-again cr scenarios upload goodflows/changes/add-login/qa-scenarios.md --json` exits `0`, still reports version `1`, and `.duplicate` is true.
- **Approve.** `$C cli scen-approve cr scenarios approve --json`. Exit `0`; the decision is `approve` for the version-1 digest. `$C api cr-1 GET /api/v1/projects/{project}/change-requests/1` shows `verdict.scenarios.intent.approved` true.
- **Edit and compare.** Change scenario 2's priority to `must` in both the front matter and the heading, upload it as `scen-up-v2` (version `2`), then run `$C cli scen-diff cr scenarios diff` and `$C cli scen-diff-json cr scenarios diff --json`. The text reads `version 2 against 1` and `~ 2  changed  should -> must`; scenario `1` is not listed. The verdict now shows intent v2 `not approved`.
- **Reject.** `$C cli scen-reject cr scenarios reject goodflows/changes/add-login/qa-scenarios.md --note Two-musts-is-too-strict`. Exit `0`; the decision is `reject` with that note. Then approve it with `$C cli scen-approve-v2 cr scenarios approve goodflows/changes/add-login/qa-scenarios.md` so later recipes see an approved intent set.
- **Refuse a dirty diff set.** Write `$GF_CHECKOUT/qa-diff.md` as a diff set (`source: diff`, `branch: "feat/verify"`, `base: "main"`, `revision: "<first 7 of HEAD>+dirty"`, `review: "1"`, `regressions: []`, the same two scenarios without `covers`, and the same body). `$C cli scen-dirty cr scenarios upload qa-diff.md` exits non-zero with a message naming `+dirty`, and `$C api sets …` lists no diff version.
- **Upload a diff set.** Set `revision` to the first 7 characters of HEAD and run `$C cli scen-diff-up cr scenarios upload qa-diff.md --json`. Exit `0`; `.source` `diff`, `.version` `1`. The verdict now has a `diff set v1` line (diff sets are never approved, so it shows no approval) and a `scenario_results_missing  [diff]` reason.

## Gotchas

- Any byte change is a new version with a new digest, and needs a new approval. Do not let an editor rewrite line endings or add a trailing newline between upload and approve.
- Section digests cover only `###`/`####` headings of the form `<id>. ` between `## Scenarios` and the next `##`. A scenario whose heading number does not match its front matter `id` is refused by the CLI before upload.
- The default policy's `scenarios.approver` is `any`, so the key that opened the change request may approve. Under `other_member` the same key gets a `403`; that is the policy working, not a CLI failure.
- `covers` items take `{capability, requirement, scenario}` or `{capability, requirement, implied: true}`. goodstack's templates also write `{proposal: true}`, which goodflows_web `6b0ea9e` refuses with a `422` on `content.scenarios`; until the server accepts it, record that as a contract gap, not a CLI failure.
- A set carrying `result`, `failed_step`, `evidence` or `reason` in its front matter is refused by the server: results go in a verify run.
