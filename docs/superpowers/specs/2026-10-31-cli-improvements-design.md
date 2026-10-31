# CLI reliability and usability

Implement all eight improvements requested after reviewing goodflows. Preserve the
TOML job format and default `goodflows` invocation. Support Linux and macOS.

## Approach

Use the existing Rust/Tokio executable, split into configuration, initialization,
execution, and output modules. Replace level barriers with a ready-job scheduler.
Keeping level barriers would retain unnecessary waits; introducing an external
workflow engine would add operational complexity to a small local runner.

## Behavior

1. Validate the entire config before executing anything. Reject unknown fields,
   empty/duplicate job names, empty commands, missing dependencies, duplicate
   dependencies, self-dependencies, cycles, zero timeouts, and malformed direct
   command quoting. Resolve workdir relative to the config directory, including
   `workdir = "."`; require an existing directory. An omitted settings block must
   have the same fail-fast default as an empty settings block (true).
2. `--jobs` includes transitive prerequisites and rejects every unknown selection.
   `validate` and `list` use the same validation and selection logic as `run`.
3. Launch ready jobs in config order up to the concurrency limit. Start dependents
   as soon as their prerequisites succeed. Failed prerequisites skip dependents
   transitively, even with `--no-fail-fast`. Fail-fast stops launching jobs after
   the first observed failure; already-running jobs finish. Report passed, failed,
   skipped, timed_out, and cancelled distinctly. Preserve failed exit codes.
4. `init` detects Cargo.toml, mix.exs, package.json, or go.mod in the current
   project directory. Explicit `--template rust|elixir|node|go|generic` overrides
   detection. Multiple detected project types require an explicit template;
   no detected project uses a generic example. Node selects the package manager
   from lockfiles and generates checks only for declared scripts. `--force`
   bypasses confirmation; `--config` controls the generated file. A generated
   nested config sets workdir back to the project root. Keep the existing [y/N]
   confirmation and EOF behavior. Init creates missing parent directories.
5. Prefix streamed stdout/stderr with job names and stream indicators. `--log-dir`
   optionally writes separate per-job stdout/stderr files with safe indexed
   filenames. `--json` emits JSON Lines events only on stdout, including job
   output, completion, summary, validation/list/init results, and errors. Events
   include schema_version=1. Summary results remain in config order. Do not
   capture unbounded logs in memory. Handle non-UTF-8 output lossily.
6. Timeout/Ctrl+C kills and reaps job process groups on Unix, including descendants.
   Ctrl+C stops launches, marks unfinished jobs cancelled, emits a final summary,
   and exits 130. Handle SIGTERM similarly (exit 143) for CI shutdown. Kill leftover
   descendants when a job's parent exits to avoid inherited output pipes hanging.
7. Track Cargo.lock and build with --locked. Add automated formatting, Clippy,
   tests, and release builds. Tag-triggered releases produce Linux x86_64/aarch64
   and macOS x86_64/aarch64 tarballs with checksums. Do not publish a version tag
   or release as part of implementation. Document installation and compatibility
   changes honestly; verify the stated Rust minimum against the locked dependency
   graph or use the tested toolchain instead of retaining the unsupported 1.70 claim.
8. Add integration tests exercising real CLI processes, dependency graphs,
   scheduling, failure propagation, logs/JSON, init, timeout, and signal cleanup.

## Constraints and choices

No remote execution, caching, retry policy, watch mode, or web UI. Keep existing
shell=true behavior; parse shell=false commands with shell-words so quoted
arguments work without shell expansion. New dependency choices are builder calls.
Exit 0 on successful checks, validation, listing, and declined init; 1 on job or
configuration failure; 130/143 on interrupt/termination. Contradictory fail-fast
flags are rejected. No automatic push or release publication.

## Verification

Tests must show that invalid configs cannot execute commands; selected jobs pull
in prerequisites; failed dependencies never execute; ready jobs avoid unrelated
barriers; concurrency is bounded; fail-fast stops queued work; JSON stdout parses
line by line; file logs preserve stream contents; timeout and signals prevent a
descendant from performing a delayed side effect. Run fmt, full tests, Clippy,
release build, and this repository's own goodflows config before completion.
