# qa-scenario-sets Specification

## Purpose

Defines how `goodflows cr scenarios` reads `qa-scenarios/2` set files, uploads them byte for byte with per-scenario section digests, lists set versions, approves or rejects the intent set, and compares versions.

## Requirements

### Requirement: Upload sends the set file byte for byte
`goodflows cr scenarios upload [FILE]` SHALL read FILE as bytes and MUST send them unchanged as `content` in `POST /api/v1/projects/{project}/change-requests/{number}/scenario-sets`, with JSON body `{content, sections}`. It MUST NOT normalize line endings, trailing whitespace or the final newline. A file that is not UTF-8 SHALL be refused with `<file> is not UTF-8`. The change request is picked as the change-requests capability specifies.

#### Scenario: CRLF and trailing spaces are kept
- **WHEN** `qa.md` uses CRLF line endings and its title line is `# QA: add-login  ` with two trailing spaces, and the user runs `goodflows cr scenarios upload qa.md --json` on a branch whose draft change request is 7
- **THEN** the CLI posts to `/api/v1/projects/goodcases/change-requests/7/scenario-sets` and the body's `content` equals the file's bytes, CRLF and trailing spaces included

### Requirement: Upload defaults to the branch's diff set file
Without FILE, `cr scenarios upload` SHALL use `qa-scenarios.md` next to the branch's claims file, `.goodflows/change-requests/<branch>/qa-scenarios.md` under the repository top level, the path `goodflows cr path --json` prints as `scenarios_path`. When that file does not exist it MUST fail with exit code 1 and `there is no scenario set at <path>, the branch's default diff set; write it there (gs-qa-scenarios) or pass the set file`.

#### Scenario: No default set yet
- **WHEN** branch `feat/login` has no `.goodflows/change-requests/feat/login/qa-scenarios.md` and the user runs `goodflows cr scenarios upload`
- **THEN** the command exits 1, its error names `qa-scenarios.md`, and no scenario set is posted

#### Scenario: Default set exists
- **WHEN** that file exists with `source: diff` and the user runs `goodflows cr scenarios upload --json`
- **THEN** the posted `content` is that file's bytes and the event's `source` is `diff`

### Requirement: A set file starts with qa-scenarios/2 front matter
A set file MUST start with a `---` line and YAML front matter up to the next `---` line, CRLF endings allowed, else `<file>: a scenario set must start with YAML front matter between --- lines`. The front matter MUST be a YAML mapping with `format: qa-scenarios/2` and `source` `intent` or `diff`. YAML anchors, aliases, tags and non-integer numbers SHALL be refused.

#### Scenario: Wrong format and source
- **WHEN** a set's front matter has `format: qa-scenarios/1` and `source: both`
- **THEN** the upload fails before any request, listing `<file>: format must be qa-scenarios/2` and `<file>: source must be intent or diff`

#### Scenario: Anchors
- **WHEN** a set lists `- &one {id: 1, name: "Q&A"}` and then `- *one`
- **THEN** the error starts `qa.md: YAML anchors and aliases are not supported` and also lists `scenario id 1 appears twice`

### Requirement: Scenario IDs are positive integers or nonempty strings
The front matter's `scenarios` MUST list at least one scenario, else `<file>: scenarios must list at least one scenario`. Each `id` MUST be a positive integer, compared as its decimal string, or a nonempty string, else `<file>: scenarios[<index>]: every scenario needs an id, a positive integer or a nonempty string`. An ID listed twice SHALL be refused with `<file>: scenario id <id> appears twice`.

#### Scenario: Integer and string IDs
- **WHEN** the scenarios have ids `1` and `S2`
- **THEN** the set's scenario IDs are `"1"` and `"S2"`, in that order

#### Scenario: Zero is not an ID
- **WHEN** a scenario has `id: 0`
- **THEN** the upload fails with `scenarios[0]: every scenario needs an id, a positive integer or a nonempty string`

### Requirement: Every problem in a set file is listed at once
The CLI SHALL check the whole front matter and every scenario heading before failing, and MUST print every problem it found, one per line, each starting with the file path, then exit 1 without sending anything.

#### Scenario: Two heading problems
- **WHEN** a set lists ids `1` and `2` and its body has `## Scenarios`, `### 1. A` and `### 1. Again`
- **THEN** the error lists both `<file>: scenario 1 has more than one heading` and `<file>: scenario 2 has no "### 2. <name>" heading under ## Scenarios`

