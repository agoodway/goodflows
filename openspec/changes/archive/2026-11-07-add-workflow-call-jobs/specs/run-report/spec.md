# Spec Delta

## MODIFIED Requirements

### Requirement: Text runs end with a run report
In text mode, `goodflows run` SHALL render each invocation's `summary` event as a run report after that invocation's job and reporting lines. A child report SHALL identify its parent invocation and caller job and appear before that call's completion. The root report SHALL print last, after every descendant report and reporting line, with nothing following it. Reports SHALL replace the previous summary count lines. JSON mode MUST NOT print the report.

#### Scenario: Report is the last output
- **WHEN** a workflow with reporting configured finishes in text mode
- **THEN** the run report appears after the reporting lines and nothing follows it

#### Scenario: JSON mode has no report
- **WHEN** the CLI runs with `--json`
- **THEN** every stdout line is a versioned JSON event and no report text is printed

#### Scenario: Nested reports
- **WHEN** a root workflow invokes a child workflow in text mode
- **THEN** the child's report identifies the caller job and parent identity, and the root report follows all child output as the final output

## ADDED Requirements

### Requirement: Reports distinguish workflow-call results
A call SHALL appear once in its caller's configured-job rows and counts, identify its target workflow, and link its child invocation when activated. Child jobs SHALL appear only in child reports and summaries, not be counted again as local parent jobs. Child failure details SHALL name the actual failed jobs. The parent call failure SHALL identify the child rather than inventing a shell command or exit code. A started call row's duration SHALL be the call result's integer `duration_ms`.

#### Scenario: Child test failure
- **WHEN** root job `integration` calls `suite`, whose command job `test` fails
- **THEN** the child report details `test`, the root report shows failed call `integration` targeting `suite`, and the root counts one failed local call rather than every child job

#### Scenario: Call skipped before activation
- **WHEN** a call is skipped because a prerequisite did not pass
- **THEN** its row shows the dependency reason, no duration, and no fabricated child identity

#### Scenario: Started call shows an integer duration
- **WHEN** a call starts, its child runs, and the call result records `duration_ms`
- **THEN** the caller's row shows that integer duration, including `0ms` when the recorded duration is zero

### Requirement: Root rerun hints preserve workflow-call context
The root failure report SHALL select failed or timed-out local call jobs and dependency-skipped local jobs using the existing rerun rules. The hint SHALL name the root workflow and preserve explicit config and profile arguments rather than replacing the command with a direct child invocation. Executing the hint SHALL therefore rerun each selected call's local prerequisite closure and its complete child workflow.

#### Scenario: Preparation is required before the child
- **WHEN** `prepare` precedes call `integration` by dependency and `publish` depends on `integration`, and the child fails
- **THEN** the root hint selects `integration` and `publish`, and executing it also reruns `prepare` before calling the child

#### Scenario: Unusual root and call names
- **WHEN** a root or call name requires shell quoting or contains a comma
- **THEN** existing quoting, leading-option handling, and full-workflow fallback rules apply without constructing a new nested-job selector syntax
