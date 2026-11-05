#!/usr/bin/env bash
# Tests scripts/verify-versions.sh and scripts/mirror-reporters.sh without
# GitHub: versions are checked in a scratch copy of the three manifests, and
# mirrors are local bare repositories reached through MIRROR_URL_BASE, with a
# local bare repository as the source `origin`. Covers independent CLI and
# reporter versions, tags that do not match what they release, missing deploy
# keys, a first release, a fast-forward release, --verify and --dry-run (which
# push nothing), a release that leaves the other reporter alone, a mirror with
# commits of its own, and tags that are not on main or not on HEAD.
#
#   scripts/test-release-scripts.sh
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

# Isolate git from the user's configuration (signing, hooks, default branch).
export GIT_CONFIG_NOSYSTEM=1
export GIT_CONFIG_GLOBAL="$work/gitconfig"
git config --global user.name "release script test"
git config --global user.email "release-test@example.invalid"
git config --global init.defaultBranch main
git config --global advice.detachedHead false
unset GITHUB_ACTIONS EXUNIT_REPORTER_DEPLOY_KEY JEST_REPORTER_DEPLOY_KEY \
  MIRROR_URL_BASE MIRROR_SOURCE_REMOTE MIRROR_SOURCE_BRANCH

passed=0
failed=0
out="$work/out"

# expect <status> <pattern> <description> -- <command...>: runs the command,
# capturing stdout and stderr in $out, and checks its exit status and that the
# output matches the pattern (an extended regex; empty matches anything).
expect() {
  local want="$1" pattern="$2" description="$3" status=0
  shift 4
  "$@" >"$out" 2>&1 || status=$?
  if [ "$status" -eq "$want" ] && { [ -z "$pattern" ] || grep -Eq -- "$pattern" "$out"; }; then
    echo "ok: $description"
    passed=$((passed + 1))
  else
    echo "FAIL: $description (exit $status, expected $want; pattern: $pattern)"
    sed 's/^/    /' "$out"
    failed=$((failed + 1))
  fi
}

# check <description> <command...>: a plain assertion.
check() {
  local description="$1"
  shift
  if "$@"; then
    echo "ok: $description"
    passed=$((passed + 1))
  else
    echo "FAIL: $description"
    failed=$((failed + 1))
  fi
}

# ---- verify-versions.sh ----

versions="$work/versions"
mkdir -p "$versions/scripts" "$versions/reporters/exunit" "$versions/reporters/jest"
cp "$root/scripts/verify-versions.sh" "$versions/scripts/"
# Writes the three manifests with the given CLI, ExUnit and Jest versions.
manifests() {
  printf '[package]\nname = "goodflows"\nversion = "%s"\n' "$1" >"$versions/Cargo.toml"
  printf 'defmodule X.MixProject do\n  @version "%s"\nend\n' "$2" >"$versions/reporters/exunit/mix.exs"
  printf '{"name": "goodflows-jest-reporter", "version": "%s"}\n' "$3" >"$versions/reporters/jest/package.json"
}
verify="$versions/scripts/verify-versions.sh"

manifests 0.7.0 0.6.1 0.6.0
expect 0 "Cargo.toml: 0.7.0" "independent versions pass without a tag" -- bash "$verify"
expect 0 "tag: v0.7.0" "a CLI tag that names the CLI version passes" -- bash "$verify" v0.7.0
expect 1 "tag v0.6.0 does not match Cargo.toml version v0.7.0" "a CLI tag that does not match the CLI version fails" -- \
  bash "$verify" v0.6.0
expect 0 "tag: exunit-reporter-v0.6.1" "an ExUnit reporter tag that names its version passes" -- \
  bash "$verify" exunit-reporter-v0.6.1
expect 1 "tag exunit-reporter-v0.7.0 does not match reporters/exunit/mix.exs version exunit-reporter-v0.6.1" \
  "an ExUnit reporter tag that does not match its version fails" -- bash "$verify" exunit-reporter-v0.7.0
expect 0 "tag: jest-reporter-v0.6.0" "a Jest reporter tag that names its version passes" -- \
  bash "$verify" jest-reporter-v0.6.0
