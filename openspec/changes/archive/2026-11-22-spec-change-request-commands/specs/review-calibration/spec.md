# Spec Delta

## ADDED Requirements

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
`policy calibration` SHALL print the response's `data` as indented `key: value` lines, and with `--json` one event `{"type": "calibration", "report": <data>}`, keeping every field the server returns.

#### Scenario: JSON output
- **WHEN** the report's `lanes.auto.merged_count` is `20` and the user passed `--json`
- **THEN** the event's `report.lanes.auto.merged_count` is `20`
