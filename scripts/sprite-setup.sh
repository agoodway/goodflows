#!/usr/bin/env bash
# GoodFlows Sprite setup hook (examples/sprites/goodflows.toml). Runs until it
# succeeds once. Installs rustup and Rust 1.88.0 into the identity's persistent
# state directory and writes rust-env.sh, which jobs source explicitly; a hook's
# own exports never reach later commands.
set -euo pipefail
state="${GOODFLOWS_SPRITE_STATE_DIR:?run this as a GoodFlows Sprite hook}"
toolchain=1.88.0
export CARGO_HOME="$state/cargo"
export RUSTUP_HOME="$state/rustup"
export PATH="$CARGO_HOME/bin:$PATH"
mkdir -p "$state"

# A `rustup` found on PATH may be a platform wrapper rather than an
# installation; only the binary in our own CARGO_HOME counts.
if ! "$CARGO_HOME/bin/rustup" --version >/dev/null 2>&1; then
  installer="$(mktemp)"
  trap 'rm -f "$installer"' EXIT
  curl --proto '=https' --tlsv1.2 -fsSL https://sh.rustup.rs -o "$installer"
  sh "$installer" -y --no-modify-path --profile minimal --default-toolchain none
  rm -f "$installer"
  trap - EXIT
fi
"$CARGO_HOME/bin/rustup" toolchain install "$toolchain" --profile minimal --no-self-update \
  --component rustfmt --component clippy

cat > "$state/rust-env.sh.tmp" <<ENV
export CARGO_HOME="$CARGO_HOME"
export RUSTUP_HOME="$RUSTUP_HOME"
export RUSTUP_TOOLCHAIN="$toolchain"
export CARGO_TARGET_DIR="$state/target"
export PATH="$CARGO_HOME/bin:\$PATH"
ENV
mv "$state/rust-env.sh.tmp" "$state/rust-env.sh"

. "$state/rust-env.sh"
rustc --version
cargo --version
cargo fetch --locked
