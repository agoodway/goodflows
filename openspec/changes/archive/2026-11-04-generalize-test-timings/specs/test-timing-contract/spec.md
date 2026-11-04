# Spec Delta

## Purpose

Define the runner-neutral `test-timings/1` wire format that goodflows collects and the GoodFlows server stores, so that any test runner can publish per-test timings through one versioned contract.

## ADDED Requirements

### Requirement: The format names its contract version
A report SHALL declare the format `test-timings/1` both in the request envelope's `format` and in its `suite_started` record's `format`, and the two MUST agree. A document with any other format, including the retired `exunit-timings`, MUST be rejected. A later incompatible contract SHALL use a new `test-timings/<n>` name instead of changing version 1.

#### Scenario: Current format
- **WHEN** a report's envelope and `suite_started` both declare `test-timings/1`
- **THEN** its format is accepted

#### Scenario: Retired format
- **WHEN** a report declares `exunit-timings`
- **THEN** it is rejected as an unsupported format

#### Scenario: Envelope and record disagree
- **WHEN** the envelope declares `test-timings/1` and `suite_started` declares `test-timings/2`
- **THEN** the report is rejected

### Requirement: The envelope has a fixed shape
A request body SHALL have exactly the keys `format`, `workflow_run_id`, `workflow`, `environment`, `job`, `job_status`, `source` and `report`. `source` SHALL have exactly `commit`, `branch` and `dirty`. `report` SHALL have exactly `suite_id`, `report_id`, `partition`, `partition_count`, `truncated` and `records`. `job_status` MUST be one of `passed`, `failed`, `timed_out`, `cancelled`, `skipped` or `unknown`.

#### Scenario: Extra envelope key
- **WHEN** a body adds a `project` key
- **THEN** it is rejected

#### Scenario: Report identity agrees with records
- **WHEN** `report.suite_id`, `report.report_id`, `report.partition` or `report.partition_count` differs from the `suite_started` record's value
- **THEN** the report is rejected

### Requirement: Every record carries common keys
Every record SHALL be a JSON object with `event`, `suite_id`, `report_id` and `received_offset_us`, and MAY have `meta`. `suite_id` and `report_id` SHALL follow the identifier grammar (1-128 bytes of `[A-Za-z0-9._-]` starting with a letter or digit) and MUST be equal in every record of a report. `received_offset_us` is the producer's integer microseconds since the suite started.

#### Scenario: Mixed identities
- **WHEN** one record of a report has a different `report_id` from the others
- **THEN** the report is rejected

#### Scenario: Record uses the retired run_id key
- **WHEN** a record carries `run_id` instead of `suite_id`
- **THEN** the report is rejected for an unknown key

### Requirement: The event vocabulary is closed and ordered
Events SHALL be `suite_started`, `group_started`, `group_finished`, `test_started`, `test_finished`, `suite_finished`, `interrupted` and `max_failures_reached`. The first record MUST be `suite_started` and no other record may be. `suite_finished`, when present, MUST be the last record. Each event MUST carry all of its core keys, nullable ones as `null`, and no keys outside its core set, the common keys and `meta`. `interrupted` and `max_failures_reached` have no core keys beyond the common keys, and MAY have `meta`.

#### Scenario: Unknown event
- **WHEN** a record has `event: "module_started"`
- **THEN** the report is rejected

#### Scenario: Missing core key
- **WHEN** a `test_finished` record omits `duration_us`
- **THEN** the report is rejected, even though `duration_us` is nullable

#### Scenario: Interruption without finish
- **WHEN** a report ends with `interrupted` and has no `suite_finished`
- **THEN** it is accepted as an incomplete report

#### Scenario: Extra key on an interruption
- **WHEN** an `interrupted` record has a `reason` key
- **THEN** the report is rejected

### Requirement: suite_started describes the run
`suite_started` SHALL have `format`, `started_at` (an RFC 3339 UTC timestamp of at most 64 bytes), `runner` (an object with exactly `name`, in the identifier grammar, and `version`, a string of at most 128 bytes or null), `partition`, `partition_count`, `seed` (null or an integer whose magnitude is at most 2^53-1) and `filters` (null or a filter object).

#### Scenario: Jest run with a negative seed
- **WHEN** `suite_started` has `runner: {"name": "jest", "version": "30.0.0"}`, `seed: -1843` and `filters: null`
- **THEN** the record is accepted

#### Scenario: Runner name outside the grammar
- **WHEN** `runner.name` is `"jest runner"`
- **THEN** the report is rejected