expect 1 "does not match reporters/jest/package.json version jest-reporter-v0.6.0" \
  "a Jest reporter tag does not take the CLI version" -- bash "$verify" jest-reporter-v0.7.0
expect 1 "is not a release tag" "a tag of no release fails" -- bash "$verify" nightly
manifests 0.7.0 "" 0.6.0
expect 1 "every version must be set" "a missing version fails" -- bash "$verify"
expect 0 "" "this checkout's versions are set" -- bash "$root/scripts/verify-versions.sh"

# ---- mirror-reporters.sh: deploy keys ----

mirror="$root/scripts/mirror-reporters.sh"
expect 1 "missing secret: JEST_REPORTER_DEPLOY_KEY \(write access.*reporter-mirrors environment" \
  "a missing key is named with its environment" -- bash "$mirror" jest-reporter-v0.1.0 --check-keys
expect 0 "deploy key: JEST_REPORTER_DEPLOY_KEY is set" "a Jest release needs only the Jest key" -- \
  env JEST_REPORTER_DEPLOY_KEY=y bash "$mirror" jest-reporter-v0.1.0 --check-keys
expect 1 "missing secret: EXUNIT_REPORTER_DEPLOY_KEY" "an ExUnit release needs the ExUnit key" -- \
  env JEST_REPORTER_DEPLOY_KEY=y bash "$mirror" exunit-reporter-v0.1.0 --check-keys
expect 1 "missing secret" "a GitHub push without its key fails before splitting" -- bash "$mirror" exunit-reporter-v0.1.0
expect 1 "missing secret" "a GitHub --verify without its key fails" -- bash "$mirror" exunit-reporter-v0.1.0 --verify
expect 2 "usage" "an unknown mode is a usage error" -- bash "$mirror" exunit-reporter-v0.1.0 --force
expect 2 "is not a reporter release tag" "a CLI tag mirrors nothing" -- bash "$mirror" v0.1.0 --check-keys

# ---- mirror-reporters.sh: local mirrors ----

git init --quiet --bare "$work/origin.git"
git clone --quiet "$work/origin.git" "$work/src" 2>/dev/null
mkdir -p "$work/mirrors/agoodway"
for name in exunit jest; do
  git init --quiet --bare "$work/mirrors/agoodway/goodflows-$name-reporter.git"
done
export MIRROR_URL_BASE="$work/mirrors/"
mirror_git() { git --git-dir "$work/mirrors/agoodway/goodflows-$1-reporter.git" "${@:2}"; }
has_tag() { mirror_git "$1" rev-parse -q --verify "refs/tags/$2" >/dev/null; }
lacks_tag() { ! has_tag "$@"; }
# release <tag> <reporter...>: commits a change to each named reporter (and the
# CLI), tags it on main, and pushes main.
release() {
  local tag="$1" name
  shift
  (
    cd "$work/src"
    for name in "$@"; do
      mkdir -p "reporters/$name"
      echo "$name $tag" >"reporters/$name/VERSION"
    done
    echo "$tag" >CLI_VERSION
    git add -A
    git commit --quiet -m "release $tag"
    git tag "$tag"
    git push --quiet origin main
  )
}
in_src() { (cd "$work/src" && "$@"); }

release exunit-reporter-v0.1.0 exunit jest
expect 0 "no main yet \(first release\)" "a first release pushes to an empty mirror" -- \
  in_src bash "$mirror" exunit-reporter-v0.1.0
check "the exunit mirror has v0.1.0" has_tag exunit v0.1.0
check "the exunit mirror has no prefixed tag" lacks_tag exunit exunit-reporter-v0.1.0
check "the exunit mirror main is the tagged split" \
  test "$(mirror_git exunit rev-parse main)" = "$(mirror_git exunit rev-parse 'v0.1.0^{commit}')"
check "the exunit mirror holds only its reporter's files" test "$(mirror_git exunit ls-tree --name-only main)" = VERSION
check "an ExUnit release leaves the Jest mirror empty" test -z "$(mirror_git jest for-each-ref)"
in_src git tag jest-reporter-v0.1.0
expect 0 "pushed main and v0.1.0" "the Jest reporter releases from the same commit on its own tag" -- \
  in_src bash "$mirror" jest-reporter-v0.1.0
