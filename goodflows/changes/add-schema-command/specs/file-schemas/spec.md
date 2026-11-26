## ADDED Requirements

### Requirement: schema list names every format
Refs: U2, U4, G1
`goodflows schema list` SHALL print one entry per format this CLI checks: `qa-scenarios/2`, `verify-run-notes/1`, `change-claims/2` and `feature-file/1`, each with its name, newest version, the versions still accepted, the file it applies to, and the command that validates it. It SHALL exit 0.

### Requirement: schema show prints what an agent needs to write the file
Refs: U2, U3, U4, D4, G1
`goodflows schema show <format>` SHALL print the format's fill-in template, its field rules, its body rules, its handling rules, and the command that validates it, and exit 0. The field rules SHALL name every key the server accepts for the format. `--source intent` and `--source diff` SHALL pick the variant of `qa-scenarios/2`; the intent variant has `change`, `specs`, `covers` and `coverage`, the diff variant `branch`, `base`, `revision` and `regressions`. Each validate command is `goodflows cr validate <path>`, or `goodflows features validate` for `feature-file/1`. With `--json`, it SHALL print one JSON event with `template` (the file's text as one string), `fields`, `body` and `handling` (lists of strings) and `validate` (a string), and no JSON Schema.

### Requirement: schema names and sources are checked
Refs: D2, D3
A format MAY be named with or without its version; without it, the newest version is meant. An unknown name, a version the CLI does not accept, `--source` on a format without variants, and `qa-scenarios/2` without `--source` SHALL each exit 2 with a usage error that names what is accepted, and print no schema.

#### Scenario: Missing source
- **WHEN** a user runs `goodflows schema show qa-scenarios/2`
- **THEN** goodflows exits 2 and the error names `--source intent` and `--source diff`

### Requirement: schema works without a server or config
Refs: U1
`goodflows schema` SHALL read no `goodflows.toml`, need no API key, and send no request.

### Requirement: Each template passes its validator once filled
Refs: G2, A1, D8
Each template `schema show` prints, with its placeholders filled in, SHALL pass the validator it names: `cr validate` for a scenario set or claims file, `cr validate` on a run folder whose `scenarios.md` is a filled scenario set for `verify-run-notes/1`, and `features validate` for a feature file. A filled claims template SHALL get no warning besides one about its `head`.

### Requirement: skills install writes a formats block into AGENTS.md
Refs: U5, U7, U8, D1, D5, D6, D7, A2, G3
`goodflows skills install` SHALL write a block that starts with the line `<!-- goodflows:formats -->` and ends with the line `<!-- /goodflows:formats -->` into `AGENTS.md` in the directory it runs in, creating the file when missing. When the harness it installs for includes claude, it SHALL also write the block into `CLAUDE.md` there in the same way, unless `CLAUDE.md` has a line `@AGENTS.md`. The block SHALL tell agents to run `goodflows schema show <format>` before writing a scenario set, a run's `run.md`, a claims file or a feature file, name each format, and say to run the validator the schema names. When a file already has such a block, a line equal to the start marker followed by a line equal to the end marker with no other start line between, the block SHALL be replaced in place; otherwise it is appended. A marker inside a line, or a start line with no end line, is the project's text. Every byte outside the block SHALL stay as it was. When a file is not UTF-8 text or cannot be written, the command SHALL exit 1 naming the file; the skills it already wrote stay. The `skills_installed` event's `files` SHALL list `AGENTS.md` and `CLAUDE.md` only when the run changed them.

#### Scenario: Rerun keeps the project's text
- **WHEN** `AGENTS.md` holds the project's text, an older goodflows block, and more text after it, and a user runs `goodflows skills install`
- **THEN** the file holds the same text before and after the block, and exactly one current block

#### Scenario: A stray marker is the project's text
- **WHEN** `AGENTS.md` mentions `<!-- goodflows:formats -->` inside a line, or holds that line with no end line, and a user runs `goodflows skills install` twice
- **THEN** the first run appends a block, and after the second the file is unchanged from the first and `files` does not list `AGENTS.md`

### Requirement: Agent guidance points at goodflows schema
Refs: U5, U6, G1, G3
The guide `goodflows agents` prints SHALL have a section that says goodflows owns these formats and that `goodflows schema list` and `goodflows schema show <format>` print them. The `gf-change-write` skill SHALL point at `goodflows schema show qa-scenarios/2 --source intent` for the intent set's keys, values and template, and `gf-change-request` at `goodflows schema show change-claims/2` for the claims file's, without keeping their own copy of the key list or the template.
