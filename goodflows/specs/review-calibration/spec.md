# review-calibration Specification

## Purpose

Defines how `goodflows cr defect` records a defect found after a merge and how `goodflows policy calibration` reads and prints the calibration report of merges and defects.

## Requirements

### Requirement: cr defect records a defect by change request or commit
`goodflows cr defect` SHALL take exactly one of `--number N` (1 or more) or `--commit REV`, and send `POST defects` with `change_request` N, or `commit` set to the full SHA that this checkout resolves REV to. It SHALL add `ticket` with `--ticket` and `severity` with `--severity` (`low`, `medium`, `high` or `critical`). Writing follows the change-requests capability's rules.

#### Scenario: By commit
- **WHEN** a user runs `goodflows cr defect --commit HEAD --ticket GOO-400 --severity high`
- **THEN** goodflows sends `POST /api/v1/projects/<project>/defects` with `{"commit": <full SHA of HEAD>, "ticket": "GOO-400", "severity": "high"}`

#### Scenario: Neither or both
- **WHEN** a user runs `goodflows cr defect` with neither flag, or with both `--number 42` and `--commit HEAD`
- **THEN** goodflows exits 2 with a usage error and sends no request

### Requirement: cr defect prints the stored defect
`cr defect` SHALL print the response's `data` as indented `key: value` lines, and with `--json` one event `{"type": "defect", "defect": <data>}`.

#### Scenario: JSON output
- **WHEN** the server answers `201` with `{"data": {"id": "d1"}}` and the user passed `--json`
- **THEN** the event has `type` `defect` and `defect.id` `d1`

### Requirement: policy calibration reads the report for a window
`goodflows policy calibration` SHALL send `GET calibration` with `from` and `to` only when `--from` and `--to` are given, passing their values unchanged, so the server's defaults apply (to now, from 90 days before to). Reading follows the change-requests capability's rules.

#### Scenario: Default window
- **WHEN** a user runs `goodflows policy calibration`
- **THEN** goodflows sends `GET /api/v1/projects/<project>/calibration` with no query

#### Scenario: Since a date
- **WHEN** a user runs `goodflows policy calibration --from 2026-10-01T00:00:00Z`
- **THEN** the request's query is `from=2026-10-01T00:00:00Z` and has no `to`

### Requirement: policy calibration prints the report as given
With `--json`, `policy calibration` SHALL print one event `{"type": "calibration", "report": <data>}`, keeping every field the server returns, including `defects`, `defects_truncated` and `unattributed_defect_count`. Without `--json` it SHALL print the report as the summary and defect table the other requirements of this capability describe, not as indented `key: value` lines.

#### Scenario: JSON output
- **WHEN** the report's `lanes.auto.merged_count` is `20` and the user passed `--json`
- **THEN** the event's `report.lanes.auto.merged_count` is `20`

#### Scenario: JSON keeps the defect list
- **WHEN** the report has two `defects` entries, `defects_truncated` false and `unattributed_defect_count` `3`, and the user passed `--json`
- **THEN** the event's `report.defects` holds the same two entries in the same order, and `report.unattributed_defect_count` is `3`

### Requirement: The calibration text starts with the window and lanes
The text output SHALL start with `Calibration <from> to <to>`, then one line per lane in the order `auto`, `reviewed`, `bypass`: `  <lane>  <merged_count> merged  <defective_count> defective  <defect_count> defects  <rate>`. `<rate>` SHALL be `defect_rate` as a percentage rounded to a whole number, or `-` when it is null.

#### Scenario: Lane lines
- **WHEN** the window is `2026-08-24T00:00:00Z` to `2026-11-22T00:00:00Z`, `auto` has 20 merged, 3 defective, 5 defects and rate `0.15`, and `bypass` has 0 merged and rate null
- **THEN** the output starts `Calibration 2026-08-24T00:00:00Z to 2026-11-22T00:00:00Z`, the `auto` line ends `5 defects  15%`, and the `bypass` line ends `-`

