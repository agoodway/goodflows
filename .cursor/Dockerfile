# Cloud agent image for goodflows.
# Toolchains match the versions this repo already pins:
#   Rust 1.88.0  — Cargo.toml rust-version, scripts/sprite-setup.sh, CI
#   Elixir 1.18.6 / Erlang OTP 27.3.4.19 — reporters.yml and mix.exs "~> 1.18"
#   Node.js 24.22.2 — CI and reporters.yml request Node 24
# Dependency fetch lives in .cursor/install.sh, which runs after checkout.

FROM ubuntu:24.04

ARG DEBIAN_FRONTEND=noninteractive

RUN apt-get update \
    && apt-get install -y --no-install-recommends \
        build-essential \
        ca-certificates \
        curl \
        git \
        pkg-config \
        sudo \
        unzip \
        xz-utils \
    && rm -rf /var/lib/apt/lists/* \
    && echo "ubuntu ALL=(ALL) NOPASSWD:ALL" > /etc/sudoers.d/ubuntu \
    && chmod 440 /etc/sudoers.d/ubuntu

ENV LANG=C.UTF-8 \
    LC_ALL=C.UTF-8 \
    RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin

RUN printf '%s\n' \
        'export RUSTUP_HOME=/usr/local/rustup' \
        'export CARGO_HOME=/usr/local/cargo' \
        'export LANG="${LANG:-C.UTF-8}"' \
        'export LC_ALL="${LC_ALL:-C.UTF-8}"' \
        'case ":$PATH:" in' \
        '  *:/usr/local/cargo/bin:*) ;;' \
        '  *) export PATH="/usr/local/cargo/bin:$PATH" ;;' \
        'esac' \
        > /etc/profile.d/goodflows-toolchains.sh \
    && chmod 644 /etc/profile.d/goodflows-toolchains.sh

# Node.js 24.22.2 (linux-x64). Checksum from nodejs.org SHASUMS256.txt.
ARG NODE_VERSION=24.22.2
ARG NODE_SHA256=48cc0e21e048790e54c0cf5749c2725005313d652651c40b7a21feb9697aaf75
RUN curl -fsSL "https://nodejs.org/dist/v${NODE_VERSION}/node-v${NODE_VERSION}-linux-x64.tar.xz" -o /tmp/node.tar.xz \
    && echo "${NODE_SHA256}  /tmp/node.tar.xz" | sha256sum -c - \
    && tar -xJf /tmp/node.tar.xz -C /usr/local --strip-components=1 --no-same-owner \
    && rm /tmp/node.tar.xz \
    && node --version \
    && npm --version

# Prebuilt Erlang/OTP from builds.hex.pm (ubuntu-24.04, OTP 27 line).
# Elixir 1.18.6 built for OTP 27, same pin as .github/workflows/reporters.yml.
ARG OTP_VERSION=27.3.4.19
ARG OTP_SHA256=8ce4332d7b90fef51f1ecaf4b9fe2a2e363b2c1b6025b0ff044ee1bf7d346f87
ARG ELIXIR_VERSION=1.18.6
ARG ELIXIR_SHA256=a116bc251166815787fd9023b28533bae4059bb668cff767830506a211191e08
RUN curl -fsSL "https://builds.hex.pm/builds/otp/ubuntu-24.04/OTP-${OTP_VERSION}.tar.gz" -o /tmp/otp.tar.gz \
    && echo "${OTP_SHA256}  /tmp/otp.tar.gz" | sha256sum -c - \
    && mkdir -p /usr/local/lib/erlang \
    && tar -xzf /tmp/otp.tar.gz -C /usr/local/lib/erlang --strip-components=1 \
    && rm /tmp/otp.tar.gz \
    && (cd /usr/local/lib/erlang && ./Install -minimal /usr/local/lib/erlang) \
    && curl -fsSL "https://builds.hex.pm/builds/elixir/v${ELIXIR_VERSION}-otp-27.zip" -o /tmp/elixir.zip \
    && echo "${ELIXIR_SHA256}  /tmp/elixir.zip" | sha256sum -c - \
    && mkdir -p /usr/local/lib/elixir \
    && unzip -q /tmp/elixir.zip -d /usr/local/lib/elixir \
    && rm /tmp/elixir.zip \
    && for cmd in erl erlc escript epmd dialyzer typer; do \
         printf '#!/bin/sh\nexec /usr/local/lib/erlang/bin/%s "$@"\n' "$cmd" > "/usr/local/bin/$cmd"; \
         chmod 755 "/usr/local/bin/$cmd"; \
       done \
    && for cmd in elixir elixirc iex mix; do \
         printf '#!/bin/sh\nexec /usr/local/lib/elixir/bin/%s "$@"\n' "$cmd" > "/usr/local/bin/$cmd"; \
         chmod 755 "/usr/local/bin/$cmd"; \
       done \
    && erl -eval 'io:format("~s~n", [erlang:system_info(otp_release)]), halt().' -noshell \
    && elixir --version

# Rust 1.88.0 via pinned rustup-init 1.29.3. rustfmt and clippy are required
# by the fmt and clippy jobs in goodflows.toml.
ARG RUSTUP_VERSION=1.29.3
ARG RUSTUP_SHA256=b1c8cb14e30889e7e2999e6b69e62d9e9b9ffc0f4df8180564d306d9680e6c55
RUN mkdir -p /usr/local/rustup /usr/local/cargo \
    && chown -R ubuntu:ubuntu /usr/local/rustup /usr/local/cargo
USER ubuntu
RUN curl --proto '=https' --tlsv1.2 -fsSL \
        "https://static.rust-lang.org/rustup/archive/${RUSTUP_VERSION}/x86_64-unknown-linux-gnu/rustup-init" \
        -o /tmp/rustup-init \
    && echo "${RUSTUP_SHA256}  /tmp/rustup-init" | sha256sum -c - \
    && chmod +x /tmp/rustup-init \
    && /tmp/rustup-init -y --no-modify-path --profile minimal \
        --default-toolchain 1.88.0 -c rustfmt,clippy \
    && rm -f /tmp/rustup-init \
    && rustc --version \
    && cargo --version \
    && rustfmt --version \
    && cargo clippy --version
USER root
RUN for cmd in rustup rustc cargo rustdoc rustfmt cargo-fmt cargo-clippy clippy-driver; do \
      ln -sfn "/usr/local/cargo/bin/$cmd" "/usr/local/bin/$cmd"; \
    done
USER ubuntu
