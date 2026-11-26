# Spec Delta

## ADDED Requirements

### Requirement: Names follow one rule
Refs: G1, AG1, D1
A goodflows name SHALL be valid only when it is nonempty after trimming whitespace, at most 64 UTF-8 bytes, and contains no control characters. Spaces, punctuation and non-ASCII text SHALL stay valid. Every name check in goodflows MUST use exactly this rule.

#### Scenario: Boundary lengths
- **WHEN** a name is 64 UTF-8 bytes, as 64 ASCII characters or as 32 two-byte characters
- **THEN** it is valid, and a name of 65 UTF-8 bytes is not

#### Scenario: Blank and control characters
- **WHEN** a name is empty, consists only of whitespace, or contains a tab, newline, carriage return, NUL, escape or C1 control character such as U+009B
- **THEN** it is not valid

#### Scenario: Ordinary punctuation
- **WHEN** a name contains spaces, commas, apostrophes or a leading hyphen
- **THEN** it is valid

### Requirement: Configured names are validated at load
Refs: G1, G2, G3, AG2, NG2, D2, D5, D6
Workflow names, environment profile names, job names, step ids and job `outputs` keys MUST follow the name rule. They SHALL be checked in root `[[jobs]]` and every named workflow and profile when configuration loads, whether or not they are selected and whether or not a job declares `reports`, before any job runs. A violation is a config error: `goodflows validate`, `goodflows list` and a run MUST all exit `2` and run no command. The error MUST name the kind of name and state the rule, and for a job, step or output key MUST identify which one, with control characters escaped. A bad job name SHALL be reported before other errors on that job, and duplicate job names stay an error.

#### Scenario: Long job name in an unselected workflow
- **WHEN** a job that declares no `reports` has a name of 65 UTF-8 bytes, in root `[[jobs]]` or in a named workflow that is not selected
- **THEN** `goodflows validate`, `goodflows list` and `goodflows run` each exit `2` with an error naming the job and stating the 64-byte rule, and no command runs

#### Scenario: Control character in a job name
- **WHEN** a job name contains an escape character or a tab, including in a job that declares no `reports`
- **THEN** validation exits `2` with an error naming the job with its control characters escaped, and the error output contains no raw control characters

#### Scenario: Whitespace-only job name
- **WHEN** a job name consists only of spaces
- **THEN** validation exits `2` with the job-name rule error and no command runs

### Requirement: Runtime output keys follow the name rule
Refs: G1, G2, D3, D6
Every key in the JSON object a command writes to `$GOODFLOWS_OUTPUT` MUST follow the name rule. If any key breaks it, the producing command or step SHALL fail with an invalid-job-outputs error that states the rule, its job SHALL publish no outputs, and its dependents SHALL be skipped.

#### Scenario: Output key too long
- **WHEN** a step writes `{"<65-byte key>": "x"}` to `$GOODFLOWS_OUTPUT`
- **THEN** the step fails, its job publishes no outputs, dependent jobs are skipped, and the run exits `1`

#### Scenario: Output key at the limit
- **WHEN** a step writes a 64-byte key and a dependent job references it as `needs.<job>.outputs['<key>']`
- **THEN** the output is accepted and the dependent receives its value

### Requirement: Saved report names follow the name rule
Refs: G1, G2, NG3, D7
Replay and upload of saved reports MUST apply the name rule to the workflow, environment and job names in a run manifest and in every request snapshot. A manifest with a name that breaks the rule MUST be refused before any request is built. A snapshot with such a name MUST NOT be sent and SHALL be counted as `invalid_envelope`.

#### Scenario: Whitespace-only name in a manifest
- **WHEN** a hand-edited run manifest names a job `"   "`
- **THEN** `goodflows reports upload` refuses the manifest and sends nothing

#### Scenario: Snapshot with an invalid environment name
- **WHEN** a saved snapshot's `environment` is 65 UTF-8 bytes
- **THEN** the snapshot is not sent and is counted as `invalid_envelope`
