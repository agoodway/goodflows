# Stored changes

An agent shares a planned goodflows change before anyone opens a branch for it: `change push` mirrors `goodflows/changes/<id>/` (or its archived directory) to the server as a new version with where it came from, `change list|show|versions` read the stored changes, `change pull` writes a version back into a checkout all or nothing, and `change delete` removes a change no change request links.

## Sub-features

- `change-push` refuses a stray file or an invalid change, finds an archived directory, sends commit, branch and dirty, keeps an identical push as `unchanged`, and names the `replaced_version`.
- `change-list` follows every page past the server's 200-row page, filters with `--status`, and shows each row's linked change request numbers, `-` when the row has none.
- `change-show` prints the latest version, `--version N`, one `--file`, and the linked change requests.
- `change-versions` lists the history newest first and marks the latest.
- `change-pull` writes a version into a fresh directory, refuses over differing local files, replaces them with `--force`, takes `--version`, refuses an archived change, and leaves the directory as it was when the write fails.
- `change-delete` needs `--yes` (exit 2), is refused naming the linking change requests, and deletes an unlinked change (204).
- `change-old-server` says a server without `/changes` routes does not store changes yet, and keeps the plain 404 for a missing project.

## How to get to it (user POV)

- `goodflows change push|show|pull|versions|delete <id>` and `goodflows change list [--status proposed|approved|archived]`, each with `--json`, from inside the checkout; `show` takes `--version N` and `--file <path>`, `pull` takes `--version N` and `--force`, `delete` takes `--yes`.

## Driving it with control-goodflows-cli

Preconditions: the baseline, on a goodflows_web that has the stored-changes routes: `$C api openapi GET /api/v1/openapi` lists `/api/v1/projects/{project}/changes` under `.paths`. Let `A=$GF_CHECKOUT/goodflows/changes/add-login` and `snap() { (cd $A && find . -type f | sort | xargs sha256sum); }`.

- **Write the change.** Write `$A/change.md`, `$A/specs/auth/spec.md` and `$A/qa-scenarios.md` below, then `$C git add goodflows && $C git commit -qm 'plan add-login'`. `$C cli validate change validate add-login` exits `0`.

  ```markdown
  ---
  format: goodflows-change/1
  id: add-login
  status: proposed
  size: small
  door: two_way
  capabilities: [auth]
  depends_on: []
  goals: [{id: G1, text: Sign in}]
  non_goals: []
  anti_goals: []
  decisions: []
  left_to_builder: [The form]
  ---

  ## Why

  People sign in.
  ```

  `specs/auth/spec.md` is `## ADDED Requirements`, a blank line, `### Requirement: Sign in`, `Refs: G1`, `People SHALL sign in.`. `qa-scenarios.md` is a `qa-scenarios/2` intent set for `change: add-login` with `specs: uncommitted` and one `must` scenario `1. Sign in` covering `{capability: auth, requirement: Sign in, implied: true}`, with `- Expect: signed in.`.
