---
format: goodflows-change/1
id: add-schema-command
status: archived
size: small
door: one_way
capabilities: [file-schemas, change-requests]
depends_on: []
goals:
  - {id: G1, text: "An agent in a goodflows project learns how to write each file goodflows reads (scenario sets, run notes, claims files, feature files) from the goodflows it runs, without a skill copying the format."}
  - {id: G2, text: "What `goodflows schema show` prints and what `goodflows cr validate` and `goodflows features validate` check come from the same release and cannot disagree on what the CLI checks."}
  - {id: G3, text: "An agent that opens a goodflows project finds out the formats exist without being told."}
non_goals:
  - {id: N1, text: "A JSON Schema of any format."}
  - {id: N2, text: "Fetching formats from the server."}
  - {id: N3, text: "Adding a format key to feature files, or changing what any validator accepts."}
  - {id: N4, text: "Changing goodstack's skills; they drop their copies of these formats on their own side."}
  - {id: N5, text: "Changing the cr scenarios upload error that names gs-qa-scenarios; a goodflows to goodstack reference is the allowed direction."}
anti_goals:
  - {id: A1, text: "A template that `schema show` prints and the validator it names refuses once its placeholders are filled."}
  - {id: A2, text: "Losing or rewriting a project's own AGENTS.md text outside the goodflows block."}
decisions:
  - id: U1
    question: Where do the schemas come from?
    answer: "Built into the CLI. `schema` needs no server, API key or goodflows.toml; the server stays the final check on upload, and a format change ships with a CLI release."
    by: user
    why: Works offline and in CI, and the printed rules match the validator of the same binary.
    rejected: ["Fetch the formats from the server"]
  - id: U2
    question: What are the commands?
    answer: "A top-level `goodflows schema list`, and `goodflows schema show <format> [--source intent|diff] [--json]`; `--source` picks the intent or diff variant of qa-scenarios/2."
    by: user
    why: "Accepted recommendation: the user accepted the draft artifact's command shape, which was not asked as a question of its own. Feature files are formats too and belong to features, so the command is not under cr."
    rejected: ["goodflows cr schema"]
  - id: U3
    question: What does --json print?
    answer: "One event with template, fields, body, handling and validate. No JSON Schema."
    by: user
    why: Agents write from the template and fix what the validator reports; a second hand-kept schema adds drift and helps no agent.
    rejected: ["A JSON Schema per format"]
  - id: U4
    question: Which formats are covered?
    answer: "qa-scenarios/2 (intent and diff), verify-run-notes/1, change-claims/2, and feature-file/1, a name that exists only in the schema list for verify-*/features/*.md; those files gain no format key."
    by: user
    why: "Accepted recommendation: the user accepted the draft artifact's list of formats, which was not asked as a question of its own. These are every file goodflows reads from a checkout."
    rejected: []
  - id: U5
    question: How do agents find the formats?
    answer: "A marked `<!-- goodflows:formats -->` block in the project's AGENTS.md that names `goodflows schema show <format>` and says to run the validator, replacing an existing block; and a short section in `goodflows agents` pointing at `goodflows schema list`."
    by: user
    why: "Accepted recommendation: the user accepted the draft artifact's AGENTS.md block and agents section, which were not asked as questions of their own. Agents read AGENTS.md before writing files, and goodstack skills defer to the project's guidance."
    rejected: []
  - id: D1
    question: Which command writes the AGENTS.md block?
    answer: "`goodflows skills install`, not `goodflows init`."
    by: agent
    why: "skills install already sets a project up for agents and installs the gf-change-* skills that write these files; init writes a checks config, often in projects that do not use change requests."
    rejected: ["Both commands", "Only init"]
  - id: D2
    question: What does `schema show qa-scenarios/2` do without --source?
    answer: "Exits 2 with a usage error naming both --source values; nothing is printed on stdout."
    by: agent
    why: The two variants have different keys, and silently printing one would teach an agent the wrong set.
    rejected: ["Default to the diff set", "Print both variants"]
  - id: D3
    question: How is a format named?
    answer: "As `<name>/<version>` or `<name>` alone, which means the newest version. An unknown name, a version the CLI does not accept, and --source on a format without variants each exit 2 naming what is accepted."
    by: agent
    why: Agents copy names from `schema list`, but an unversioned name is the natural first guess.
    rejected: ["Require the version"]
  - id: D4
    question: In --json, is template a list of lines like the other sections?
    answer: "No: template is the file's text as one string; fields, body and handling are lists of strings."
    by: agent
    why: An agent writes the template to a file as is; a list of lines would have to be joined first.
    rejected: ["template as a list of lines"]
  - id: D5
    question: Where does the AGENTS.md block go?
    answer: "AGENTS.md in the directory skills install runs in, created when missing. An existing block, from its start marker to its end marker, is replaced in place; otherwise the block is appended after the file's text. Every other byte stays."
    by: agent
    why: Rerunning install must refresh the block without touching the project's own guidance (A2).
    rejected: []
  - id: D6
    question: Which lines are the AGENTS.md block, and what if AGENTS.md cannot be read or written?
    answer: "The block is a line equal to the start marker up to the next line equal to the end marker, with no other start line between. A marker inside a line, or a start line with no end line, is the project's text. When AGENTS.md is not UTF-8 text or cannot be written, install exits 1 naming the file and what to do; the skills it already wrote stay."
    by: agent
    why: "Found in verification: matching the markers as substrings let a rerun delete the project's text between a stray marker, or a start line with no end line, and the new block, against A2."
    rejected: ["Match the markers anywhere in the file", "Refuse to install when a start line has no end line"]
  - id: U6
    question: Do goodflows's own skills keep their copies of the formats?
    answer: "No. gf-change-write's qa-scenarios.md section points at `goodflows schema show qa-scenarios/2 --source intent`, and gf-change-request points at `goodflows schema show change-claims/2`, for the keys, values and template. They keep the rules about the workflow: what to put in each field for this change or branch."
    by: user
    why: The user asked for it in review; a copy in a skill drifts from the format the CLI checks, as the goodstack copies do.
    rejected: []
  - id: U7
    question: Does Claude Code see the formats block?
    answer: "Yes. Claude Code reads CLAUDE.md, not AGENTS.md, so for the claude harness the block must reach CLAUDE.md too."
    by: user
    why: The user asked for it in review.
    rejected: []
  - id: D7
    question: How does the block reach Claude Code?
    answer: "When the harness `skills install` targets includes claude, it also writes the block into CLAUDE.md with the same rules as AGENTS.md, unless CLAUDE.md already has a line `@AGENTS.md`, which loads the AGENTS.md block already."
    by: agent
    why: "Adding `@AGENTS.md` to CLAUDE.md would load all of AGENTS.md into every Claude Code session, which may hold guidance a project keeps out of Claude on purpose or already copies into CLAUDE.md; the block changes only goodflows's lines and is replaced in place like the AGENTS.md one."
    rejected: ["Add an @AGENTS.md line to CLAUDE.md"]
  - id: U8
    question: Which guidance files does the skills_installed event list?
    answer: "Only the guidance files the run changed, so an unchanged AGENTS.md or CLAUDE.md is not listed."
    by: user
    why: The user asked for it in review; files lists what was written.
    rejected: ["Always list AGENTS.md"]
  - id: D8
    question: Does cr validate check the claims rules that only the server checks?
    answer: "Partly, as warnings: a missing or unknown key, a door or proof outside its values, a ran fact without evidence, and a recommendation that is not one of its options. Warnings do not fail the file, so what cr validate, cr open and cr claims accept does not change."
    by: agent
    why: "The schema prints these rules, and checking them costs one more read of the front matter with the same constants, so a filled template is checked against them; errors would change what the validator accepts (N3)."
    rejected: ["Errors that fail the file", "No check"]