### Requirement: Scenario headings are found under ## Scenarios
A scenario's heading SHALL be a `###` or `####` ATX heading, starting at the line's first byte, between the body's first `## Scenarios` heading and the next heading of level 1 or 2, whose title starts `<id>. ` for an ID in the front matter. Headings inside ``` or ~~~ fences, and headings with no `<id>. `, such as group headings, MUST NOT count. A body with no `## Scenarios` heading SHALL be refused with `<file>: the body has no "## Scenarios" heading`.

#### Scenario: Look-alike headings
- **WHEN** ids are `1` and `2` and the body is `## Scenarios`, `### 10. Ten`, `### 1 seat`, `### 2. Two`
- **THEN** the upload fails naming scenario 1 as having no heading, because `10.` is not `1.` and `1 seat` has no `. `

#### Scenario: A numbered heading outside the section
- **WHEN** `### 1. Not a scenario` appears under `## What changed` before `## Scenarios`, and `#### 1. Add a seat` appears under the group heading `### Free plan` inside it
- **THEN** scenario 1's section starts at `#### 1. Add a seat`

### Requirement: Each scenario's section digest covers its heading to the next heading at its level or higher
For each scenario, in front matter order, the CLI SHALL send `{id, sha256}` in `sections`, where `sha256` is the lowercase hex SHA-256 of the file's bytes from the start of the scenario's heading line up to the start of the next heading, outside fences, whose level is the same or higher (fewer `#`), or to the end of the file.

#### Scenario: Grouped set
- **WHEN** a diff set has `### Free plan`, `#### 1. Add a seat (Must)`, a fenced line `# not a heading`, `### 2 seats left`, `#### 2. Last seat (Should)` and then `## Regression checks`
- **THEN** scenario 1's digest covers `#### 1.` up to `### 2 seats left`, and scenario 2's covers `#### 2.` up to `## Regression checks`

#### Scenario: Lower-level headings stay inside
- **WHEN** `### 3. Export` is followed by `#### Notes` and then `### 4. Import`
- **THEN** scenario 3's section includes `#### Notes` and ends at `### 4. Import`

#### Scenario: CRLF bytes are digested
- **WHEN** a CRLF file has `### 1. Sign in (Must)` followed by `### 2. Wrong password (Should)` as its last heading
- **THEN** section 1's digest is the SHA-256 of the bytes from `### 1.` up to `### 2.`, CR bytes included, and section 2's runs to the end of the file

### Requirement: Upload refuses a set whose revision ends +dirty
When the front matter's `revision` is a string ending `+dirty`, `cr scenarios upload` MUST fail with exit code 1 and `<file> covers uncommitted changes (revision ends +dirty); commit them and write the set again`, before finding the change request or sending any request.

#### Scenario: Dirty diff set
- **WHEN** `qa-diff.md` has `source: diff` and `revision: "a1b2c3d+dirty"`
- **THEN** `goodflows cr scenarios upload qa-diff.md` exits 1 with a message naming `+dirty`, and the server stores no diff version

#### Scenario: Clean revision
- **WHEN** the same file has `revision: "a1b2c3d"`
- **THEN** the upload is sent

### Requirement: The front matter's source names the set
The CLI SHALL NOT send a source: the server files the upload under the intent or diff set named by the content's `source`. The CLI MUST report that `source` as read from the file. An intent set is written from an OpenSpec change before code, and a diff set is written from the branch's change.

#### Scenario: Intent set
- **WHEN** the user uploads `openspec/changes/add-login/qa-scenarios.md` with `source: intent`
- **THEN** the request body has only `content` and `sections`, and the event's `source` is `intent`

### Requirement: Upload checks the digest the server stored
The CLI SHALL compute the set digest as the lowercase hex SHA-256 of the file's bytes. When the server's `data.digest` is a string different from it, the command MUST fail with exit code 1 and `the server stored digest <stored> for <file>, whose SHA-256 is <digest>`.

