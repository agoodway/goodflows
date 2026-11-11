# Spec Delta

## MODIFIED Requirements

### Requirement: Commands and expressions use consistent environments
Job commands and job conditions SHALL see caller, resolved settings, and job env values. Step commands and step conditions SHALL additionally see that step's env overrides. Shell and expression conditions SHALL agree on ordinary variable values. Job output expressions SHALL see the job environment without step-only overrides. Runner-owned variable injection and scrubbing SHALL retain existing behavior, except that a `GOODFLOWS_OUTPUT` inherited from the caller SHALL NOT be visible to expressions. CI context for an uploaded run record SHALL be read only from the caller environment; settings and job env values MUST NOT supply or change it.

#### Scenario: Conditions observe profile values
- **WHEN** a selected profile sets `RUN_CHECKS=1`
- **THEN** a job expression condition referencing `env.RUN_CHECKS` and a shell condition reading `RUN_CHECKS` both observe `1`, as do their commands

#### Scenario: Step condition and output expression
- **WHEN** settings set `VALUE=settings` and a step sets `VALUE=step`
- **THEN** that step's shell and expression conditions and command see `step`, while a job output expression referencing `env.VALUE` sees `settings`

#### Scenario: Partition identity stays authoritative
- **WHEN** partitioned and unpartitioned jobs run with settings env defaults
- **THEN** partitioned commands and expressions still receive their runner-assigned identity and unpartitioned commands and expressions still have no partition identity

#### Scenario: Profile identity stays authoritative
- **WHEN** the caller exports `GOODFLOWS_ENVIRONMENT=outer` and jobs run with settings env defaults
- **THEN** job and step commands and conditions and job output expressions see the selected profile name when a profile is selected, and no `GOODFLOWS_ENVIRONMENT` when none is selected

#### Scenario: Inherited output path is hidden from expressions
- **WHEN** the caller exports `GOODFLOWS_OUTPUT` and no job or step env sets it
- **THEN** expression conditions and job output expressions see `env.GOODFLOWS_OUTPUT` as null, while commands still receive their own output file

#### Scenario: Settings do not supply the publisher endpoint
- **WHEN** settings define the configured reporting `url_env` variable with a value different from the caller's endpoint, or the caller has no endpoint
- **THEN** commands receive the configured settings value while the publisher resolves its endpoint only from the caller environment, leaving it unavailable when absent there

#### Scenario: Settings do not supply CI context
- **WHEN** run-record upload is enabled, the caller has no `GITHUB_ACTIONS`, and settings env sets `GITHUB_ACTIONS=true` and `GITHUB_RUN_ID=1`
- **THEN** commands receive the settings values and the uploaded record has no `ci`
