# goodflows CLI verification map

This directory is the maintained source for verifying the goodflows CLI's server commands against a real GoodFlows Web server. Read this index before driving, then use the matching feature file as the recipe.

## Baseline preconditions

- Launch with `eval "$(.agents/skills/verify-goodflows-cli/scripts/control-goodflows-cli launch)"` from this repository's root, with `GOODFLOWS_WEB_DIR` naming a set-up goodflows_web checkout. This exports `RUN_ID`, `GF_URL`, `GF_RUN_DIR` and `GF_CHECKOUT`.
- `C=.agents/skills/verify-goodflows-cli/scripts/control-goodflows-cli` is set in the shell.
- `$C doctor` prints only `ok` lines.
- The project is new: no change requests, no review policy (version 0), no feature index, no verify runs.
- The scratch checkout is on `feat/verify`, one commit (`add login`, adding `lib/login.ex`) ahead of `main`, and `main` is pushed to `origin`.
- Never drive a server this run did not launch.

## Driving conventions

- Start each recipe from the baseline unless it says it builds on another. Recipes that build on [Change request lifecycle](./change-request-lifecycle.md) say so; drive that one first in the same run.
- Run the CLI only through `$C cli <name> …`, git only through `$C git …`, and server reads only through `$C api …`.
- Treat every command and file body as literal. File bodies are written into `$GF_CHECKOUT` with `cat >… <<'EOF'`; substitute `$HEAD` (the output of `$C git rev-parse HEAD`) where a recipe names it.
- Assert on `--json` output with `jq` on `$GF_RUN_DIR/cli/<name>.stdout`; capture the same command's text output once.

## Proof and skip reporting

- CLI proof is the saved command, stdout, stderr and exit code.
- Every mutation also needs a second view: an `api` read of the stored record, a file in the checkout, or `git ls-remote`.
- Record each driven sub-feature with `$C record <id> "<entry point>" <pass|fail> <artifacts...>`, then run `$C finish`.
- When the server refuses a request, record `fail` with the CLI's message and the matching line from the server log in goodflows_web's run folder. Do not change the recipe's files to get past it.
- Report an unreachable path with the attempted command and the unmet precondition. Do not report a skipped entry point as verified through another path.

## Feature entry contract

Each feature file starts with an H1 title and one paragraph describing the behavior a user (usually a coding agent) sees. It then has exactly four H2 sections in this order: `Sub-features`, `How to get to it (user POV)`, `Driving it with control-goodflows-cli`, and `Gotchas`.

## Features

- [Change request lifecycle](./change-request-lifecycle.md) covers `cr path`, `cr open` before code and from a claims file, `cr claims`, `cr push`, `cr ready|draft|close|reopen`, `cr status` and `cr verdict`.
- [Scenario sets](./scenario-sets.md) covers `cr scenarios upload|approve|reject|diff` for an intent set approved before code.
- [Diff-only change requests](./diff-only.md) covers a change request with no linked change under `scenarios.diff_only = "weakening"`: the default diff set path, `cr scenarios list`, `cr scenarios diff --source diff` against the baseline, accepting a weakened diff set through `cr answer`, and a docs-only push. Drive it in its own run.
- [Verify runs](./verify-runs.md) covers `cr runs upload` of a `.verify` run folder, with and without evidence files.
- [Review policy](./review-policy.md) covers `policy get|set|history`, `features sync`, how a policy version changes the verdict, and `cr defect` with `policy calibration`.
- [Stored changes](./stored-changes.md) covers `change push|list|show|versions|pull|delete` on a server with the stored-changes routes, and `change push|list` against `$C old-server`, a stub of a server without them. Drive it in its own run: it links change request 1 to `add-login`.
- [Review and events](./review-and-events.md) covers `cr review`, `cr answer`, `cr tasks`, `cr wait`, `events`, `cr lease` and `cr merged`.
