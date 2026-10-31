# CLI improvements: completion evidence

All eight recommendations are implemented on `feat/cli-improvements`.

| Requirement | Implementation | Verification |
|---|---|---|
| Dependency-aware selection and unknown-job errors | `src/config.rs::select` | transitive selection, whole-graph validation, unknown selection tests |
| Failed prerequisites block dependents; distinct results | `src/runner.rs` | transitive skip, failed exit-code, status/count, independent-job tests |
| Ready scheduling, bounded concurrency, fail-fast | `src/runner.rs::execute` | ready-dependent handshake, exclusive-directory limit, queued fail-fast, join prerequisites, override tests |
| Validate config and correct relative workdir | `src/config.rs`, `validate` command | invalid-config table for run/list/validate, nested path with spaces, env validation, no-command execution tests |
| Project-aware init, templates, force, destination | `src/init.rs` | ten init tests covering all template types, Node manifests/lockfiles, nested destinations, confirmation/EOF, symlinks |
| Prefixed streams, raw files, JSON events | `src/output.rs`, `src/runner.rs::drain` | six output tests for prefixes, logs, safe names, JSON commands/errors/summary, partial/non-UTF-8/large output |
| Timeout and signal cleanup | process groups, cancellation channel | four process tests for timeout, SIGINT/SIGTERM, escaped side effects, inherited pipes, blocked stdout |
| Lockfile, automated checks, release binaries | Cargo.lock, CI/Release workflows, packaging script | locked builds/tests, actionlint, native archive/checksum and extracted binary smoke test |

## Commands and results

- `cargo fmt --check`: passed.
- `cargo clippy --locked --all-targets -- -D warnings`: passed.
- `cargo test --locked`: 33 integration tests passed on default toolchain.
- `cargo +1.88 test --locked`: 33 integration tests passed on Rust 1.88.0.
- `cargo +1.88 build --release --locked`: passed.
- `RUSTUP_TOOLCHAIN=1.88 bash scripts/package-release.sh x86_64-unknown-linux-gnu v0.1.0`: passed.
- Native archive SHA-256, exact contents (binary/README/LICENSE), extracted
  `--version`, generic init, validate, and JSON run: passed.
- Packaging rejects unsupported target and mismatched version tag: passed.
- `./target/release/goodflows --json --log-dir /tmp/goodflows-self-check-logs`:
  fmt, clippy, test, doc all passed (summary: 4 passed, 0 failed).
- `go run github.com/rhysd/actionlint/cmd/actionlint@v1.7.12 -color`: passed.
- `bash -n scripts/package-release.sh` and `git diff --check`: passed.

## Independent review

A fresh reviewer checked the implementation, tests, and workflows. All substantive
findings were reproduced with failing regressions, fixed, and verified with the
full suite:

- Blocked stdout stalled timeouts/signals: dedicated output thread, bounded async
  channel, cancellation-aware acknowledgements; blocked-output test now passes.
- Invalid bytes before a partial UTF-8 character corrupted valid text: preserve
  incomplete suffix independently; mixed-byte boundary test now passes.
- Existing symlink init used the wrong config base: resolve the destination for
  workdir generation; symlink init test now passes.
- Node without a manifest invented a test script: require package.json and known
  declared scripts; missing-manifest regression now passes.

An additional error-path regression showed a failed log write allowed the command
to keep running. Reader errors now terminate/reap the process, and the /dev/full
test passes. Process-group cleanup is performed once, avoiding repeated signals
after a PID might be reused.

## Deliberate limits

Supported runtime platforms are Linux/macOS. Process groups cannot control jobs
that deliberately detach into new groups/sessions. A downstream consumer that
stops reading cannot receive a guaranteed final summary: on signal, output gets
250 ms to drain, then further events are suppressed so shutdown can finish.
Only native Linux x86_64 packaging was executed locally; the checked GitHub
workflow runs the other three native platform/architecture combinations after
dispatch/tagging. No remote branch push, tag, or release publication was performed.
