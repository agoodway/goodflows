#!/usr/bin/env bash
# Fetch dependencies for the workflows in goodflows.toml, the Sprite example,
# and reporters/fixtures/goodflows.toml. Idempotent: a second run refreshes
# the same lockfiles and does not change versions.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$root"

cargo fetch --locked

# Hex and rebar live in the agent's home. The reporter itself has no Hex
# packages; the fixture depends on it with `only: :test`.
mix local.hex --force
mix local.rebar --force

(
  cd reporters/exunit
  MIX_ENV=test mix deps.get
  MIX_ENV=test mix deps.compile
)

(
  cd reporters/fixtures/exunit
  MIX_ENV=test mix deps.get
  MIX_ENV=test mix deps.compile
)

(
  cd reporters/fixtures/jest
  npm ci --no-audit --no-fund
)
