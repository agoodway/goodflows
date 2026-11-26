---
format: qa-scenarios/2
source: intent
change: add-schema-command
specs: uncommitted
scenarios:
  - id: 1
    name: List names every format and its validator
    priority: must
    covers:
      - {capability: file-schemas, requirement: schema list names every format, implied: true}
      - {capability: file-schemas, requirement: schema works without a server or config, implied: true}
  - id: 2
    name: Show prints each section for each format
    priority: must
    covers:
      - {capability: file-schemas, requirement: schema show prints what an agent needs to write the file, implied: true}
  - id: 3
    name: JSON show is one event without a JSON Schema
    priority: must
    covers:
      - {capability: file-schemas, requirement: schema show prints what an agent needs to write the file, implied: true}
  - id: 4
    name: Bad names and sources are usage errors
    priority: must
    covers:
      - {capability: file-schemas, requirement: schema names and sources are checked, scenario: Missing source}
  - id: 5
    name: Filled templates pass their validators
    priority: must
    covers:
      - {capability: file-schemas, requirement: Each template passes its validator once filled, implied: true}
  - id: 6
    name: Install writes and refreshes the AGENTS.md block
    priority: must
    covers:
      - {capability: file-schemas, requirement: skills install writes a formats block into AGENTS.md, scenario: Rerun keeps the project's text}
      - {capability: file-schemas, requirement: skills install writes a formats block into AGENTS.md, scenario: A stray marker is the project's text}
  - id: 7
    name: The agent guide and skills point at goodflows schema
    priority: must
    covers:
      - {capability: file-schemas, requirement: Agent guidance points at goodflows schema, implied: true}
  - id: 8
    name: cr validate warns about server-only claims rules
    priority: must
    covers:
      - {capability: change-requests, requirement: cr validate checks claims as cr open and cr claims read them, implied: true}
---

# QA: add-schema-command

Run these with this build in an empty directory with no `goodflows.toml` and no `GOODFLOWS_API_KEY`, unless a scenario says otherwise.

## Scenarios

### 1. List names every format and its validator (Must)

1. Run `goodflows schema list`, then `goodflows schema list --json`.

Expected: both exit 0 with no request sent. Each names `qa-scenarios/2`, `verify-run-notes/1`, `change-claims/2` and `feature-file/1`, with the file each applies to and `goodflows cr validate` or `goodflows features validate`.

### 2. Show prints each section for each format (Must)

1. Run `goodflows schema show` for `qa-scenarios/2 --source intent`, `qa-scenarios/2 --source diff`, `verify-run-notes/1`, `change-claims/2` and `feature-file/1`.

Expected: each exits 0 and prints a template, field rules, body rules, handling rules and a validate line. The intent output names `change`, `specs`, `covers` and `coverage` and not `regressions`; the diff output names `branch`, `base`, `revision` and `regressions` and not `coverage`.

### 3. JSON show is one event without a JSON Schema (Must)

1. Run `goodflows schema show change-claims/2 --json`.

Expected: exit 0 and one JSON line whose `template` is a string, `fields`, `body` and `handling` are lists of strings, and `validate` is `goodflows cr validate <file>`; there is no `$schema` key.

### 4. Bad names and sources are usage errors (Must)

1. Run `goodflows schema show qa-scenarios/2`.
2. Run `goodflows schema show qa-scenarios/1 --source diff`, `goodflows schema show nope`, and `goodflows schema show change-claims --source diff`.
3. Run `goodflows schema show qa-scenarios --source intent`.

Expected: steps 1 and 2 each exit 2 and print no schema; step 1 names both `--source` values, and the others name the accepted formats or that the format has no variants. Step 3 exits 0 and prints the intent set.

### 5. Filled templates pass their validators (Must)

1. Save each template from scenario 2 to a file, replacing each bracketed placeholder with a real value.
2. Run `goodflows cr validate` on the scenario sets and the claims file, on a run folder holding the filled run notes as `run.md` and a filled set as `scenarios.md`, and `goodflows features validate --dir` on a directory holding the filled feature file.

Expected: every command exits 0 with no errors, and the claims file gets no warning besides one about its `head`.

### 6. Install writes and refreshes the AGENTS.md block (Must)

1. In a directory with no `AGENTS.md`, run `goodflows skills install --json` with no coding agent variables set.
2. Run it again.
3. Put `# Project rules` before the block and `More rules` after it, change a line inside the block, and run `goodflows skills install` again.
4. Replace `AGENTS.md` with the two lines `<!-- goodflows:formats -->` and `Keep this line.`, and run `goodflows skills install` twice.
5. In a new directory whose `CLAUDE.md` holds `# Claude rules`, run `goodflows skills install --harness claude --json`. In another whose `CLAUDE.md` holds `@AGENTS.md`, do the same.

Expected: step 1 creates `AGENTS.md` holding one block from `<!-- goodflows:formats -->` to `<!-- /goodflows:formats -->` that names `goodflows schema show` and each format, its `files` lists `AGENTS.md`, and no `CLAUDE.md` is created. Step 2's `files` does not list `AGENTS.md`. After step 3, the file still starts `# Project rules`, still ends with `More rules`, and holds exactly one block, with the edited line restored. After step 4, `Keep this line.` is still there and the second run changed nothing. In step 5, the first `CLAUDE.md` holds `# Claude rules` followed by the same block and `files` lists it; the second is unchanged and not listed.

### 7. The agent guide and skills point at goodflows schema (Must)

1. Run `goodflows agents`.
2. Run `goodflows skills install` and read the installed `gf-change-write/SKILL.md` and `gf-change-request/SKILL.md`.

Expected: the guide has a section naming `goodflows schema list` and `goodflows schema show <format>`. `gf-change-write` names `goodflows schema show qa-scenarios/2 --source intent` and `gf-change-request` names `goodflows schema show change-claims/2`; neither holds its own front matter template.

### 8. cr validate warns about server-only claims rules (Must)

1. Write a claims file whose front matter passes `cr validate` today but has `door: maybe`, an extra key `note`, no `reach`, a fact with `proof: ran` and no `evidence`, and a question whose `recommendation` is not one of its `options`.
2. Run `goodflows cr validate <file> --json`.

Expected: exit 0 and `ok` true, with one warning naming each of the five problems.
