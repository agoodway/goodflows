# Verify runs

After a verify skill drives a scenario set, its run folder `.verify/<app>/<run-id>/` holds `scenarios.md` (the set, copied byte for byte), `run.md` (the `verify-run-notes/1` front matter and a Markdown index) and the evidence files. An agent uploads the folder with one command; the CLI builds the `verify-run/1` document, uploads each evidence file to the URL the server returns, commits the run, and prints the verdict that grades the set with it.

## Sub-features

- `runs-upload` uploads a run folder with evidence and commits it.
- `runs-upload-again` uploads the same folder again without creating a second run.
- `runs-grading` shows the run's results in the verdict's scenario set line.
- `runs-refused` refuses a folder whose `set.sha256` disagrees with `scenarios.md`, before any request.

## How to get to it (user POV)

- `goodflows cr runs upload <.verify run folder> [-n <number>] [--json]` from the branch's checkout, then `goodflows cr verdict`.

## Driving it with control-goodflows-cli

Preconditions: builds on [Scenario sets](./scenario-sets.md) through **Reject** (intent version 2 approved) and on [Change request lifecycle](./change-request-lifecycle.md) through **Push**, so change request 1 has Git facts for HEAD.

- **Write the run folder.** Let `R=$GF_CHECKOUT/.verify/web/run-1`. Copy the intent set: `cp $GF_CHECKOUT/openspec/changes/add-login/qa-scenarios.md $R/scenarios.md`. Write `$R/evidence/login.txt` containing `dashboard shown` and `$R/run.md`, substituting `$HEAD` and `$SHA` (`sha256sum $R/scenarios.md | cut -c1-64`):

  ```markdown
  ---
  format: "verify-run-notes/1"
  commit: "$HEAD"
  branch: "feat/verify"
  dirty: false
  started_at: "2026-11-21T10:00:00Z"
  finished_at: "2026-11-21T10:05:00Z"
  set: {path: "openspec/changes/add-login/qa-scenarios.md", sha256: "$SHA"}
  results:
    - {id: 1, result: passed, failed_step: null, reason: null, evidence: ["evidence/login.txt"]}
    - {id: 2, result: failed, failed_step: "1", reason: "No error shown", evidence: []}
  features:
    - {id: sign-in, entry_point: "/login", result: passed, notes: "dropped by the CLI"}
  ---

  # Verify run 1

  | # | Scenario | Result |
  |---|---|---|
  | 1 | User logs in | passed |
  | 2 | Wrong password is refused | failed |
  ```

- **Upload.** `$C cli runs-up cr runs upload .verify/web/run-1 --json`. Exit `0`; the output names a `run_id`, `status` `final`, `results` `{passed: 1, failed: 1, not_run: 0}`, `artifacts` `1`, and the change request's verdict. Second view: `$C api run GET "/api/v1/projects/{project}/verify-runs/<run_id>"` has `status` `final`, `scenario_set.digest` equal to `$SHA`, `features` exactly `[{id: "sign-in", entry_point: "/login", result: "passed"}]`, and one artifact `evidence-login.txt` whose `download_url` returns the file byte for byte (`curl -s "$url" | cmp - $R/evidence/login.txt`).
- **Upload again.** `$C cli runs-up-again cr runs upload .verify/web/run-1 --json` exits `0` with the same `run_id` (the id derives from the folder) and `.duplicate` true, and `$C api runs GET /api/v1/projects/{project}/verify-runs` lists one run.
- **Grading.** `$C cli runs-verdict cr verdict`. The intent set line reads `approved  passed 1, failed 1, not run 0  (failed: 2)` and `counted runs: 1`. Scenario 2 is `must` in intent version 2, so the verdict has a `blocked  must_failed  [intent, 2]` reason.
- **Refused before sending.** Copy the folder to `run-2`, append a line to `run-2/scenarios.md`, and run `$C cli runs-bad cr runs upload .verify/web/run-2`. It exits non-zero naming `set.sha256` and `scenarios.md`; the run list still has one run.

## Gotchas

- Evidence goes to launch's versitygw. A `503` "Artifacts need object storage, which is not configured" means the server started without `CACHE_S3_*`: `$C doctor` shows whether versitygw is up. A `422` naming the artifacts as missing or not matching means the store did not return the checksum on HEAD.
- `commit` must be the full 40-character SHA; a run whose commit is not the change request's head (or an ignored-path ancestor) is stored but not counted.
- The run folder is under `.verify/`, which the scratch checkout does not ignore. Do not commit it: an untracked file changes nothing, but a commit moves HEAD and the run stops counting.
- The server accepts only `id`, `entry_point` and `result` per feature; the CLI drops other keys, so `notes` above must not reach the stored run.
- A commit answered `503` with `retry-after` is retried by the CLI; a run left `pending` after the command fails is a finding.
