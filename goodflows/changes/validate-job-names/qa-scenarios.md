---
format: qa-scenarios/2
source: intent
change: "validate-job-names"
specs: "uncommitted"
scenarios:
  - id: 1
    name: "Name boundaries are applied the same way to every kind of name"
    priority: must
    covers:
      - {capability: "name-rule", requirement: "Names follow one rule", scenario: "Boundary lengths"}
      - {capability: "name-rule", requirement: "Names follow one rule", scenario: "Ordinary punctuation"}
  - id: 2
    name: "Long job name without reports is refused before anything runs"
    priority: must
    covers:
      - {capability: "name-rule", requirement: "Configured names are validated at load", scenario: "Long job name in an unselected workflow"}
  - id: 3
    name: "Blank and control-character job names are refused with escaped errors"
    priority: must
    covers:
      - {capability: "name-rule", requirement: "Configured names are validated at load", scenario: "Control character in a job name"}
      - {capability: "name-rule", requirement: "Configured names are validated at load", scenario: "Whitespace-only job name"}
      - {capability: "name-rule", requirement: "Names follow one rule", scenario: "Blank and control characters"}
  - id: 4
    name: "A runtime output key over 64 bytes fails the producer"
    priority: must
    covers:
      - {capability: "name-rule", requirement: "Runtime output keys follow the name rule", scenario: "Output key too long"}
      - {capability: "name-rule", requirement: "Runtime output keys follow the name rule", scenario: "Output key at the limit"}
  - id: 5
    name: "Saved reports with invalid names are not sent"
    priority: must
    covers:
      - {capability: "name-rule", requirement: "Saved report names follow the name rule", scenario: "Whitespace-only name in a manifest"}
      - {capability: "name-rule", requirement: "Saved report names follow the name rule", scenario: "Snapshot with an invalid environment name"}
  - id: 6
    name: "Escape sequences in failure output stay visible"
    priority: must
    covers:
      - {capability: "run-report", requirement: "Job-provided text cannot rewrite the terminal", scenario: "Escape sequence in a command or error"}
---

# QA: validate-job-names

## Scenarios

### 1. Name boundaries are applied the same way to every kind of name (Must)

Why: One rule means a name valid in one place is valid everywhere, and the server accepts it.

1. Write a `goodflows.toml` whose workflow, environment profile, job, step id and `outputs` key are each exactly 64 ASCII bytes, and a second job named with 32 × `é` (64 bytes). Add a job named `a b,c` and one named `-x`, and one named `it's`. Run `goodflows validate`.
   - Expect: exit `0`.
2. Lengthen each of the 64-byte names in turn to 65 bytes and run `goodflows validate`.
   - Expect: exit `2` every time, and the error names the kind of name (workflow, environment, job, step or output) and the 64-byte rule.

### 2. Long job name without reports is refused before anything runs (Must)

Why: A job name over 64 bytes makes the server reject the whole run history upload.

1. Write a config whose root `[[jobs]]` has a job with no `reports`, a 65-byte name and `command = "touch ran"`. Run `goodflows validate`, `goodflows list` and `goodflows run`.
   - Expect: each exits `2`, the error names the job and states the 64-byte rule, and no `ran` file exists.
2. Move that job into a named workflow `other`, keep a valid default workflow, and run `goodflows run` (default workflow), `goodflows validate` and `goodflows list`.
   - Expect: the same: exit `2`, the job is named, nothing runs.
3. Put two jobs with the same valid name in one workflow.
   - Expect: validation still fails with a duplicate job name error.

### 3. Blank and control-character job names are refused with escaped errors (Must)

Why: A job name must never carry escape sequences into the report or error output.

1. Configure a job with no `reports` named `"   "` and run `goodflows validate`.
   - Expect: exit `2` with the job-name rule error; no command runs.
2. Configure a job named `"a\u001b[2Jb"`, then one named `"a\tb"`, then `""`, and also `\n`, `\r`, `\u0000` and `\u009b` variants, and run `goodflows validate 2>err.txt` for each.
   - Expect: exit `2`, the error names the job with the control character escaped (for example `\u{1b}` or `\t`), and `err.txt` contains no raw control bytes other than newlines.
3. Configure a job with a bad name that also has another error, such as `timeout_secs = 0`.
   - Expect: the name error is reported for that job before the other error.

### 4. A runtime output key over 64 bytes fails the producer (Must)

Why: Keys are only known at runtime and must still follow the rule the server applies.

1. Configure job `produce` with a step that writes `{"<65 x k>": "x"}` to `$GOODFLOWS_OUTPUT`, and job `consume` that depends on it. Run `goodflows run --json`.
   - Expect: `produce` fails with an invalid job outputs error that states the rule, it publishes no outputs, `consume` is skipped, and the run exits `1`.
2. Change the key to exactly 64 bytes and have `consume` echo `${{ needs.produce.outputs['<64-byte key>'] }}`. Run again.
   - Expect: both jobs pass, the run exits `0`, and `consume` prints `x`.

### 5. Saved reports with invalid names are not sent (Must)

Why: Hand-edited spool files must not reach the server with names it refuses.

1. Run a reporting job so a spool is written, then edit the run manifest so a job name is `"   "`. Point the reporting URL at a stub server that logs requests and run `goodflows reports upload`.
   - Expect: the manifest is refused and the stub server receives no request.
2. Restore the manifest, then edit one request snapshot so its `environment` is 65 bytes. Run `goodflows reports upload --json`.
   - Expect: that snapshot is not sent and is counted as `invalid_envelope`; other valid snapshots are sent.

### 6. Escape sequences in failure output stay visible (Must)

Why: Job output must not clear or restyle the user's terminal.

1. Configure a job whose command is `printf 'x\033[2J'; exit 1` with an error containing a bare carriage return, and run `goodflows run --output=failures > out.txt`.
   - Expect: the report shows `\u{1b}[2J` and `\r` as visible text, and `out.txt` contains no raw control bytes other than newlines (tab allowed).