- **Push refusals.** `echo x > $A/notes.txt`; `$C cli push-stray change push add-login` exits `1` naming `notes.txt`; `rm $A/notes.txt`. `sed -i '/^Refs:/d' $A/specs/auth/spec.md`; `$C cli push-invalid change push add-login` exits `1` saying nothing was sent and naming the missing `Refs:` line; `$C git checkout -- goodflows`. Second view: `$C api none GET /api/v1/projects/{project}/changes` has `data` `[]`.
- **Push.** `$C cli push change push add-login --json`. Exit `0`; `.status` `created`, `.version` `1`, `.replaced_version` null, `.change.status` `proposed`, `.source` `{commit: $HEAD, branch: "feat/verify", dirty: false}`. Second view: `$C api v1 GET /api/v1/projects/{project}/changes/add-login` has `version` `1` and each file byte for byte (`jq -j '.data.files[]|select(.path=="change.md").text' $GF_RUN_DIR/api/v1.json | cmp - $A/change.md`).
- **Unchanged push.** `$C cli push-again change push add-login --json` exits `0` with `.status` `unchanged`, `.version` `1` and `.replaced_version` `1`.
- **Replaced version, dirty.** `echo Edited. >> $A/change.md`; `$C cli push-dirty change push add-login` exits `0` printing `Pushed change add-login as version 2; replaced version 1 (status proposed)` and a `from commit … branch feat/verify  dirty` line. Second view: `$C api v2 GET /api/v1/projects/{project}/changes/add-login/versions/2` has `source.dirty` true. Then `$C git commit -qam 'edit add-login'`, so HEAD holds version 2.
- **Archived lookup.** `O=$GF_CHECKOUT/goodflows/changes/archive/2026-11-20-old-login`; `mkdir -p $O/specs/auth`; copy the three files of `$A` with `sed 's/add-login/old-login/; s/^status: proposed/status: archived/'`. `$C cli push-archived change push old-login --json` exits `0` with `.dir` `goodflows/changes/archive/2026-11-20-old-login` and `.change.status` `archived`.
- **Link a change request.** `$C cli cr-open cr open --change add-login --title "Add login" --json` exits `0` with number `1`.
- **List paging and --status.** Push 200 more changes, so the list spans two pages: for `i` in `$(seq -w 1 200)`, copy `$A` to `goodflows/changes/bulk-$i` with `sed "s/add-login/bulk-$i/"` (and `s/^status: proposed/status: approved/` for `bulk-001` only), run `$C cli bulk change push bulk-$i --json >/dev/null`, stop unless `cli/bulk.exit` is `0`, and remove the directory. `$C cli list change list --json` exits `0` with 202 `.changes`. Second view: `$C api page1 GET '/api/v1/projects/{project}/changes?limit=200'` has 200 rows and a non-null `next_cursor`. `$C cli list-text change list` prints 202 lines `<id>  <status>  v<N>  <pushed_at>  <links>`; the `add-login` line ends `#1` when its `--json` row has `change_requests`, and `-` when it lacks it. `$C cli list-approved change list --status approved --json` lists only `bulk-001`; `$C cli list-archived change list --status archived --json` lists only `old-login`; `$C cli list-bad change list --status done` exits `2`.
- **Show.** `$C cli show change show add-login` prints `add-login version 2  proposed`, the commit, `branch feat/verify  dirty`, `Change requests: #1` and the three files. `$C cli show-v1 change show add-login --version 1 --json` has `.version` `1` and `.change_requests` naming number `1`. `$C cli show-file change show add-login --file qa-scenarios.md` prints the file byte for byte (`cmp $GF_RUN_DIR/cli/show-file.stdout $A/qa-scenarios.md`). Second view: `$C api cr-1 GET /api/v1/projects/{project}/change-requests/1` has `change_id` `add-login`.
- **Versions.** `$C cli versions change versions add-login` prints `v2 … latest` then `v1`, each with commit, branch and dirty. `$C cli versions-json change versions add-login --json` has `[.versions[].version]` `[2,1]` and only the first `latest`. Second view: `$C api versions GET /api/v1/projects/{project}/changes/add-login/versions` has versions `[2,1]`.
- **Pull fresh.** `rm -rf $A`; `$C cli pull change pull add-login`. Exit `0`, `Wrote version 2 of add-login to goodflows/changes/add-login (3 files)`. Second view: `{ snap; echo "status: [$($C git status --porcelain -- goodflows/changes/add-login)]"; } > $GF_RUN_DIR/pull-fresh.txt` lists the three files and `status: []`, so they equal HEAD's and nothing was staged.
- **Pull refused.** `echo local >> $A/change.md`; `mkdir -p $A/specs/login && echo x > $A/specs/login/spec.md`; `snap > $GF_RUN_DIR/pull-before.txt`. `$C cli pull-refused change pull add-login` exits `1` listing `change.md, specs/login/spec.md`; `snap > $GF_RUN_DIR/pull-after-refused.txt` equals `pull-before.txt` (`cmp`).
- **Pull write failure.** `chmod a-w $GF_CHECKOUT/goodflows/changes`; `$C cli pull-fail change pull add-login --force` exits `1` saying nothing was written; `chmod u+w $GF_CHECKOUT/goodflows/changes`. `{ snap; ls -A $GF_CHECKOUT/goodflows $GF_CHECKOUT/goodflows/changes; } > $GF_RUN_DIR/pull-after-fail.txt` holds the same hashes as `pull-before.txt` and no leftover `.pull-*` entry.
- **Pull --force, --version.** `$C cli pull-force change pull add-login --force` exits `0` with `Deleted: specs/login/spec.md`; `$C git status --porcelain -- goodflows/changes/add-login` is empty and `specs/login` is gone. `$C cli pull-v1 change pull add-login --version 1 --force --json` exits `0` with `.version` `1`; `$C git diff > $GF_RUN_DIR/pull-v1.diff` shows only the `Edited.` line removed from `change.md`. `$C git checkout -- goodflows`.
- **Pull archived.** `rm -rf $GF_CHECKOUT/goodflows/changes/archive`; `$C cli pull-archived change pull old-login --force` exits `1` saying `old-login` is archived and nothing was written; `$GF_CHECKOUT/goodflows/changes/old-login` does not exist.
- **Delete.** `$C cli delete-noyes change delete add-login` exits `2` asking for `--yes`. `$C cli delete-linked change delete add-login --yes` exits `1` naming `#1 (draft)`; `$C api still GET /api/v1/projects/{project}/changes/add-login` is `200`. `$C cli delete change delete bulk-200 --yes --json` exits `0` with `.deleted` true; `$C api gone GET /api/v1/projects/{project}/changes/bulk-200` is `404`.
- **Older server.** `$C old-server` starts a stub of a released server. `$C old-cli old-push change push add-login` and `$C old-cli old-list change list` exit `1` with `the GoodFlows server does not store changes yet`. `sed 's/^project = .*/project = "no-such-project"/' $GF_CHECKOUT/goodflows.toml > $GF_CHECKOUT/.goodflows/other.toml` (a config needs its jobs too); `$C old-cli old-noproject --config .goodflows/other.toml change list` exits `1` with `not found (HTTP 404)`. Second view: `$GF_RUN_DIR/old-server.log` shows each `/changes` 404 followed by exactly one `GET …/change-requests?limit=1` probe, `200` for the run's project and `404` for `no-such-project`.

## Gotchas

- Released goodflows_web servers have no `/changes` routes; check the precondition before driving, or every step but the older-server one fails with that message. The older-server step uses `$C old-server`, a stub that answers like a released server (same JSON 404 for an unknown route and an unknown project), not a released build.
- List rows carry `change_requests` only on servers that ship it; the uncommitted c9f00e2 build does not, so its `add-login` line ends `-` even while `show` names `#1`. `tests/stored_changes.rs` (`list_follows_every_page_and_filters_by_status`) covers a row with numbers.
- A pull from a server answering `../`, absolute or non-change paths needs a server that misbehaves; `tests/stored_changes.rs` (`pull_refuses_a_version_with_a_path_outside_the_change_files`) covers it, not this recipe.
- The write-failure step relies on `chmod a-w`; as root it does not fail, so drive it as an ordinary user.
- Never `$C api` an unknown route: the dev server's 404 page echoes the request, key included. Drop such a capture.
- Pushing an active `goodflows/changes/old-login/` beside its archive is refused locally; pulling while `goodflows/changes/archive/*-old-login` exists is refused before the server's archived check, which is why that step removes the archive first.
