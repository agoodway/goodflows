# run-report Specification

## Requirements

### Requirement: Job-provided text cannot rewrite the terminal
In text mode, the report and the live failure and step error lines SHALL print control characters other than tab in commands, errors and skip reasons as visible escapes (for example `\u{1b}` or `\r`), and SHALL write no raw control characters other than newlines from that text. Job names cannot carry control characters into the report because they follow the goodflows name rule (see the `name-rule` capability).

#### Scenario: Escape sequence in a command or error
- **WHEN** a failed job's command contains `ESC [2J` and its error a bare carriage return
- **THEN** the report shows them as `\u{1b}[2J` and `\r` and writes no raw control characters other than newlines