check "the jest mirror holds only its reporter's files" test "$(mirror_git jest ls-tree --name-only main)" = VERSION

# A release that changes only the Jest reporter.
release jest-reporter-v0.2.0 jest
before="$(mirror_git jest rev-parse main)"
exunit_before="$(mirror_git exunit rev-parse main)"
expect 0 "dry run: nothing pushed" "--dry-run prints the split and pushes nothing" -- \
  in_src bash "$mirror" jest-reporter-v0.2.0 --dry-run
expect 0 "verified: agoodway/goodflows-jest-reporter can take v0.2.0; nothing pushed" \
  "--verify checks and pushes nothing" -- in_src bash "$mirror" jest-reporter-v0.2.0 --verify
check "--dry-run and --verify left main unchanged" test "$(mirror_git jest rev-parse main)" = "$before"
check "--dry-run and --verify pushed no tag" lacks_tag jest v0.2.0
expect 0 "is an ancestor of the split" "a later release fast-forwards" -- in_src bash "$mirror" jest-reporter-v0.2.0
check "the fast-forward kept the earlier main" mirror_git jest merge-base --is-ancestor "$before" main
check "the fast-forward moved main" test "$(mirror_git jest rev-parse main)" != "$before"
check "a Jest release leaves the ExUnit mirror unchanged" test "$(mirror_git exunit rev-parse main)" = "$exunit_before"
check "a Jest release tags no ExUnit version" lacks_tag exunit v0.2.0
expect 0 "pushed main and v0.2.0" "re-running a release that already pushed succeeds" -- \
  in_src bash "$mirror" jest-reporter-v0.2.0

# A commit made directly in the ExUnit mirror fails its next release before
# anything is pushed.
git clone --quiet "$work/mirrors/agoodway/goodflows-exunit-reporter.git" "$work/edit" 2>/dev/null
(cd "$work/edit" && echo edit >EDIT && git add EDIT && git commit --quiet -m "direct edit" && git push --quiet origin main)
release exunit-reporter-v0.3.0 exunit
expect 1 "goodflows-exunit-reporter main .* is not an ancestor of the split" \
  "a mirror with commits of its own fails the release" -- in_src bash "$mirror" exunit-reporter-v0.3.0
check "the edited mirror got no tag" lacks_tag exunit v0.3.0
# Undo the direct edit for the remaining cases.
(cd "$work/edit" && git reset --quiet --hard HEAD~1 && git push --quiet --force origin main)

# A tag on a branch that is not on main fails, in --verify too.
(
  cd "$work/src"
  git checkout --quiet -b maintenance
  echo hotfix >reporters/jest/HOTFIX
  git add -A
  git commit --quiet -m hotfix
  git tag jest-reporter-v0.2.1
)
expect 1 "jest-reporter-v0.2.1 .* is not on origin/main" "a tag that is not on main fails" -- \
  in_src bash "$mirror" jest-reporter-v0.2.1
expect 1 "is not on origin/main" "--verify refuses a tag that is not on main" -- \
  in_src bash "$mirror" jest-reporter-v0.2.1 --verify
expect 0 "warning: .* is not on origin/main" "--dry-run only warns about a branch off main" -- \
  in_src bash "$mirror" jest-reporter-v0.2.1 --dry-run
check "the off-main tag reached no mirror" lacks_tag jest v0.2.1

# A tag that does not name the checked-out commit fails.
in_src git checkout --quiet main
expect 1 "tag jest-reporter-v0.1.0 .* does not name the checked-out commit" "a tag on another commit fails" -- \
  in_src bash "$mirror" jest-reporter-v0.1.0
expect 1 "tag jest-reporter-v0.9.0 does not name the checked-out commit" "a tag that does not exist fails" -- \
  in_src bash "$mirror" jest-reporter-v0.9.0

# The source remote and branch are configurable.
expect 1 "cannot fetch main from nowhere" "an unreachable source remote fails" -- \
  in_src env MIRROR_SOURCE_REMOTE=nowhere bash "$mirror" exunit-reporter-v0.3.0 --verify

echo "$passed passed, $failed failed"
[ "$failed" -eq 0 ]
