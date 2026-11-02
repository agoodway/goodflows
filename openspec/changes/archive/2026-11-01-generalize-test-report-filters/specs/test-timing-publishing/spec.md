# Spec Delta

## ADDED Requirements

### Requirement: Suite filter tags are boolean identifiers
Collection SHALL accept each `suite_started` filter entry whose `tag` is 1 to 128 bytes matching `[A-Za-z0-9][A-Za-z0-9._-]*` and whose `value` is a boolean. Include and exclude entries MUST be frozen in the supplied order, including a repeated tag. Tags are case-sensitive and MUST be frozen without normalization. When `filters` is present it MUST be an object whose `include` and `exclude` are both arrays, each holding at most 256 entries. A tag outside that grammar, a value that is not a boolean, a `filters` that is not such an object (including one missing `include` or `exclude`), or a list over 256 entries MUST reject that report with the structural code `unsupported_filter` before a snapshot is frozen, and the raw file MUST remain intact.

#### Scenario: Boolean identifier tags
- **WHEN** a report's `filters.include` is `[{"tag": "slow", "value": true}]` and `filters.exclude` is `[{"tag": "chrome", "value": true}]`
- **THEN** the report is frozen with both entries unchanged

#### Scenario: Repeated tags keep their order
- **WHEN** `filters.exclude` lists `{"tag": "slow", "value": true}` and then `{"tag": "slow", "value": false}`
- **THEN** the frozen array has those two entries in that order

#### Scenario: Tag outside the identifier grammar
- **WHEN** a filter `tag` is `slow test`, `ok?`, `slow` followed by a newline, `""`, or longer than 128 bytes
- **THEN** the report is rejected with `unsupported_filter`, no snapshot is written and the raw file is untouched

#### Scenario: Non-boolean filter value
- **WHEN** a filter entry has `tag` `chrome` and `value` `yes`
- **THEN** the report is rejected with `unsupported_filter`, no snapshot is written and the raw file is untouched

#### Scenario: Filter list over the cap
- **WHEN** `filters.include` has 257 entries
- **THEN** the report is rejected with `unsupported_filter`, no snapshot is written and the raw file is untouched

#### Scenario: Case-sensitive tags
- **WHEN** `filters.include` lists `{"tag": "Chrome", "value": true}` and `{"tag": "chrome", "value": true}`
- **THEN** the report is frozen with both entries unchanged, as distinct tags

#### Scenario: Missing or non-array filter list
- **WHEN** `filters` is present but has no `exclude` key, or its `include` is `null`, an object or a string
- **THEN** the report is rejected with `unsupported_filter`, no snapshot is written and the raw file is untouched

#### Scenario: Non-object filters
- **WHEN** `filters` is present and is `null`, an array or a string
- **THEN** the report is rejected with `unsupported_filter`, no snapshot is written and the raw file is untouched
