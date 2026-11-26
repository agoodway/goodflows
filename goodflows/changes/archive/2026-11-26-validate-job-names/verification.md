---
format: goodflows-verification/1
change: validate-job-names
base: fa3eb28998703429bb085c24f0fab9a89e7adb43
deltas:
  "name-rule": eaa63f505870023599b30f61e1e2d28cdef3c3e448e782ea85815df2b6bcc925
  "run-report": da4b48f500ffe48fe17b56c857ebce2a5bf20802cbdb0003398d355230146e2c
files:
  "README.md": 548572316a4e20121973ded1780d6a54ae4e9bbda0cf7dc3750e016f34666ef0
  "src/config.rs": 8dca9f14c18589975ac6871233e7fe53ccccd88fc9c331f84aea66affb5f1c4f
  "src/reporting/collect.rs": 3b2da28fadf41d70ea1f8777f2c1692e3a905c7516f73375e1cc0b16a7ed8977
  "src/reporting/history_client.rs": 7a2fc6ee7047c02aaa70050a21fcd9cb369911dc1fa5c12951bc6eabaf6b3d84
  "src/reporting/replay.rs": 12e00d97d90c6e6e581d1dd1f004abfb0bdbc9faa23ee2b10f868e3ea9d89395
  "src/reporting/run_contract.rs": e9de2891a2aaa7a1f7ad9fa0f9ecd2091eab7302be688bcf6a00eec48f99bf06
  "src/runner.rs": dfe0df89919500c4c81bd6a82d6832374e29d04ebc00a45e2a228c197dd9274e
  "tests/output.rs": 1b4c179b5f41bcc54dd4cfb7890b5363ac04e619483aae0ea80ab0148af2132c
  "tests/reporting.rs": 513cc402a4b3b7c5e627c4fd159999001c8702a3fdb4b1135bb27415790637bb
  "tests/runner.rs": 8b2292f4cbfee009390e76571d6d97517ce8a11cf615a488639b7832b38bb4ff
  "tests/steps.rs": ddfff909ead69f6c61c3d8f57d130f2022bbc5e92d346128b470f7bf3813f3b4
open: []
---

# Verification

Each check ran three times: on the build, after fix-1 (bars-1 to bars-4 from verify-bars-1), and after fix-2 (step ids and output keys printed raw). The last round of every verifier reported no rows.

The three open questions from the build (QA scenario 4.2, QA scenario 6, and the D6 release notes) were settled as recommended in the review of change request #1. Scenarios 4.2 and 6 were amended accordingly and re-driven, and nothing is open.

- **bars:** checked decisions D1 to D9, the goals, non-goals and anti-goals, the left_to_builder entries, and that every Must scenario has a test that fails on a wrong implementation. It probed the built binary with control characters in workflow, environment, job and step names and in output keys, and in round 3 it confirmed with a mutation that the new escaping test fails when `escape_debug` is removed. Rounds 1 and 2 found the escaping and message gaps that fix-1 and fix-2 repaired.
- **drift:** traced every behavior in the diff back to a requirement or decision, and checked that the builder's delta edits (D8, D9) are `by: agent`, start with "Found while building:", and are cited on the `Refs:` line. It found no behavior that the spec does not state.
- **qa:** ran every Must scenario in `qa-scenarios.md` through the verify-goodflows-cli skill against a local goodflows_web server (run folders `.verify/goodflows-cli/20261126T094137Z-fa3eb28/`, `20261126T095613Z-fa3eb28/`, `20261126T101218Z-fa3eb28/`). Scenarios 1, 2, 3, 4.1, 5 and 6 pass. After the review of change request #1, scenarios 4.2 and 6 were amended and re-driven (verify-qa-4, run folder `.verify/goodflows-cli/20261126T110916Z-1dbcafd/`): 4.2 now reads the 64-byte output key in `consume`'s condition, with a control that compares against a different value and skips; 6 now puts the escape sequence and carriage return in the command and its output, and the report shows them as escaped text with no raw control bytes. Both pass.
- **spec:** checked each requirement of `name-rule` and `run-report` against the code and tests: one predicate `run_contract::valid_name` for every kind of name, load-time refusal with escaped names, runtime output keys, replay and envelope revalidation, and terminal escaping.

Full check gate: `./target/release/goodflows` (fmt, clippy, test, doc) passed on the final tree (detached run `fffa62f3-c2ce-48a0-9229-871cc09e75f4`).

Not verified:

- Release notes for D6 (verify-bars-1, bars-5): the next release notes must repeat the README "Upgrading from 0.9 or earlier" note and list the removed messages. The notes are generated at release time and the repository has no changelog, so the branch cannot show them. The review of change request #1 settled that the person cutting the release copies the README paragraph into the release notes.
