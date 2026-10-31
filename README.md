# goodci

A tiny, config-driven CI runner designed to run inside [Ampcode orbs](https://ampcode.com/docs/orbs) instead of (or in addition to) GitHub Actions.

It gives you:

- A simple TOML config (`goodci.toml`)
- Job dependencies (`depends_on`)
- Tunable parallelism (match the orb size: tiny=1, small=2, medium=4, large=8, xxlarge=16)
- Fail-fast or continue-on-error
- Per-job timeouts and environment variables
- Colored output + summary

## Quick start

```bash
# Generate an example config
goodci init

# Edit goodci.toml to match your project
# Then run
goodci
```

If `goodci.toml` already exists, `goodci init` asks before overwriting it.
Enter `y` or `yes` to replace it; any other response (including Enter or
end-of-input) keeps the existing file.

Or override settings on the command line:

```bash
goodci --parallelism 2 --no-fail-fast
goodci --jobs fmt,clippy
goodci --config ./ci/goodci.toml
```

## Config

```toml
[settings]
parallelism = 4          # max concurrent jobs (0 or omit = CPU count)
fail_fast = true         # stop on first failure
# workdir = "."          # optional

[[jobs]]
name = "fmt"
command = "cargo fmt --check"

[[jobs]]
name = "test"
command = "cargo test --all"
depends_on = ["fmt"]
timeout_secs = 300
env = { RUST_BACKTRACE = "1" }
```

Commands are run via `sh -c` by default (so pipes, redirects, and shell features work). Set `shell = false` for direct exec.

## Using on Ampcode orbs

1. Commit `goodci.toml` (and optionally the `goodci` binary or a build step) to your repo.
2. In `.agents/setup`, make sure Rust is available if you need to build the tool itself, or install a prebuilt binary:

```bash
# Example snippet for .agents/setup
if ! command -v goodci >/dev/null; then
  cargo install --path ./goodci --locked   # or download a release binary
fi
```

3. In an orb thread, just tell the agent:

   > Run the mini CI (`goodci`) and fix anything that fails.

Or run it yourself in the orb terminal.

Because orbs are billed by the minute and come in fixed CPU sizes, set `parallelism` in the config (or via `--parallelism`) to the number of cores you actually have. Oversubscribing usually just makes things slower on these machines.

## Commands

| Command          | Description                          |
|------------------|--------------------------------------|
| `goodci` / `goodci run` | Run the jobs                      |
| `goodci list`    | Show jobs and their dependencies     |
| `goodci init`    | Write an example `goodci.toml`       |

Global flags: `--config`, `--parallelism`, `--fail-fast`, `--no-fail-fast`, `--jobs`.

## Build

```bash
cargo build --release
# binary is at target/release/goodci
```

Requires Rust 1.70+.
