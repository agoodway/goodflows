# Named workflows

Implement the user's accepted design: independently runnable named workflows in
one TOML config, inheriting shared settings, with local job dependency graphs.
Retain existing configs and execution behavior; no cross-workflow dependencies.

## Contract

- `[[workflows.ci.jobs]]` defines jobs; `[workflows.ci.settings]` overrides shared
  `[settings]` values field by field (including false and parallelism=0).
- `goodflows run ci`, `goodflows list ci`, `goodflows validate ci` select ci. `--jobs` selects
  the dependency closure inside that workflow. Repeated job names across workflows
  are valid. All config graphs are validated before any job runs.
- Existing root `[[jobs]]` form the default workflow and preserve legacy log paths.
  Root jobs and a named `default` cannot coexist. Without root jobs, a named
  `default` is used for bare run/list. Otherwise require an explicit workflow;
  never choose an arbitrary workflow (especially release).
- `list --workflows` discovers workflows with counts and effective settings.
  Bare validate checks every workflow and reports per-workflow selection metadata.
  Legacy validate output fields remain intact. --jobs with aggregate validation
  requires an explicit workflow. Workflow names are nonempty, <=64 UTF-8 bytes,
  and contain no control characters. All log components are encoded safely and
  uniquely, including on case-insensitive filesystems.
- Relative workdirs, shared or overridden, resolve from the config's directory.
  Settings precedence: CLI > workflow > shared > built-in defaults.
- JSON events for a selected workflow include workflow. Pre-selection errors may
  omit it; explicit workflow selection is available even on selection errors.
  Named log paths use <log-dir>/<encoded-workflow>/, legacy default stays flat.
- `init --workflow ci` writes a named-workflow starter with the existing template,
  config path, overwrite, nonblocking input, and clean TOML behavior. It writes a
  whole config and does not append or modify another workflow silently.

## Tasks

- [x] Write failing CLI integration tests for selection, defaults, discovery,
  metadata, settings inheritance, relative paths, graph isolation/validation,
  errors, log paths, and named initialization.
- [x] Add config Workflow/WorkflowSettings models and resolve/validate/select APIs;
  reuse job validation, selection, scheduling and process cleanup.
- [x] Extend command parsing, inspection/validation events and output context;
  scope named log paths; generate named starters and document the new syntax.
- [x] Run the full suite and Rust 1.88 fmt/Clippy/tests, build release, exercise
  named workflows and legacy self-check, then package/test native binary.
- [x] Obtain an independent whole-change review (requesting-code-review skill),
  reproduce/fix substantive findings, and record completion evidence.

Keep changes local on feat/named-workflows; no merge, push or release publication.


## Completion evidence

All requirements above are implemented. Existing 45 integration tests continue
passing; 14 workflow tests cover selection/closure, event context, shared and
per-workflow inheritance (false/zero included), CLI overrides, actual fail-fast
execution, relative directories, discovery, aggregate/selected validation,
backward-compatible defaults, unknown/missing selections, graph isolation and
whole-file validation, safe/case-distinct logs, nested named init, name bounds,
and positional names that resemble CLI flags.

The initial nine workflow tests failed against the old CLI, then passed after
implementation. An added regression caught missing context on implicit-default
post-selection errors; the selected Output context now reaches the outer error
handler. Independent review found one valid positional-name bug: a workflow named
--json enabled JSON even after the option terminator. The regression failed, then
passed after using the parsed JSON flag for normal output (raw scanning remains
only for parse-error reporting). No outstanding substantive review findings.

Verification completed:

- Default toolchain and Rust 1.88.0: 59 integration tests passed on each.
- Rust 1.88 fmt and Clippy --locked --all-targets -- -D warnings: passed.
- Rust 1.88 locked release build: passed.
- Existing root-job goodflows self-check: fmt/clippy/test/doc all passed with default
  workflow context and legacy log layout.
- Native x86_64 Linux archive checksum and extracted binary: named discovery,
  aggregate validation, selected dependency execution, isolated logs, refusing an
  arbitrary default, nested named init and placeholder failure all passed.
- git diff --check: passed. README includes syntax, commands, defaults, precedence,
  JSON metadata and log-path compatibility.

Runtime execution was tested on Linux; macOS coverage remains in the CI matrix.
Changes are committed locally; no merge, push, tag or release publication.
