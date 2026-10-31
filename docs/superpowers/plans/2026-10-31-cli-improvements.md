# CLI Improvements Implementation Plan

> **For agentic workers:** Use superpowers:executing-plans to implement task by task.

**Goal:** Address all eight recommended CLI improvements.
**Architecture:** Keep Rust/Tokio, extract focused modules, and schedule ready jobs
using a bounded JoinSet. Share validation and event output across commands.
**Tech Stack:** Rust, Tokio, clap, serde/TOML, serde_json, shell-words, libc on Unix.
**Spec:** `docs/superpowers/specs/2026-10-31-cli-improvements-design.md`

## Global Constraints

Linux/macOS; preserve existing TOML and default command; fail_fast defaults true;
no automatic remote publication; JSON Lines schema_version=1; exit 130/143 for
interrupt/termination. All listed spec behaviors must be covered.

## Review Focus

- Config paths containing spaces and nested directories must resolve consistently.
- Selected jobs must not hide malformed unrelated config or miss unknown names.
- Failed processes and lingering descendant pipes must not hang output draining.
- Output bytes without UTF-8 or a final newline must still be delivered.
- Ambiguous projects and hostile job names must not produce misleading configs or unsafe log paths.

## Task 1: Configuration validation and selection

Files: `src/config.rs`, `src/main.rs`, `tests/runner.rs`, `tests/common/mod.rs`.
Interfaces: `Config::load(path)`, `Config::validate()`, `Config::select(names)`;
config holds canonical workdir and jobs with command/env/dependency metadata.
- [x] Write CLI tests for validation, workdir resolution, selection, defaults,
  malformed commands, and conflicting flags; run and observe missing behavior.
- [x] Extract parsing/validation and implement dependency closure; add `validate`.
- [x] Run focused and full tests; review task diff.

## Task 2: Scheduling, results, and process lifecycle

Files: `src/runner.rs`, `src/main.rs`, `tests/runner.rs`, `tests/process.rs`.
Interfaces: runner consumes validated selected jobs/workdir/options and emits
structured JobResult values; use status enums and a shared cancellation signal.
- [x] Write failing tests for failure propagation, queued fail-fast, concurrency,
  ready scheduling, failed exit codes, timeout and signal cleanup.
- [x] Implement ready scheduler and process groups, with cancellation/reaping.
- [x] Run focused and full tests; review task diff.

## Task 3: Streaming output and log files

Files: `src/output.rs`, `src/runner.rs`, `src/main.rs`, `tests/output.rs`.
Interfaces: shared Output emits typed schema_version=1 JSON Lines or human text;
runner drains stdout/stderr concurrently and optionally writes stream files.
- [x] Write failing tests for prefixed streams, JSON summaries/errors, raw log files,
  invalid bytes, partial lines, and safe filenames.
- [x] Implement events, result reporting, incremental output, and logs.
- [x] Run focused and full tests; review task diff.

## Task 4: Project-aware initialization

Files: `src/init.rs`, `src/main.rs`, `tests/init.rs`.
Interfaces: init takes destination, optional Template, force, and Output.
- [x] Write failing tests for each template/detection, ambiguity, Node scripts and
  package managers, custom nested paths, force, and existing confirmation.
- [x] Implement template generation and guarded writes; generate valid configs.
- [x] Run focused and full tests; review task diff.

## Task 5: Documentation and release confidence

Files: `README.md`, `.gitignore`, `Cargo.toml`, `Cargo.lock`,
`.github/workflows/ci.yml`, `.github/workflows/release.yml`, `goodci.toml`.
- [x] Track lockfile, configure CI and tag-triggered release archives/checksums.
- [x] Document all commands, statuses, templates, logs, JSON, platform/toolchain
  support and migration behavior; validate workflow structure and release steps.
- [x] Run cargo fmt --check, cargo test --locked, cargo clippy --locked
  --all-targets -- -D warnings, cargo build --release --locked, and goodci itself.
- [x] Request an independent whole-change review and fix substantive findings.

## Progress

Baseline: 2 integration tests pass. Work in the clean shared checkout on a feature
branch to keep deliverables visible where the user is working. User authorized
the complete recommendation list; settle routine implementation choices inline.

Task 1 complete: six CLI regression tests reproduced missing validation,
dependency selection, relative paths, and direct-exec quoting. All eight tests
pass after implementation. Also reject invalid environment entries before spawn.
Tasks 2 and 3 share process I/O and result types; implement them together with
separate behavioral test files to avoid introducing a temporary output API.

Tasks 2–4 complete: scheduling/output/process tests and all init tests pass.
25 integration tests pass on the default toolchain and Rust 1.88.0. Rust 1.88 is
the declared tested minimum. CI/release automation and documentation added;
remaining work is error-path review, packaging verification, and independent review.

Final completion audit: see `2026-10-31-verification.md`. All five tasks complete;
independent review findings reproduced and fixed, 33 integration tests green,
native release archive verified. No outstanding substantive findings.
