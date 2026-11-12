# Installing goodflows

The README's [Install](../README.md#install) section has the short version. This page covers the release archives in detail and how to build from source.

## From source with Cargo

With Rust 1.88 or newer:

```bash
cargo install --git https://github.com/agoodway/goodflows.git --locked
```

## Release archives

[GitHub Releases](https://github.com/agoodway/goodflows/releases) has archives for Linux and macOS, on `x86_64` and `aarch64`. v0.7.0 publishes these names, and [`.github/workflows/release.yml`](../.github/workflows/release.yml) builds the same four targets:

- `goodflows-v<VERSION>-x86_64-unknown-linux-gnu.tar.gz`
- `goodflows-v<VERSION>-aarch64-unknown-linux-gnu.tar.gz`
- `goodflows-v<VERSION>-x86_64-apple-darwin.tar.gz`
- `goodflows-v<VERSION>-aarch64-apple-darwin.tar.gz`

Each archive has a `.sha256` file next to it. The release also includes `SHA256SUMS`, which joins those checksum files into one. An archive holds `goodflows`, `README.md`, and `LICENSE` at the top level.

Run these in the directory that holds the archive and its `.sha256` file:

```bash
sha256sum -c goodflows-v0.7.0-x86_64-unknown-linux-gnu.tar.gz.sha256
tar -xzf goodflows-v0.7.0-x86_64-unknown-linux-gnu.tar.gz
mkdir -p ~/.local/bin
install -m 755 goodflows ~/.local/bin/goodflows
```

On macOS, `shasum -a 256 -c` checks the same `.sha256` file.

## glibc on Linux

The release workflow links the Linux archives with Rust 1.88.0 in Debian 12 (`debian:bookworm-slim`). [`scripts/package-release.sh`](../scripts/package-release.sh) rejects a binary that uses glibc symbol versions newer than 2.36, the glibc in Debian 12 and in Ampcode orbs.

The v0.6.0 Linux archives need glibc 2.34. On both `x86_64-unknown-linux-gnu` and `aarch64-unknown-linux-gnu`, the highest versioned glibc symbol is GLIBC_2.34. A later release can need any glibc up to 2.36.

There is no musl archive. For musl, or for a glibc older than the archive needs, build from source on that system:

```bash
cargo build --release --locked
install -m 755 target/release/goodflows ~/.local/bin/goodflows
```
