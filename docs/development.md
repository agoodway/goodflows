# Developing goodflows

## Checks

Run these from the repository root before you open a pull request:

```bash
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo nextest run --locked
node --test tests/amp-plugin.test.mjs
```

The tests run with [cargo-nextest](https://nexte.st) (`curl -LsSf https://get.nexte.st/latest/linux | tar zxf - -C ~/.cargo/bin`); this crate has no doctests, so nextest covers the whole suite. The Node test needs Node.js 22.18 or newer. Run `cargo build` first, because the test runs `target/debug/goodflows`. CI on this repository also runs `./target/release/goodflows`, which executes this repository's own `goodflows.toml`. Its `test` job runs nextest through the in-repo [nextest reporter](../reporters/nextest/README.md), which converts the JUnit file `.config/nextest.toml` enables into a `test-timings/1` report, and goodflows uploads it when `GOODFLOWS_URL` and `GOODFLOWS_API_KEY` are set.

When the README and `docs/` say nothing about a behavior, the code defines it and the tests in [`tests/`](../tests/) pin it.

## Cursor cloud agents

[`.cursor/environment.json`](../.cursor/environment.json) defines the cloud agent environment for this repository. The image installs Rust 1.88.0 with rustfmt and clippy, Elixir 1.18.6 on Erlang/OTP 27.3.4.19, and Node.js 24.22.2. When an environment build runs, [`.cursor/install.sh`](../.cursor/install.sh) fetches the Cargo, Mix, and Jest fixture dependencies.

To change a toolchain, edit its version and checksum in [`.cursor/Dockerfile`](../.cursor/Dockerfile). Keep them in line with these files:

- `Cargo.toml` (`rust-version`)
- `scripts/sprite-setup.sh`
- `reporters/exunit/mix.exs`
- `.github/workflows/ci.yml`
- `.github/workflows/reporters.yml`

The next environment build uses the updated file. Nothing in this repository needs a process that stays running, so the environment sets no `start` command.

## Releases

[releasing.md](releasing.md) has the release steps for the CLI and for each reporter.
