# Review policy

An owner or admin reads the project's `review-policy/1` TOML, edits it, and writes it back as a new version; the CLI refuses to write a version identical to the current one, and every change request's verdict is recomputed against the new version. `features sync` uploads the verify skill's feature files as the project's feature index, which the coverage rules read.

## Sub-features

- `policy-get` prints the current policy, its version and the effective settings.
- `policy-set` writes a new version with a note.
- `policy-set-same` skips a write identical to the current version.
- `policy-set-stale` refuses a write whose `--previous-version` is not current.
- `policy-history` lists versions newest first.
- `policy-verdict` shows the verdict naming the new version.
- `features-sync` uploads the feature index and shows it in the verdict's coverage line.
- `calibration-defects` records defects for a merged change request and an unattributed commit, and `policy calibration --json` lists the defect with its lane, policy version, rules and globs.
- `calibration-text` prints the calibration summary and defect table.

## How to get to it (user POV)

- `goodflows policy get [--version N] [--effective]`, `goodflows policy set <file> [--previous-version N] [--note <why>]`, `goodflows policy history [--limit N]`, `goodflows features sync [--dir <features dir>]`, `goodflows cr defect (-n N | --commit REV) [--ticket ID] [--severity S]`, `goodflows policy calibration [--from T] [--to T]`, each with `--json`.

## Driving it with control-goodflows-cli

Preconditions: the baseline. The verdict steps build on [Change request lifecycle](./change-request-lifecycle.md) through **Open from claims**.

- **Get.** `$C cli policy-get policy get --json`. Exit `0`; `.policy.version` is `0` (no version written yet) and `.policy.policy` is the default policy, with `format` `review-policy/1` and `size.max_changed_lines` `1500`. `$C cli policy-get-text policy get > /dev/null`; its stdout file starts with `# review-policy version 0 (previous -), written - by -` and a `# active settings:` line, then the TOML from `format = "review-policy/1"`. The `#` lines are TOML comments, so the file is valid TOML as a whole.
- **Set.** Copy the text output to `$GF_CHECKOUT/policy.toml` (`cp $GF_RUN_DIR/cli/policy-get-text.stdout $GF_CHECKOUT/policy.toml`), change `max_changed_lines = 1500` to `max_changed_lines = 1`, and run `$C cli policy-set policy set policy.toml --note Tiny-changes-only --json`. Exit `0`; `.written` true and `.policy.version` `1`. Second view: `$C api policy GET /api/v1/projects/{project}/review-policy` has `data.version` `1` and `data.policy.size.max_changed_lines` `1`.
- **Same again.** `$C cli policy-set-same policy set policy.toml`. Exit `0`; it prints `Review policy version 1 was already written`, and `policy` read again is still version `1`.
- **Stale write.** Change the value to `2` and run `$C cli policy-set-stale policy set policy.toml --previous-version 0`. Exit `1` with `version 0 is no longer the current policy version, because another write landed first`; the stored version is still `1`.
- **History.** `$C cli policy-history policy history`. Lists version `1` with the note `Tiny-changes-only`.
- **Verdict.** `$C cli policy-verdict cr verdict`. The verdict names `policy v1` and a `max_changed_lines` reason, because the branch changes more than `1` line.
- **Feature index.** Write `$GF_CHECKOUT/.agents/skills/verify-web/features/sign-in.md`:

  ```markdown
  ---
  id: sign-in
  title: "Sign in"
  paths: ["lib/login.ex"]
  ---

  # Sign in
  ```

  Run `$C cli features-sync features sync --json`. Exit `0`; `.version` `1`, `.features` `["sign-in"]` and `.skipped` `[]`. `$C cli features-verdict cr verdict` shows a `coverage: feature index v1` line.

- **Calibration.** Builds on [Change request lifecycle](./change-request-lifecycle.md) through **Open from claims**; it merges change request `1`, so drive it last. Write `$GF_CHECKOUT/calibration-policy.toml`:

  ```toml
  format = "review-policy/1"

  [[paths]]
  glob = "lib/**"
  outcome = "needs_review"
  ```

  Run `$C cli calibration-policy policy set calibration-policy.toml --note Watch-lib --json` (`.written` true; call its `.policy.version` `N`) and `$C cli calibration-ready cr ready`, whose verdict has `path_match` and `proof_below_minimum` reasons under `policy vN`. Merge without a lease: `$C git checkout -q main`, `$C git merge --no-ff -qm merge feat/verify`, `$C git push -q origin main`, then `$C cli calibration-merged cr merged -n 1 --commit HEAD`, which prints `#1 merged` and `merged without a mergeable verdict`. Record two defects: `$C cli defect-number cr defect -n 1 --ticket GOO-1 --severity high --json` (`.defect.attribution` `change_request`) and `$C cli defect-commit cr defect --commit 'HEAD^1' --json`, the `main` commit before the merge, which no merged change request's Git facts list (`.defect.attribution` `unattributed`, `.defect.change_request` null). Then `$C git checkout -q feat/verify`.

  Run `$C cli calibration policy calibration --json`. Exit `0`; `.report.lanes.bypass` has `merged_count` `1` and `defect_count` `1`; `.report.defects` holds one entry with `change_request` `1`, `ticket` `GOO-1`, `severity` `high`, `lane` `bypass`, `policy_version` `N`, `rules` `["path_match", "proof_below_minimum"]` and `globs` `["lib/**"]`; `.report.defects_truncated` is false and `.report.unattributed_defect_count` is `1`. Run `$C cli calibration-text policy calibration`: it prints `  bypass  1 merged  1 defective  1 defects  100%`, a `Defects:` line ending `#1  high  GOO-1  bypass  vN  path_match,proof_below_minimum  lib/**`, and `Unattributed defects recorded in the window: 1`. Second view: `$C api calibration GET /api/v1/projects/{project}/calibration` returns `200` with the same entry.

## Gotchas

- `policy set` sends the TOML as written; the server parses it. A TOML error is the server's `422`, quoted by the CLI.
- Only owners and admins write policy. The launch key belongs to the seeded owner, so a `403` here is a finding.
- Limits must be positive: `max_changed_files = 0` is a `422`.
- Set the policy back (`max_changed_lines = 1500`, a new version) before driving recipes that expect the default verdict.
- The calibration defect list and `unattributed_defect_count` need goodflows_web with agoodway/goodflows_web#32 (`a0994a3` or later). An older server sends neither, and the text output stops after the `unmatched` line.
- The launch key is `sk_`, so calibration shows tickets. A `pk_` key gets `ticket` null in each defect; this recipe does not mint one.
- `features sync` with no `--dir` needs exactly one `verify-*/features` directory in the checkout's skills directories; the scratch checkout has only the one written above.
