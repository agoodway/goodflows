# Spec Delta

## Purpose

Defines the one name rule that goodflows applies to every name it accepts: names in configuration, output keys that steps write at runtime, and names read back from saved report files. Server endpoints can then rely on every name goodflows sends.

## ADDED Requirements

### Requirement: Names follow one rule
A goodflows name SHALL be valid only when it is nonempty after trimming whitespace, at most 64 UTF-8 bytes, and contains no control characters. Spaces, punctuation and non-ASCII text SHALL stay valid. Every name check in goodflows MUST use exactly this rule.

#### Scenario: Boundary lengths
- **WHEN** a name is 64 UTF-8 bytes, as 64 ASCII characters or as 32 two-byte characters
- **THEN** it is valid, and a name of 65 UTF-8 bytes is not

#### Scenario: Blank and control characters
- **WHEN** a name is empty, consists only of whitespace, or contains a tab, newline, escape or other control character
- **THEN** it is not valid

#### Scenario: Ordinary punctuation
- **WHEN** a name contains spaces, commas, apostrophes or a leading hyphen
- **THEN** it is valid

### Requirement: Configured names are validated at load
Workflow names, environment profile names, job names, step ids and job `outputs` keys MUST follow the name rule. They SHALL be checked for every workflow and profile when configuration loads, whether or not they are selected, and before any job runs. The error MUST name the kind of name and state the rule. When the name is a job, step or output key, it MUST also identify which one, with control characters escaped.

#### Scenario: Invalid name in an unselected workflow
- **WHEN** a workflow that is not selected has a job, step id or output key of 65 UTF-8 bytes
- **THEN** validation fails, `goodflows validate`, `goodflows list` and runs all report it, and no jobs execute

#### Scenario: Consistent error text
- **WHEN** a workflow, environment, job, step or output name breaks the rule
- **THEN** the error reads `<kind> name must be nonempty, at most 64 UTF-8 bytes, and contain no control characters`

### Requirement: Runtime output keys follow the name rule
Every key in the JSON object a command writes to `$GOODFLOWS_OUTPUT` MUST follow the name rule. If any key breaks it, the producing command or step SHALL fail with an `invalid job outputs` error that states the rule, publish no outputs, and skip its dependents.

#### Scenario: Output key too long
- **WHEN** a step writes `{"<65-byte key>": "x"}` to `$GOODFLOWS_OUTPUT`
- **THEN** the step fails with `invalid job outputs`, its job publishes no outputs, and dependent jobs are skipped

#### Scenario: Output key at the limit
- **WHEN** a step writes a 64-byte key
- **THEN** the output is accepted and can be referenced by expressions

### Requirement: Saved report names follow the name rule
Replay and upload of saved reports MUST apply the name rule to the workflow, environment and job names in a run manifest and in every request snapshot. A manifest with a name that breaks the rule MUST be refused before any request is built. A snapshot with such a name MUST NOT be sent and SHALL be counted as `invalid_envelope`.

#### Scenario: Whitespace-only name in a manifest
- **WHEN** a hand-edited run manifest names a job `"   "`
- **THEN** `goodflows reports upload` refuses the manifest and sends nothing

#### Scenario: Snapshot with an invalid environment name
- **WHEN** a saved snapshot's `environment` is 65 UTF-8 bytes
- **THEN** the snapshot is not sent and is reported as `invalid_envelope`