#### Scenario: Rounding
- **WHEN** the `reviewed` lane's `defect_rate` is `0.3333333333333333`
- **THEN** its line ends `33%`

### Requirement: The calibration text summarises versions, rules and globs
After the lanes, the text SHALL print `Policy versions:` with `  v<version>  auto <m>/<d>  reviewed <m>/<d>  bypass <m>/<d>` per version (merged and defective counts), `Rules:` with `  <rule>  fired <fired_count>, with defects <fired_with_defects_count>` per rule, `Gaps: <numbers>` (`none` when empty), and `Globs:` with `  <glob>  <defect_count> defects` per glob, then `  unmatched  <unmatched_defect_count> defects`.

#### Scenario: Rule and glob lines
- **WHEN** `path_match` fired on 2 change requests, 1 of them defective, glob `app/priv/**` has 2 defects and `unmatched_defect_count` is 1
- **THEN** the output has `  path_match  fired 2, with defects 1`, `  app/priv/**  2 defects` and `  unmatched  1 defects`

#### Scenario: Gaps
- **WHEN** `gaps` is `[46, 51]`
- **THEN** the output has `Gaps: 46, 51`

### Requirement: The calibration text lists defects newest first
When the report has `defects`, the text SHALL print `Defects:` and one line per entry in the server's order: `  <recorded_at>  #<change_request>  <severity>  <ticket>  <lane>  v<policy_version>  <rules>  <globs>`, with `rules` and `globs` joined by `,`, and `-` for a null value or an empty list. An empty list SHALL print `Defects: none`.

#### Scenario: Defect line
- **WHEN** a defect recorded at `2026-11-03T09:00:00Z` for CR 43 has severity `high`, ticket `GOO-512`, lane `reviewed`, policy version 7, rules `["max_changed_lines", "path_match"]` and globs `["app/priv/repo/migrations/**"]`
- **THEN** the output has `  2026-11-03T09:00:00Z  #43  high  GOO-512  reviewed  v7  max_changed_lines,path_match  app/priv/repo/migrations/**`

#### Scenario: Gap defect read with a public key
- **WHEN** a defect has severity null, ticket null, rules `[]` and globs `[]`
- **THEN** its line shows `-` in the severity, ticket, rules and globs columns

### Requirement: The calibration text says when defects were cut or unattributed
When `defects_truncated` is true, the text SHALL print `Showing the <n> newest defects; more exist. Narrow the window with --from and --to.` after the defect lines, where `<n>` is the number of entries. When the report has `unattributed_defect_count`, it SHALL print `Unattributed defects recorded in the window: <count>`.

#### Scenario: Truncated list
- **WHEN** the report lists 500 defects and `defects_truncated` is true
- **THEN** the output has `Showing the 500 newest defects; more exist. Narrow the window with --from and --to.`

#### Scenario: Unattributed count
- **WHEN** `unattributed_defect_count` is `2`
- **THEN** the output has `Unattributed defects recorded in the window: 2`

### Requirement: The calibration text works with servers without the defect list
When the report has no `defects` key, the text SHALL print the window, lanes, versions, rules, gaps and globs, and no `Defects` line. When it has no `unattributed_defect_count`, it SHALL print no unattributed line.

#### Scenario: Older server
- **WHEN** the server's report has only `from`, `to`, `lanes`, `policy_versions`, `rules`, `gaps`, `globs` and `unmatched_defect_count`
- **THEN** `policy calibration` exits 0, prints the lane lines, and prints no line starting `Defects` or `Unattributed`

### Requirement: Server text in the calibration table is escaped
Rules, globs, tickets and other strings from the report SHALL be printed with control characters escaped, as every change request command's output does.

#### Scenario: Escape in a ticket
- **WHEN** a defect's ticket is `GOO-1\u001b[2J`
- **THEN** the printed line holds the escaped form and no raw ESC byte
