# Releasing goodflows

`Cargo.lock` is committed for reproducible dependency resolution. Edition 2021 does
not make the resolver MSRV-aware, so `.cargo/config.toml` sets
`[resolver] incompatible-rust-versions = "fallback"`; a plain `cargo update` then
prefers versions compatible with `rust-version` (1.88). Confirm with
`cargo +1.88.0 check --locked`. HTTPS uses rustls on the ring provider, so release
binaries need no OpenSSL.

CI runs formatting, Clippy, tests, a release build, and goodflows's own checks on
Linux/macOS with Rust 1.88 and stable. Release builds run checks on all four native
platform/architecture combinations before packaging. Linux archives are then
compiled with Rust 1.88.0 in a Debian 12 container so the binary runs on Ampcode
orbs (glibc 2.36).

## Release workflow

The Release workflow can be dispatched manually to build artifacts without
publishing (its `mirror-dry-run` job prints the reporter splits and pushes
nothing). Pushing a `v<version>` tag matching `Cargo.toml` builds and publishes
archives, individual SHA-256 checksums, and `SHA256SUMS`, then pushes each bundled
reporter to its read-only mirror with the same tag.

On a tag push the jobs run in this order, and each stops the release if it fails:

1. `verify-versions`: the CLI and both reporter versions, and the tag, agree.
2. `mirror-check` (in the `reporter-mirrors` environment): both deploy keys are
   set, the tag names a commit on `main`, and each mirror's `main` is an ancestor
   of the new split (`scripts/mirror-reporters.sh <tag> --verify`). Nothing is
   pushed. `reporters` runs the same reporter drift guard as CI, alongside.
3. `build` and then `publish` create the GitHub release.
4. `mirror-reporters` repeats the checks and pushes each mirror's `main` and tag,
   only after the release is published, so no mirror ever has a tag without a CLI
   release. If it fails after publish, fix the cause and use **Re-run failed
   jobs**; mirrors already pushed are up to date.

Releases are tagged on `main` only. A tag on any other branch fails `mirror-check`
before anything is built. A fix for an older release ships as the next patch
version from `main`.

## Cutting a release

1. Bump the version in `Cargo.toml` (and run `cargo update -p goodflows`),
   `reporters/exunit/mix.exs` (`@version`) and `reporters/jest/package.json`
   together. CI's `release-scripts` job fails on every commit where the three
   disagree. Check locally with `scripts/verify-versions.sh v<version>`.
2. Review CI on `main`, including the `reporters` and `release-scripts` jobs.
3. Make sure the `reporter-mirrors` environment, both mirror deploy keys and the
   `v*` tag ruleset are configured.
4. Push the tag from `main`: `git tag v<version> origin/main`, then
   `git push origin v<version>`.

`scripts/test-release-scripts.sh` (run in CI) tests `verify-versions.sh` and
`mirror-reporters.sh` against local bare repositories.

To exercise packaging locally (Linux targets need Docker; macOS builds natively):

```bash
bash scripts/package-release.sh x86_64-unknown-linux-gnu v0.5.0
```
