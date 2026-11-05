#!/usr/bin/env bash
# Prints the CLI (Cargo.toml) and both reporter package versions, failing when
# any is missing. Given a release tag (`$1`), also fails unless the tag names
# the version of what it releases. The CLI and each reporter are versioned on
# their own:
#
#   v<version>                  the CLI, Cargo.toml
#   exunit-reporter-v<version>  the ExUnit reporter, reporters/exunit/mix.exs
#   jest-reporter-v<version>    the Jest reporter, reporters/jest/package.json
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
if [ -z "$cli" ] || [ -z "$exunit" ] || [ -z "$jest" ]; then
  echo "error: every version must be set: Cargo.toml (version), reporters/exunit/mix.exs (@version) and reporters/jest/package.json (version)" >&2
  status=1
fi
if [ -n "$tag" ]; then
  echo "tag: $tag"
  case "$tag" in
    exunit-reporter-v*) want="$exunit" file="reporters/exunit/mix.exs" prefix="exunit-reporter-v" ;;
    jest-reporter-v*) want="$jest" file="reporters/jest/package.json" prefix="jest-reporter-v" ;;
    v*) want="$cli" file="Cargo.toml" prefix="v" ;;
    *)
      echo "error: tag $tag is not a release tag (v<version>, exunit-reporter-v<version> or jest-reporter-v<version>)" >&2
      exit 1
      ;;
  esac
  if [ "$tag" != "$prefix$want" ]; then
    echo "error: tag $tag does not match $file version $prefix$want" >&2
    status=1
  fi
fi
exit "$status"
