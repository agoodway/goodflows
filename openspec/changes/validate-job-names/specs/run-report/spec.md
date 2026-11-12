# Spec Delta

## MODIFIED Requirements

### Requirement: Job-provided text cannot rewrite the terminal
In text mode, the report and the live failure and step error lines SHALL print control characters other than tab in commands, errors and skip reasons as visible escapes (for example `\u{1b}` or `\r`). Job names MUST follow the goodflows name rule (see the `name-rule` capability) whether or not the job declares `reports` or is selected, so a job name can never carry control characters into the report.

#### Scenario: Escape sequence in a command or error
- **WHEN** a failed job's command contains `ESC [2J` and its error a bare carriage return
- **THEN** the report shows them as `\u{1b}[2J` and `\r` and writes no raw control characters other than newlines

#### Scenario: Control character in a job name
- **WHEN** a job name contains an escape character or a tab, including in a job that declares no `reports`
- **THEN** validation fails with an error naming the job, with its control characters escaped, while names with spaces or commas stay valid

#### Scenario: Job name longer than 64 bytes
- **WHEN** a job that declares no `reports` has a name of 65 UTF-8 bytes, in root `[[jobs]]` or in a named workflow that is not selected
- **THEN** validation fails with `job name must be nonempty, at most 64 UTF-8 bytes, and contain no control characters`, the error names the job, and no jobs execute

#### Scenario: Whitespace-only job name
- **WHEN** a job name consists only of spaces
- **THEN** validation fails with the job-name rule error and no jobs execute