### Requirement: Partition fields have a fixed grammar
`partition` SHALL be null or 1-64 bytes of ASCII letters, digits, `.`, `_` or `-`. `partition_count` SHALL be null or an integer from 1 to 2^53-1.

#### Scenario: Partition with a space
- **WHEN** `partition` is `"a b"`
- **THEN** the report is rejected

#### Scenario: Zero partition count
- **WHEN** `partition_count` is `0`
- **THEN** the report is rejected

### Requirement: Groups are the runner's scheduling unit
`group_started` SHALL have `group` (text), `file` (text or null), `execution_id` and `started_offset_us`. `group_finished` SHALL repeat those keys and add `duration_us`, `outcome` and `counts`. A group is what the runner schedules as a unit, such as an ExUnit module or a Jest test file. Text is a string of 1 to 4,096 bytes.

#### Scenario: Group that failed to load
- **WHEN** a Jest test file throws while loading and runs no tests
- **THEN** its `group_finished` is accepted with `outcome: "errored"` and empty `counts`

#### Scenario: Group name too long
- **WHEN** `group` is 4,097 bytes
- **THEN** the report is rejected

### Requirement: Tests are identified by group, path and name
`test_started` SHALL have `group` (text), `path` (an array of at most 32 text strings, outermost first), `name` (text), `file` (text or null), `line` (null or an integer of at least 1), `execution_id` and `started_offset_us`. `test_finished` SHALL repeat those keys and add `duration_us` and `outcome`.

#### Scenario: Nested describe blocks
- **WHEN** a Jest test `adds` sits inside `describe("math")` and `describe("sum")`
- **THEN** its `path` is `["math", "sum"]` and its `name` is `"adds"`

#### Scenario: Test outside any describe
- **WHEN** an ExUnit test has no `describe`
- **THEN** its `path` is `[]`

#### Scenario: Path too deep
- **WHEN** `path` has 33 entries
- **THEN** the report is rejected

### Requirement: Finished events pair with started events
`execution_id` SHALL be an integer from 1 to 2^53-1, unique among a report's `group_started` events and separately among its `test_started` events. Each `group_finished` or `test_finished` MUST follow an unfinished started event of its kind with the same `execution_id`, and MUST repeat that event's core keys with identical values. `meta` may differ.

#### Scenario: Finish without start
- **WHEN** a `test_finished` names an `execution_id` with no earlier `test_started`
- **THEN** the report is rejected

#### Scenario: Finish changes identity
- **WHEN** a `test_finished` repeats an `execution_id` but changes `name`
- **THEN** the report is rejected

#### Scenario: Finished twice
- **WHEN** two `test_finished` records share one `execution_id`
- **THEN** the report is rejected

#### Scenario: Started but never finished
- **WHEN** a truncated report ends after a `test_started`
- **THEN** the report is accepted and that test counts as unfinished

#### Scenario: Started twice
- **WHEN** two `test_started` records share one `execution_id`
- **THEN** the report is rejected

### Requirement: Outcomes and counts are runner-neutral
`outcome` SHALL be `passed`, `failed`, `skipped` or `errored` for tests and groups alike. `counts` SHALL be an object whose keys are a subset of those outcomes and whose values are nonnegative integers. A runner's finer distinctions, such as excluded, pending or todo, SHALL be carried in `meta`, not in `outcome`.

#### Scenario: Runner-specific outcome
- **WHEN** a `test_finished` has `outcome: "todo"`
- **THEN** the report is rejected

#### Scenario: Unknown count key
- **WHEN** `counts` has a key `excluded`
- **THEN** the report is rejected

### Requirement: Durations and offsets are nonnegative integer microseconds
`received_offset_us`, `started_offset_us` and every non-null `duration_us` SHALL be integers from 0 to 2^53-1. `duration_us` MAY be null when the runner does not report one, for example for a skipped test. Durations SHALL be stored as supplied and MUST NOT be checked against offsets.

#### Scenario: Millisecond runner
- **WHEN** a runner reports 12 milliseconds and the producer sends `duration_us: 12000`
- **THEN** the value is accepted unchanged

#### Scenario: Negative duration
- **WHEN** `duration_us` is `-5`
- **THEN** the report is rejected

### Requirement: suite_finished summarizes the run
`suite_finished` SHALL have `aborted` (boolean), `unfinished_tests` and `unfinished_groups` (nonnegative integers), `counts` and `duration_us`. `unfinished_tests` and `unfinished_groups` MUST equal the number of started events of each kind in the report that have no matching finished event.

#### Scenario: Unfinished count disagrees
- **WHEN** one `test_started` has no `test_finished` and `suite_finished` reports `unfinished_tests: 0`
- **THEN** the report is rejected