left_to_builder:
  - "The wording of every printed rule and of the AGENTS.md block, with the bar that each format's fields name every key the server accepts for it, and that rules the CLI itself checks (format names, sources, result values, limits) are printed from the same constants the parsers use."
  - "Layout of the text output and the names of the JSON event types and their other keys, besides template, fields, body, handling and validate."
  - "The wording of usage errors, each naming the input at fault and what is accepted."
  - "Tests, with the bar that breaking the behavior of any Must scenario fails an automated test; the template check runs every printed template through its validator."
  - "Where the command is documented: README, `goodflows agents`, and the help text of `schema`, `cr validate` and `skills install`."
---

## Why

goodflows owns the files it reads: scenario sets, verify run notes, claims files and feature files. Today their formats live in goodstack's skills (`gs-qa-scenarios`, `gs-create-verification-skill`) and in goodflows's own `gf-change-request`, so a format change in goodflows needs a change in another repository before agents write the new shape. goodstack will stop copying these formats and defer to the project's guidance. goodflows needs a command that prints a format completely enough for an agent to write a valid file, and a way for agents in a goodflows project to find it.

## Context

The contract is spread across the parsers (`src/change_requests/markdown.rs`, `verify_run.rs`), the `after_help` of `cr scenarios upload` and `cr runs upload`, the specs `qa-scenario-sets`, `verify-runs` and `feature-index`, the `gf-change-request` skill, and goodstack's `gs-qa-scenarios` skill. The CLI's validators check less than the server: a claims file's door, reach and facts, and a scenario set's key set and lengths, are checked only by goodflows_web (`ScenarioSets.Document`, `ChangeRequests.ClaimsDocument`, `FeatureIndex`). `cr validate --help` already says so for claims.

The design draft is the artifact "Goodflows schema commands: draft" in thread T-6aca825d-0ab6-40ae-819b-be26200c2f3b; its Decisions section is recorded here as U1 to U5 and N5.

## Approaches

The schemas are compiled into the CLI as data next to the parsers, reusing the parsers' constants for format names, enumerations and limits, and the server's rules as prose. Fetching them from the server (U1) and a generated JSON Schema (U3) were rejected in the draft. Tightening `cr validate` to refuse on every server rule was considered and left out (N3): it would change what the validators accept, and the server stays the final check. The claims rules the schema prints and the CLI can check cheaply become warnings instead (D8).
