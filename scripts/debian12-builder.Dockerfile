# Ampcode orbs run Debian 12 (glibc 2.36). Stay on bookworm so release
# binaries do not pick up a newer glibc. scripts/package-release.sh rejects
# a binary whose symbol versions exceed that baseline.
FROM debian:bookworm-slim

SHELL ["/bin/bash", "-o", "pipefail", "-c"]

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        binutils \
        ca-certificates \
        curl \
        gcc \
        libc6-dev \
    && rm -rf /var/lib/apt/lists/*

ARG RUST_VERSION=1.88.0
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH

RUN curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs \
    | sh -s -- -y --default-toolchain "${RUST_VERSION}" --profile minimal --no-modify-path \
    && rustc --version \
    && cargo --version
