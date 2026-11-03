# Spec Delta

## Purpose

Lets workflows declare reusable command environment defaults and override individual variables through named workflows, machine profiles, jobs, and steps.

## ADDED Requirements

### Requirement: Settings accept environment maps
Shared settings, named workflow settings, and named environment profile settings SHALL accept an optional `env` map of string keys to string values. Omitted or empty maps SHALL contribute no overrides. Empty string values SHALL explicitly set a variable to an empty string. Values SHALL be literal, without interpolation or variable removal syntax.

#### Scenario: Every settings scope accepts env
- **WHEN** a configuration declares `[settings.env]`, `[workflows.ci.settings.env]`, and `[environments.local.settings.env]` with string values
- **THEN** it parses and validates successfully

#### Scenario: Non-string value
- **WHEN** any settings env map assigns an integer, boolean, array, or table as a variable value
- **THEN** configuration parsing fails before commands execute

#### Scenario: Existing configuration
- **WHEN** a valid existing configuration omits all settings env maps
- **THEN** its command environments and execution behavior remain unchanged

### Requirement: Environment inheritance merges individual keys
Effective command environments SHALL merge by key in increasing precedence: caller environment, shared settings, selected workflow settings, explicitly selected profile settings, job env, step env. A scope SHALL retain lower-precedence keys it does not override. Profiles SHALL apply only when explicitly selected. Named workflow overrides SHALL NOT affect sibling workflows or legacy top-level jobs.

#### Scenario: Profile overrides a workflow variable
- **WHEN** shared settings set `MIX_ENV=test` and `KEEP=shared`, workflow ci sets `MIX_ENV=dev`, and selected profile ci sets `MIX_ENV=prod` and `CI=true`
- **THEN** a job without overrides receives `MIX_ENV=prod`, `KEEP=shared`, and `CI=true`

#### Scenario: Job and step overrides
- **WHEN** settings supply `VALUE=settings`, a job sets `VALUE=job`, and one step sets `VALUE=step`
- **THEN** that step receives `step` and a subsequent step without an override receives `job`

#### Scenario: Caller has a conflicting value
- **WHEN** the caller exports `MIX_ENV=dev` and shared settings set `MIX_ENV=test`
- **THEN** a job without more specific overrides receives `MIX_ENV=test`

#### Scenario: Empty map and empty string
- **WHEN** shared settings set `VALUE=shared`, workflow env is empty, and the selected profile sets `VALUE=""`
- **THEN** the command receives an existing `VALUE` variable whose value is empty

#### Scenario: Profile is not selected
- **WHEN** a profile declares env overrides but no profile is selected
- **THEN** those overrides have no effect

#### Scenario: Workflow isolation
- **WHEN** only workflow ci overrides a shared variable
- **THEN** other workflows and legacy top-level jobs retain the shared value

### Requirement: Commands and expressions use consistent environments
Job commands and job conditions SHALL see caller, resolved settings, and job env values. Step commands and step conditions SHALL additionally see that step's env overrides. Shell and expression conditions SHALL agree on ordinary variable values. Job output expressions SHALL see the job environment without step-only overrides. Runner-owned variable injection and scrubbing SHALL retain existing behavior, except that a `GOODFLOWS_OUTPUT` inherited from the caller SHALL NOT be visible to expressions.

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

### Requirement: Every settings map is validated before execution
All shared, workflow, and profile env maps SHALL follow existing job/step env validation, including malformed keys or values, reserved reporting and partition names, `GOODFLOWS_ENVIRONMENT`, and the configured reporting token variable. Settings env maps SHALL additionally reject `GOODFLOWS_OUTPUT`. Validation SHALL include unselected workflows and profiles and invalid entries hidden by later overrides. Semantic env validation errors after successful TOML deserialization SHALL identify the settings scope without exposing variable values. TOML syntax/type errors SHALL retain existing parser diagnostics, which may include source excerpts and configured values.

#### Scenario: Invalid unselected profile
- **WHEN** an unselected profile sets `GOODFLOWS_PARTITION` or a malformed environment entry
- **THEN** validation fails before any job runs and identifies the profile settings scope

#### Scenario: Invalid overwritten shared variable
- **WHEN** shared settings contain an invalid env entry that a workflow overrides with a valid value
- **THEN** validation still fails for the shared settings entry

#### Scenario: Profile identity override is rejected
- **WHEN** any shared, workflow, or profile settings env map sets `GOODFLOWS_ENVIRONMENT`, including an unselected workflow or profile
- **THEN** validation fails before any job runs and identifies the settings scope without exposing the configured value

#### Scenario: Reporting token in settings
- **WHEN** reporting names `PRIVATE_TOKEN` as its token variable and any settings scope sets `PRIVATE_TOKEN`
- **THEN** validation fails without printing the configured token value

#### Scenario: Inherited publisher credential
- **WHEN** the caller exports the configured reporting token and commands use settings env defaults
- **THEN** the token remains absent from job, step, and condition commands and expression env lookups

### Requirement: Inspection exposes resolved configured defaults
JSON output that reports effective settings SHALL include an `env` object containing the merged shared, workflow, and selected profile env maps. It SHALL exclude caller variables and job/step overrides. Workflow discovery and aggregate validation SHALL resolve env per workflow using the selected profile. Job env entries SHALL continue to describe explicit job configuration.

#### Scenario: Inspect a selected workflow
- **WHEN** `list ci --env local --json` reports effective settings
- **THEN** `settings.env` contains the merged configured defaults for ci and local, and `jobs[*].env` retains explicit job overrides

#### Scenario: Discover and validate workflows
- **WHEN** `list --workflows --env local --json` or aggregate `validate --env local --json` reports workflows
- **THEN** each workflow's settings contain its own merged env defaults with local overrides

#### Scenario: Do not expose ambient variables
- **WHEN** the caller exports an unrelated variable and no settings env map defines it
- **THEN** it does not appear in the effective settings env object

### Requirement: Elixir initialization declares the shared default once
Elixir initialization SHALL write `MIX_ENV=test` once under shared settings env and SHALL omit repeated job env declarations. This SHALL apply to both default and named workflow generation. Generated configurations SHALL validate, and templates with no settings env defaults SHALL omit empty settings env maps.

#### Scenario: Default Elixir workflow
- **WHEN** `init --template elixir` generates a configuration
- **THEN** it contains one shared `MIX_ENV=test` declaration, no job-level copies, and all generated jobs inherit it

#### Scenario: Named Elixir workflow
- **WHEN** `init --template elixir --workflow ci` generates a configuration
- **THEN** ci jobs inherit the single shared declaration and validation succeeds

#### Scenario: Other template
- **WHEN** a Rust, Go, Node, or generic template has no environment defaults
- **THEN** its generated TOML contains no empty settings env map
