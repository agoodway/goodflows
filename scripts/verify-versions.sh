#!/usr/bin/env bash
# Fails unless the CLI (Cargo.toml) and both reporter packages declare the same
# version and, when a release tag is given (`$1`, for example `v0.6.0`), unless
# the tag names that version too. The reporters ship in lockstep with the CLI.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tag="${1:-}"

cli="$(sed -n 's/^version = "\(.*\)"$/\1/p' "$root/Cargo.toml" | head -n 1)"
exunit="$(sed -n 's/^  @version "\(.*\)"$/\1/p' "$root/reporters/exunit/mix.exs" | head -n 1)"
jest="$(node -p 'require(process.argv[1]).version' "$root/reporters/jest/package.json")"

echo "Cargo.toml: ${cli:-<missing>}"
echo "reporters/exunit/mix.exs: ${exunit:-<missing>}"
echo "reporters/jest/package.json: ${jest:-<missing>}"
status=0
if [ -z "$cli" ] || [ "$cli" != "$exunit" ] || [ "$cli" != "$jest" ]; then
  echo "error: the CLI and reporter versions must agree; bump the version in Cargo.toml, reporters/exunit/mix.exs (@version) and reporters/jest/package.json together (see \"Development and releases\" in README.md)" >&2
  status=1
fi
if [ -n "$tag" ]; then
  echo "tag: $tag"
  if [ "$tag" != "v$cli" ]; then
    echo "error: tag $tag does not match version v$cli" >&2
    status=1
  fi
fi
exit "$status"
