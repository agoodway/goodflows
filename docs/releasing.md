# Releasing goodflows

The repository commits `Cargo.lock` for reproducible dependency resolution.
Edition 2021 does not make the resolver MSRV-aware, so `.cargo/config.toml` sets
`[resolver] incompatible-rust-versions = "fallback"`. With that setting, a plain
`cargo update` prefers versions compatible with `rust-version` (1.88). Confirm
with `cargo +1.88.0 check --locked`. HTTPS uses rustls on the ring provider, so
release binaries need no OpenSSL.

CI runs formatting, Clippy, tests, a release build, and goodflows's own checks on
Linux and macOS with Rust 1.88 and stable. Release builds run checks on all four
native platform and architecture combinations before packaging. The release then
compiles Linux archives with Rust 1.88.0 in a Debian 12 container, so the binary
runs on Ampcode orbs (glibc 2.36).

The CLI and each reporter have their own versions and their own releases, each
from its own tag:

| Tag                          | Releases            | Version in                              |
| ---------------------------- | ------------------- | --------------------------------------- |
| `v<version>`                 | the CLI             | `Cargo.toml`                            |
| `exunit-reporter-v<version>` | the ExUnit reporter | `reporters/exunit/mix.exs` (`@version`) |
| `jest-reporter-v<version>`   | the Jest reporter   | `reporters/jest/package.json`           |

A CLI release never touches the reporter mirrors, so a reporter only gets a new
version when it changes. The reporters write `test-timings/1`, which every
goodflows release from v0.6.0 accepts. A change to that contract ships as a new
format (see `src/reporting/contract.rs`), not as a reporter release that needs a
matching CLI.

## CLI releases

You can dispatch the Release workflow manually to build artifacts without
publishing. Pushing a `v<version>` tag that matches `Cargo.toml` builds and
publishes archives, individual SHA-256 checksums, and `SHA256SUMS`.

On a tag push the jobs run in this order, and each stops the release if it fails:

1. `verify-versions` checks that the tag names the `Cargo.toml` version.
2. `build` runs, and `reporters` runs the same reporter drift guard as CI
   alongside it. A CLI that rejects the bundled reporters' output therefore
   never ships.
3. `publish` creates the GitHub release.

To cut a CLI release:

1. Bump the version in `Cargo.toml` and run `cargo update -p goodflows`. Check
   locally with `scripts/verify-versions.sh v<version>`.
2. Review CI on `main`.
3. Push the tag from `main` with `git tag v<version> origin/main`, then
   `git push origin v<version>`.

## Reporter releases

The Reporter release workflow publishes one reporter to its read-only mirror
(`agoodway/goodflows-exunit-reporter` or `agoodway/goodflows-jest-reporter`). It
pushes the mirror's `main` and a plain `v<version>` tag, so users pin, for
example, `tag: "v0.6.1"`. If you dispatch it manually and choose a reporter, it
runs `mirror-dry-run`, which prints the split and pushes nothing.

On a tag push the jobs run in this order, and each stops the release if it fails:

1. `verify-versions` checks that the tag names that reporter's package version.
2. `mirror-check` runs in the `reporter-mirrors` environment. It checks that the
   reporter's deploy key is set, that the tag names a commit on `main`, and that
   the mirror's `main` is an ancestor of the new split
   (`scripts/mirror-reporters.sh <tag> --verify`). It pushes nothing. Alongside
   it, `reporters` runs the same reporter drift guard as CI.
3. `mirror-reporter` repeats the checks and pushes the mirror's `main` and tag.
   If it fails, fix the cause and use **Re-run failed jobs**.

Tag reporter releases on `main` only. A tag on any other branch fails
`mirror-check`, because the mirror's `main` could not fast-forward to it later.
A fix for an older reporter release ships as its next patch version from `main`.

To cut a reporter release:

1. Bump the reporter's version, which is `@version` in `reporters/exunit/mix.exs`
   or `version` in `reporters/jest/package.json`. Check locally with
   `scripts/verify-versions.sh exunit-reporter-v<version>` (or
   `jest-reporter-v<version>`).
2. Review CI on `main`, including the `reporters` and `release-scripts` jobs.
3. Make sure the `reporter-mirrors` environment, the mirror's deploy key and the
   tag ruleset cover reporter tags.
4. Push the tag from `main`. For example, run
   `git tag jest-reporter-v<version> origin/main`, then
   `git push origin jest-reporter-v<version>`.

CI runs `scripts/test-release-scripts.sh`, which tests `verify-versions.sh` and
`mirror-reporters.sh` against local bare repositories.

You can exercise packaging locally. Linux targets need Docker, and macOS builds
natively. Run:

```bash
bash scripts/package-release.sh x86_64-unknown-linux-gnu v0.5.0
```
