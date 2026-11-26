# review-policy Specification

## Purpose

Defines how `goodflows policy get`, `set` and `history` read a project's `review-policy/1` document as TOML, write a new version after the current one, skip identical writes, and list versions.

## Requirements

### Requirement: policy get prints a policy version as TOML
`goodflows policy get` SHALL read `GET review-policy`, or `GET review-policy/versions/<N>` with `--version N`, and print the version's document as written in TOML. With `--effective` it SHALL print the `effective` document, with every default filled in, instead. Reading SHALL follow the shared rules the change-requests capability specifies.

#### Scenario: Stored version
- **WHEN** a user runs `goodflows policy get --version 2`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/review-policy/versions/2` and prints that version's `policy` document

#### Scenario: Effective document
- **WHEN** the current version's `policy` sets only `size.max_changed_files` and the user runs `goodflows policy get --effective`
- **THEN** the printed TOML is the response's `effective` document, which also holds `size.max_changed_lines`

### Requirement: Policy text output starts with comment lines
The text output of `policy get` and `policy set` SHALL start with `# review-policy version <v> (previous <p>), written <inserted_at> by <user_id>`, then `# note: <note>` when the version has a note, then `# active settings: <names>` when the server lists them, then the document. Each header line SHALL start with `#`, so the whole output parses as TOML.

#### Scenario: Output parses as TOML
- **WHEN** `policy get` prints version 2 with previous version 1, note `Require ci`, `evidence.required_workflows` `["ci"]` and one `paths` entry with glob `app/priv/**`
- **THEN** the output starts `# review-policy version 2 (previous 1)` and parses as TOML with `evidence.required_workflows[0]` `ci` and `paths[0].glob` `app/priv/**`

### Requirement: policy get emits one review_policy event
With `--json`, `policy get` SHALL print one event `{"type": "review_policy", "policy": <the response's data>, "effective": <whether --effective was given>}`.

#### Scenario: JSON output
- **WHEN** a user runs `goodflows policy get --json` and the current version is 1
- **THEN** the event has `type` `review_policy`, `policy.version` `1` and `effective` false

### Requirement: policy set reads the file as TOML
`goodflows policy set <FILE>` SHALL parse the file as TOML before sending anything, convert it to JSON, and add `format` `review-policy/1` when the file has no `format` key. A file that is not valid TOML SHALL fail with `<file> is not valid TOML: <error>` and exit 1, with no request sent.

#### Scenario: Format filled in
- **WHEN** `policy.toml` holds only `[evidence]` with `required_workflows = ["ci"]` and one `[[paths]]` entry
- **THEN** the `PUT` body's `policy` is `{"format": "review-policy/1", "evidence": {"required_workflows": ["ci"]}, "paths": [{"glob": "app/priv/**", "outcome": "needs_review"}]}`

#### Scenario: Invalid TOML
- **WHEN** `policy.toml` holds `max_changed_lines = ` with no value
- **THEN** `policy set policy.toml` exits 1 with a message starting `policy.toml is not valid TOML:` and sends no request

### Requirement: policy set writes a version after the current one
`policy set` SHALL send `PUT review-policy` with `previous_version`, `policy`, and `note` when `--note` is given. Without `--previous-version`, `previous_version` SHALL be the version a `GET review-policy` returned just before. With `--previous-version N` it SHALL be N, and goodflows SHALL NOT read the current version first.

#### Scenario: After the current version
- **WHEN** the current version is 3 and a user runs `goodflows policy set policy.toml --note "Require ci"`
- **THEN** goodflows sends `GET` then `PUT /api/v1/projects/<project>/review-policy` with `previous_version` `3` and `note` `Require ci`

#### Scenario: Named previous version
- **WHEN** a user runs `goodflows policy set policy.toml --previous-version 0` and the current version is 1
- **THEN** goodflows sends only the `PUT`, with `previous_version` `0`

### Requirement: policy set skips a write identical to the current version
Without `--previous-version`, when the current version's `policy` equals the file's document and `--note` is absent or equals the current note, `policy set` SHALL send no `PUT`, print `# Review policy version <v> was already written` and the version, and exit 0. A different note SHALL write a new version.

#### Scenario: Same document
- **WHEN** the current version 3 has the file's document and note `Require ci`, and a user runs `policy set policy.toml` or `policy set policy.toml --note "Require ci"`
- **THEN** goodflows sends only the `GET`, prints `version 3 was already written`, and exits 0

#### Scenario: Another note
- **WHEN** the same file is set with `--note Other`
- **THEN** goodflows sends a `PUT` with `previous_version` `3`

### Requirement: policy set reports whether it wrote
A `201` response SHALL print `# Wrote review policy version <v>` before the version. With `--json`, `policy set` SHALL print one event `{"type": "review_policy", "policy": ..., "written": <bool>, "effective": false}`, where `written` is true only for a `201`.

#### Scenario: New version
- **WHEN** the server answers the `PUT` with `201` and version 4
- **THEN** the `--json` event has `written` true and `policy.version` `4`

#### Scenario: Server replays a retried write
- **WHEN** the server answers the `PUT` with `200` and the current version
- **THEN** the event has `written` false and the text output says `was already written`

### Requirement: A stale previous version has its own message
When the `PUT` gets `409` with `errors.detail` `previous_version is not the current policy version`, `policy set` SHALL fail with exit 1 and `policy set failed: version <N> is no longer the current policy version, because another write landed first; read it with goodflows policy get, make the change again, and set it`.

#### Scenario: Stale write
- **WHEN** the current version is 1 and a user runs `policy set policy.toml --previous-version 0` with a changed document
- **THEN** goodflows exits 1 with `version 0 is no longer the current policy version, because another write landed first`

### Requirement: policy history lists versions newest first
`goodflows policy history` SHALL send `GET review-policy/versions` with `limit` (clap accepts 1 to 200) and `cursor` when given, and print one line per version, `v<version>  <inserted_at>  <user_id>  <note>`, then the next cursor when there is one. With `--json` it SHALL print `{"type": "review_policy_versions", "versions": [...], "next_cursor": ...}`. A response whose `data` is not a list SHALL fail.

#### Scenario: One version
- **WHEN** version 1 has note `Tiny-changes-only` and a user runs `goodflows policy history`
- **THEN** the output lists `v1` with `Tiny-changes-only`

#### Scenario: Limit out of range
- **WHEN** a user runs `goodflows policy history --limit 0` or `--limit 201`
- **THEN** goodflows exits 2 with a usage error and sends no request
