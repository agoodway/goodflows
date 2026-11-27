---
name: run-ci-goodflows
description: "Use when running GoodFlows checks or reading a goodflows failure in the goodflows CLI repository."
---

# Run CI for goodflows

## What GoodFlows runs

The config is `goodflows.toml` at the repository root. It has one workflow, the default (top-level `[[jobs]]`), with `parallelism = 4` and `fail_fast = true`:

1. `fmt`: `cargo fmt --check`
2. `clippy`: `cargo clippy --locked --all-targets -- -D warnings`, after `fmt`
3. `test`: builds `reporters/nextest`, then `goodflows-nextest-reporter -- cargo nextest run --locked --workspace`, after `fmt`. It lists `reports = ["test-timings"]`, so the reporter's `test-timings/1` report is collected and uploaded. It needs cargo-nextest installed and JUnit enabled in `.config/nextest.toml`
4. `doc`: `cargo doc --locked --no-deps`, after `fmt`

`[reporting]` names the project `goodflows` with `history = true`, so each run, and the `test` job's test-timings report, is uploaded to the server that `GOODFLOWS_URL` and `GOODFLOWS_API_KEY` name.

## Scenarios

Nobody chose scenarios for this repository, so the map is the config.

- **Default: every check, before a commit or a change request.** This checkout is the goodflows package, so build it and run its own binary:

  ```bash
  cargo build --release --locked
  ./target/release/goodflows
  ```

- **Some checks only:** `./target/release/goodflows --jobs clippy,test` runs those jobs plus their `depends_on`.

## Run it locally

From the repository root, after `cargo build --release --locked`:

| Job | Command in the config | Invocation that runs it |
|---|---|---|
| fmt | `cargo fmt --check` | `./target/release/goodflows` |
| clippy | `cargo clippy --locked --all-targets -- -D warnings` | `./target/release/goodflows` |
| test | `goodflows-nextest-reporter -- cargo nextest run --locked --workspace` | `./target/release/goodflows` |
| doc | `cargo doc --locked --no-deps` | `./target/release/goodflows` |

## Read a failure

`./target/release/goodflows last` prints the last run's report again, and `./target/release/goodflows run --failed` reruns the jobs that did not pass. For exit codes, error codes, the `Failures` section, `last`, `run --failed`, detached runs, and the JSON fields, run `./target/release/goodflows agents`, which prints the guide for the installed goodflows.

## Green

`./target/release/goodflows` passed for the default workflow, as `./target/release/goodflows agents` describes.

## Keeping this skill accurate

When `goodflows.toml` changes, the job names and the scenario map, with its default scenario and build line, have to match the config. When the GoodFlows config changes, run `maintain-ci-skill` to keep this skill accurate. Install it with `goodflows skills install`.
