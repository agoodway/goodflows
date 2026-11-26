---
format: goodflows-verification/1
change: add-schema-command
base: 7f03965dcefd5709436b236a7112f028b89b2555
deltas:
  "change-requests": 498c15164415245549d2c81ee522d2b9515817b4b841f66789fd17b6ccaa5fd1
  "file-schemas": a9afe104491d28b621be7c7585132b437010476eb0aaa51f20a5685c61140b9b
files:
  "README.md": 492ca900a2ea04b0d93f698d149e77ad3b27b43106dc356cf28419ad676df1ff
  "skills/gf-change-request/SKILL.md": 51d81dbecfca2d04b98834dde16e09541a5fe96ba0e24f67ca676ebfd1108d94
  "skills/gf-change-write/SKILL.md": e3f1fd61fd2db38238b832270b4b8655edc23876128d8bec36772e6953bdd108
  "src/agents_guide.md": 68e611f4cc4f3c0195c2877988af2e161117a1c0fdc991fb6a0d47a8095970c0
  "src/change_requests/markdown.rs": e6230751a6a714f35e5eecc7607973e9399f5200f3b5ac053d464516ad014600
  "src/change_requests/mod.rs": 8f9bba174a34464a14dac81297dcc721f8ca2a844fa39d6a5e02202696d6de07
  "src/change_requests/verify_run.rs": eb5d540f28f78c44f12fe13b6158d44b134383918bcabe44302d02f493a366cd
  "src/main.rs": ae10310d7653b06d4aa303d1892007095d7ef09a8692f9284ad5bede57f17a3d
  "src/output.rs": 20e29244436cb864b7b53a780d96158ae3d956add6e1fa27d9c85f574c3f9a6f
  "src/schema.rs": e01c377f14720a12b73e555deb34404059a78b5e820d5de45fc42bc924960ec8
  "src/skills.rs": 806f495d051d0140acc34e1f1fc93a39e77b1f0d447638f6fa530edb024b2898
  "tests/change_requests.rs": c5d3fe8121454d7e9c881856b2c76e3d584d478c709d4f6c946cabe4e0f96576
  "tests/schema.rs": aa89e13eca203760f4431e8756a48408799919a885dca9fed5c8a2feb1885f8c
open: []
---

# Verification

Each check ran twice: on the first build (4d7d597), and on the approved revision (559047b) after the review fixes. In the last round, the spec and qa checks reported no rows, and the rows from bars and drift were fixed.

- **spec:** checked each ADDED and MODIFIED requirement against the binary in scratch directories and against goodflows_web's ScenarioSets.Document, ChangeRequests.ClaimsDocument, FeatureIndex and ReviewPolicies.Glob. Every key the server accepts is named in the field rules, and the limits match. Round 1 found that the AGENTS.md markers were matched as substrings, so a rerun could delete project text after a stray or unpaired marker (A2). Markers now match only as whole lines.
- **drift:** traced every behavior in the diff to a requirement. It confirmed the parser refactors change no message or accepted value, and that the skill rewrites lose no workflow rule. Round 2's dropped "one sentence per note" rule was restored in gf-change-request. The decisions added after the first build (D6, U6–U8, D7, D8), the change-requests delta and scenario 8 were added while the change was proposed, and the user approved that revision.
- **bars:** checked the decisions, goals, non-goals and anti-goals, and used mutations to show that each Must scenario has a test that fails. Round 1 added tests for the text output and nonempty sections. Round 2 added an integration test that keeps the claims findings as warnings (N3) and one that asserts the validator line in `schema list`. Each of those mutations now fails a test.
- **qa:** drove all eight Must scenarios through the binary with no server or config (run folders `.verify/goodflows-cli/20261126T144020Z-4d7d597/` and `20261126T150742Z-559047b/`). All pass, and `goodflows cr validate` accepts both run folders.

Full check gate: `goodflows` (fmt, clippy, test, doc) passed on the final tree.

Not verified: "sends no request" was checked with a local listener and by reading the code, because strace is not in the orb.