### Requirement: Runner metadata is flat and bounded
Any record MAY carry `meta`, an object of at most 32 keys. Each key SHALL follow the identifier grammar. Each value SHALL be null, a boolean, an integer whose magnitude is at most 2^53-1, or a string of at most 256 bytes. Arrays, objects, fractional numbers and longer strings MUST be rejected. Producers MUST NOT put logs, failure messages, assertion values or stack traces in `meta`.

#### Scenario: ExUnit runner details
- **WHEN** `suite_started.meta` is `{"elixir_version": "1.20.0", "otp_release": "28", "max_cases": 16}`
- **THEN** the record is accepted

#### Scenario: Structured meta value
- **WHEN** a `meta` value is `["a", "b"]` or `{"x": 1}`
- **THEN** the report is rejected

#### Scenario: Too many meta keys
- **WHEN** `meta` has 33 keys
- **THEN** the report is rejected

#### Scenario: Long meta string
- **WHEN** a `meta` string is 257 bytes
- **THEN** the report is rejected

### Requirement: Suite filters are boolean identifier tags
A non-null `filters` SHALL be an object with exactly `include` and `exclude`, each an array of at most 256 entries. Each entry SHALL have exactly `tag`, in the identifier grammar, and `value`, a boolean. Entries are case-sensitive and SHALL be kept in the supplied order, including a repeated tag.

#### Scenario: Runner without tags
- **WHEN** a Jest report sends `filters: null`
- **THEN** the record is accepted

#### Scenario: Tagged ExUnit run
- **WHEN** `filters` is `{"include": [{"tag": "slow", "value": true}], "exclude": []}`
- **THEN** the record is accepted with the entries unchanged

#### Scenario: Missing list
- **WHEN** `filters` is `{"include": []}`
- **THEN** the report is rejected

#### Scenario: Filters key omitted
- **WHEN** `suite_started` has no `filters` key
- **THEN** the report is rejected for a missing core key

#### Scenario: Repeated tags keep their order
- **WHEN** `filters.exclude` lists `{"tag": "slow", "value": true}` and then `{"tag": "slow", "value": false}`
- **THEN** the report is accepted with those two entries in that order

#### Scenario: Case-sensitive tags
- **WHEN** `filters.include` lists `{"tag": "Chrome", "value": true}` and `{"tag": "chrome", "value": true}`
- **THEN** the report is accepted with both entries unchanged

#### Scenario: Tag outside the identifier grammar
- **WHEN** a filter `tag` is `slow test`, `ok?`, an empty string, or longer than 128 bytes
- **THEN** the report is rejected

#### Scenario: Non-boolean filter value
- **WHEN** a filter entry has `tag` `chrome` and `value` `yes`
- **THEN** the report is rejected

#### Scenario: Filter list over the cap
- **WHEN** `filters.include` has 257 entries
- **THEN** the report is rejected

#### Scenario: Filters is not an object
- **WHEN** `filters` is an array or a string
- **THEN** the report is rejected

#### Scenario: Unknown key in filters
- **WHEN** `filters` is `{"include": [], "exclude": [], "x": 1}`
- **THEN** the report is rejected

#### Scenario: Unknown key in a filter entry
- **WHEN** a filter entry is `{"tag": "slow", "value": true, "note": "x"}`
- **THEN** the report is rejected

### Requirement: Documents are bounded and strictly encoded
A request body SHALL be at most 32 MiB and hold at most 100,000 records, with objects and arrays nested at most 16 levels. It MUST be valid UTF-8 JSON without duplicate object keys, fractional or exponent numbers, integers beyond the magnitude 2^53-1, or U+0000 in any string or key.

#### Scenario: Duplicate key
- **WHEN** a record has two `name` keys
- **THEN** the report is rejected instead of keeping either value

#### Scenario: Fractional number
- **WHEN** `duration_us` is `12.5`
- **THEN** the report is rejected

### Requirement: Completion is derived from core fields
A report SHALL be complete only when `truncated` is false, its last record is `suite_finished` with `aborted` false and `unfinished_tests` and `unfinished_groups` both zero, and it has no `interrupted` or `max_failures_reached` record. Every other accepted report SHALL be incomplete. Failed or errored tests and groups MUST NOT by themselves make a report incomplete.

#### Scenario: Failing but complete
- **WHEN** a report finishes normally with two failed tests
- **THEN** it is complete

#### Scenario: Bail
- **WHEN** a report contains `max_failures_reached` and ends with `suite_finished`
- **THEN** it is incomplete
