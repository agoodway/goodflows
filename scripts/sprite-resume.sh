#!/usr/bin/env bash
# GoodFlows Sprite resume hook (examples/sprites/goodflows.toml). Runs on every
# later prepared run against a fresh source tree. It verifies the persistent
# toolchain and attaches the persistent Cargo caches (via rust-env.sh); it never
# reinstalls the environment implicitly. To rebuild it, destroy the Sprite.
set -euo pipefail
state="${GOODFLOWS_SPRITE_STATE_DIR:?run this as a GoodFlows Sprite hook}"
if [ ! -f "$state/rust-env.sh" ]; then
  echo "rust-env.sh is missing from $state; destroy and recreate this Sprite to rerun setup" >&2
  exit 1
fi
. "$state/rust-env.sh"
rustc --version
cargo --version
# Dependencies only; offline when Cargo.lock is unchanged.
cargo fetch --locked