#### Scenario: Server changed the bytes
- **WHEN** the server answers `201` with a `digest` that is not the file's SHA-256
- **THEN** the command exits 1 naming both digests

### Requirement: Upload reports the version or that it was already stored
On success `cr scenarios upload` SHALL exit 0 and emit a `scenario_set` event with `number`, `file`, `source`, `digest`, `scenarios` (the IDs), `version`, `duplicate` (true when the server answered `200`, not `201`) and `stored` (the server's data). Text output SHALL be `Uploaded <source> set of change request #<n> as version <v> (digest <digest>)`, starting `Already stored:` in place of `Uploaded` for a duplicate.

#### Scenario: First upload
- **WHEN** the server answers `201` with version 1
- **THEN** the text reads `Uploaded intent set of change request #1 as version 1 (digest <sha256 of the file>)`

#### Scenario: Same bytes again
- **WHEN** the same file is uploaded again and the server answers `200` with version 1
- **THEN** `--json` shows `version` 1 and `duplicate` true

### Requirement: List shows the versions of both sets
`goodflows cr scenarios list [--source intent|diff]` SHALL read `GET …/change-requests/{number}/scenario-sets` and print, for each source (both, intent first, unless `--source` names one), `<source> set of change request #<n>:` then one line per version as the server orders them: `  v<version>  <first 12 of digest>  <inserted_at>` and marks. It SHALL print `<source> set of change request #<n>: no versions` for an empty set.

#### Scenario: Only the diff set
- **WHEN** the user runs `goodflows cr scenarios list --source diff`
- **THEN** no intent line is printed

### Requirement: List marks current, approved, accepted and baseline versions
After a version line, the CLI SHALL print two spaces and a comma-separated list of marks: `current` for the current version, `approved` for an approved intent version, `accepted` for an accepted diff version, and `baseline` for the diff baseline. The CLI MUST compute the diff baseline as the newest accepted diff version, else 1, and add it to the diff set as `baseline_version` in the `scenario_sets` event.

#### Scenario: Baseline is the newest accepted version
- **WHEN** diff versions 3 (current), 2 (accepted) and 1 have digests `333…`, `222…` and `111…`
- **THEN** the output is `diff set of change request #7:`, `  v3  333333333333  2026-11-21T10:00:00Z  current`, `  v2  222222222222  2026-11-21T10:00:00Z  accepted, baseline`, `  v1  111111111111  2026-11-21T10:00:00Z`

#### Scenario: Nothing accepted
- **WHEN** no diff version is accepted
- **THEN** v1 is marked `baseline`

### Requirement: Approve and reject decide one intent set digest
`goodflows cr scenarios approve [FILE | --digest <sha256>]` and `reject [FILE | --digest <sha256>] --note <why>` SHALL decide one digest: `--digest` as given, else the SHA-256 of FILE's raw bytes, else the current intent version's digest from `GET …/scenario-sets`. FILE and `--digest` MUST conflict (exit 2). FILE is not parsed as a set.

#### Scenario: Approve a file
- **WHEN** the user runs `goodflows cr scenarios approve qa.md -n 7`
- **THEN** the CLI posts `{"digest": "<sha256 of qa.md's bytes>"}` to `/api/v1/projects/goodcases/change-requests/7/scenario-sets/intent/approvals`

#### Scenario: Both given
- **WHEN** the user passes FILE and `--digest`
- **THEN** the command exits 2 and sends nothing

### Requirement: A given digest is 64 lowercase hex characters
`--digest` MUST be exactly 64 characters of `0-9` and `a-f`, else the command fails with exit code 1 and `--digest must be 64 lowercase hex characters`, sending no decision.

#### Scenario: Uppercase digest
- **WHEN** `--digest` is 64 hex characters with uppercase `A`
- **THEN** the command exits 1 with `--digest must be 64 lowercase hex characters`

### Requirement: Only the intent set is approved or rejected
Without FILE or `--digest`, when the change request has no current intent version the command MUST fail with exit code 1: with `change request #<n> has only a diff set; a diff set is accepted by approving its diff_set_weakened items: goodflows cr review` when a diff set exists, else `change request #<n> has no intent scenario set; upload one first`. Decisions always go to the `intent` routes.

#### Scenario: Diff-only change request
- **WHEN** change request 7 has diff versions and no intent version, and the user runs `goodflows cr scenarios approve`
- **THEN** it exits 1 and the error names `has only a diff set` and `goodflows cr review`

### Requirement: Reject sends a required note
`cr scenarios reject` MUST require `--note` (exit 2 without it) and SHALL post `{digest, note}` to `…/scenario-sets/intent/rejections`. `approve` SHALL post `{digest}` to `…/scenario-sets/intent/approvals`.

#### Scenario: Reject a file
- **WHEN** the user runs `goodflows cr scenarios reject qa.md --note Two-musts-is-too-strict`
- **THEN** the body is `{"digest": "<sha256 of qa.md>", "note": "Two-musts-is-too-strict"}`

#### Scenario: No note
- **WHEN** the user runs `goodflows cr scenarios reject` without `--note`
- **THEN** the command exits 2

### Requirement: Decisions print the digest and whether they repeat one
A decision SHALL emit a `scenario_decision` event with `number`, `digest`, `decision` (the server's data) and `repeated` (true when the server answered `200`). Text output SHALL be `Approved intent set digest <digest> of change request #<n>`, `Rejected` when the decision is `reject`, with ` (already recorded)` appended when repeated, then `  note: <note>` when the decision has a note.

#### Scenario: Rejected with a note
- **WHEN** the server answers `201` with `{"decision": "reject", "note": "Missing export"}`
- **THEN** the output is `Rejected intent set digest <digest> of change request #7` then `  note: Missing export`

### Requirement: Diff compares set versions by scenario ID
`goodflows cr scenarios diff [--source intent|diff] [--from N] [--to N]` SHALL read `GET …/change-requests/{number}/scenario-sets/{source}/compare`, `source` defaulting to `intent`, sending `from` and `to` as query parameters only when known. For the intent set without `--from` it MUST send no `from`, so the server compares against the newest approved version.

#### Scenario: Two given versions
- **WHEN** the user runs `goodflows cr scenarios diff --from 1 --to 3`
- **THEN** the request is `GET …/scenario-sets/intent/compare?from=1&to=3`

### Requirement: Diff of a diff set starts from its baseline
For `--source diff` without `--from`, the CLI SHALL read the verdict and send `from` as `scenarios.diff.baseline_version` when it is lower than `--to`, or than the verdict's diff `version` when `--to` is absent. Otherwise it MUST send no `from`. `--from` MUST win over the baseline.

#### Scenario: Baseline below current
- **WHEN** the verdict's diff set is version 3 with baseline 2 and the user runs `goodflows cr scenarios diff --source diff`
- **THEN** the request ends `/diff/compare?from=2` and the output starts `diff set of change request #7: version 3 against 2 (the baseline)`

#### Scenario: Given from
- **WHEN** the user runs `goodflows cr scenarios diff --source diff --from 1`
- **THEN** the request ends `/diff/compare?from=1` and the verdict's baseline is not used

### Requirement: Diff prints each change with a mark
Diff SHALL emit a `scenario_changes` event with `number`, `source`, `comparison` and `from_baseline`. Text output SHALL start `<source> set of change request #<n>: version <to> against <from>`, plus ` (the baseline)` when `from` came from the baseline, then `  no changes` or one line per change: a mark (`!` weakening, `+` added, `-` removed, `~` other), the id and change, `  <from> -> <to>` priorities when they differ, and `  (weakening)`.

#### Scenario: Priority raised
- **WHEN** scenario 2 changed from `should` to `must` between intent versions 1 and 2
- **THEN** the output has `version 2 against 1` and `  ~ 2  changed  should -> must`, and scenario 1 is not listed

#### Scenario: Must demoted
- **WHEN** the comparison has `{id: "3", change: "demoted", weakening: true, from_priority: "must", to_priority: "should"}`
- **THEN** the line is `  ! 3  demoted  must -> should  (weakening)`
