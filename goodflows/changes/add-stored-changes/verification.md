---
format: goodflows-verification/1
change: add-stored-changes
base: ec6939766551a76ad2563a145981385a10977b25
deltas:
  "change-requests": dfaa826d7536fc7e615ad9eafbea629fa6ede9595335738fb5e1274021a89be2
  "stored-changes": 7a1ec3f784029984819b52aea7480b3633137b875272919e9475cbd7d4a9e97d
files:
  "src/change_requests/api.rs": ec3703105e6abd63f2ee7ae1bd2d0d1fdac3ed7cb3a6ec4f0930d12d14a8529b
  "src/change_requests/changes.rs": 22e26ef61ad2e7874bc23242c251f032f1fcdbb13c7741faa41958926f2f04af
  "src/change_requests/mod.rs": e2493f3740260955e3122d721fb011b3a05b92791b65c39be15a0ae126078183
  "src/change_requests/stored.rs": 8b3e0a41b51c9b4465938100e5ef00d0022062e5988610b3535e57e8a68b347d
  "src/main.rs": bd7c2044de306d543684d346fcb8574c64ffe74cc8f9e22ebacef04549bc81c0
  "src/output/change_requests.rs": c1e2357245c50bd04978184c307992d178dd3d31b7dbe20384b5679b97c9e542
  "tests/stored_changes.rs": 0e4370a12786dc6f08b13e384fd762a4d096924a73078a31aac4fd98f36f3fdb
open:
  - "change push reports the version it created or kept | question | .goodflows/build/add-stored-changes/verify-bars-1.md | D53 (by: agent, one_way door) defines change_push.change as the server's push answer without duplicate and replaced_version, refining D46. Keep that definition? | Recommend: yes, keep D53"
  - "change show prints one version or one of its files | question | .goodflows/build/add-stored-changes/verify-bars-1.md | D52 (by: agent, one_way door) makes show --version N send a second read, GET /changes/:id, because the server's version read carries no linked change requests. Keep it? | Recommend: yes, keep D52"
  - "change list shows every stored change | not_verified | .goodflows/build/add-stored-changes/verify-qa-1.md | Linked change request numbers on list rows were checked only against the mock; the server copy's GET /changes rows do not carry change_requests yet (agreed with the server thread, not shipped) | Recommend: re-drive scenario 5 step 1 once the server ships it"
  - "scenario 15 | not_verified | .goodflows/build/add-stored-changes/verify-qa-1.md | The pk_ 403 path for show --file and pull was not driven live: the verify skill mints only sk_ keys | Recommend: none"
  - ".agents/skills/verify-goodflows-cli/features/README.md | question | .goodflows/build/add-stored-changes/verify-qa-1.md | The verify skill has no stored-changes feature file; scenarios were driven ad hoc. Add one through gs-maintain-verification-skill? | Recommend: yes, as part of ship's features step"
---

# Verification: add-stored-changes

## What each verifier checked

- **Spec (verify-spec-1):** walked every requirement of both deltas against the code in `src/change_requests/stored.rs`, `src/main.rs` and `src/output/change_requests.rs`, including push, list, show, versions, pull (paths, all-or-nothing install), delete, the 404 probe for older servers, and the shared event and exit-code conventions. All requirements are built. It flagged D53, a `by: agent` edit to the public `change_push.change` field on a one_way change, for a person to confirm. It also flagged that an unchanged push printed the sent provenance instead of the kept version's.
- **Drift (verify-drift-1):** confirmed `change-approved.md` matches the approved `change.md`. Every working-tree delta edit is covered by a `by: agent` "Found while building" decision (D51–D55) cited on its Refs line. It found no behavior that the requirements do not state. Two suggestions: the unchanged-push provenance, and a missing decision for the server's version-entry field names (`version`, `pushed_by.user_id`).
- **Bars (verify-bars-1):** checked every `by: user` and `by: agent` decision, the non-goals and anti-goal A1 against the code. It also checked the error-wording bar and the test bar. Findings: errors without a next step, no test for C1 control characters in the branch name, no test for pull's real write path, D51 untested for `--version N`, and D52/D53 for a person to confirm.
- **QA (verify-qa-1):** drove the Must scenarios 1–14 of `qa-scenarios.md` with the verify-goodflows-cli skill against a live server: goodflows_web c9f00e2 plus the uncommitted stored-changes server build. Run folder: `.verify/goodflows-cli/20261127T184521Z-2de3c59/`. Scenarios 1–14 passed; scenarios 10 and 13 used a stub server. Scenario 15 (Should) was not run.
- **Repair (verify-repair-2):** re-checked the six fix-1 rows from the files. All six are fixed, change validate passes, and 17 stored-change tests pass. It found one new suggestion: the read-only-directory pull test fails when run as root.

## What was fixed

- **fix-1:** an unchanged push now prints the kept version's provenance and labels the sent one "this push came from". Every listed error in `stored.rs` now gives a next step. New tests cover C1 control characters in the branch name, pull's real write path failing (a read-only `goodflows/changes`), and `pull --version N` without `latest_status`. D56 records the server's version-entry field names. Each new test was checked by a mutation.
- **fix-2:** the read-only pull test returns early when `geteuid() == 0`, as `tests/history.rs` does.

## Not verified

- **Linked change request numbers on `change list` rows:** checked only against the mock. The server build's `GET /changes` rows do not carry `change_requests` yet (agreed with the server thread, not shipped), so the live run showed `-` (verify-spec-1 spec-3, verify-drift-1 drift-3, verify-qa-1 qa-1).
- **Scenario 15, the pk_ 403 path for `show --file` and `pull`:** not driven live, because the verify skill mints only sk_ keys (verify-qa-1 qa-2).
- **The root skip in the read-only pull test:** not run as root, so the skip itself was not watched (fix-2).
