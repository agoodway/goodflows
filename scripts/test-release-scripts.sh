#!/usr/bin/env bash
# Tests scripts/verify-versions.sh and scripts/mirror-reporters.sh without
# GitHub: versions are checked in a scratch copy of the three manifests, and
# mirrors are local bare repositories reached through MIRROR_URL_BASE, with a
# local bare repository as the source `origin`. Covers version drift, a tag
# that does not match the version, missing deploy keys, a first release, a
# fast-forward release, --verify and --dry-run (which push nothing), a mirror
# with commits of its own, and tags that are not on main or not on HEAD.
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

manifests 0.6.0 0.6.0 0.6.0
expect 0 "Cargo.toml: 0.6.0" "versions agree without a tag" -- bash "$verify"
expect 0 "tag: v0.6.0" "a tag that names the version passes" -- bash "$verify" v0.6.0
expect 1 "tag v0.7.0 does not match version v0.6.0" "a tag that does not match the version fails" -- bash "$verify" v0.7.0
manifests 0.6.0 0.6.0 0.5.0
expect 1 "reporters/jest/package.json" "Jest reporter version drift fails and names the files to bump" -- bash "$verify"
manifests 0.6.0 0.5.0 0.6.0
expect 1 "must agree" "ExUnit reporter version drift fails" -- bash "$verify" v0.6.0
expect 0 "" "this checkout's versions agree" -- bash "$root/scripts/verify-versions.sh"

# ---- mirror-reporters.sh: deploy keys ----

mirror="$root/scripts/mirror-reporters.sh"
expect 1 "EXUNIT_REPORTER_DEPLOY_KEY.*JEST_REPORTER_DEPLOY_KEY.*reporter-mirror-deploy-keys" \
  "missing keys are named with the setup docs" -- bash "$mirror" v0.1.0 --check-keys
expect 1 "missing secret\(s\): JEST_REPORTER_DEPLOY_KEY \(write access" "only the missing key is named" -- \
  env EXUNIT_REPORTER_DEPLOY_KEY=x bash "$mirror" v0.1.0 --check-keys
expect 0 "deploy keys: all set" "both keys set passes the key check" -- \
  env EXUNIT_REPORTER_DEPLOY_KEY=x JEST_REPORTER_DEPLOY_KEY=y bash "$mirror" v0.1.0 --check-keys
expect 1 "missing secret" "a GitHub push without keys fails before splitting" -- bash "$mirror" v0.1.0
expect 1 "missing secret" "a GitHub --verify without keys fails" -- bash "$mirror" v0.1.0 --verify
expect 2 "usage" "an unknown mode is a usage error" -- bash "$mirror" v0.1.0 --force

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
# release <tag>: commits a change to both reporters and tags it on main.
release() {
  (
    cd "$work/src"
    mkdir -p reporters/exunit reporters/jest
    echo "exunit $1" >reporters/exunit/VERSION
    echo "jest $1" >reporters/jest/VERSION
    echo "$1" >CLI_VERSION
    git add -A
    git commit --quiet -m "release $1"
    git tag "$1"
    git push --quiet origin main
  )
}
in_src() { (cd "$work/src" && "$@"); }

release v0.1.0
expect 0 "no main yet \(first release\)" "a first release pushes to empty mirrors" -- in_src bash "$mirror" v0.1.0
for name in exunit jest; do
  check "$name mirror has v0.1.0" has_tag "$name" v0.1.0
  check "$name mirror main is the tagged split" test "$(mirror_git "$name" rev-parse main)" = "$(mirror_git "$name" rev-parse 'v0.1.0^{commit}')"
  check "$name mirror holds only its reporter's files" \
    test "$(mirror_git "$name" ls-tree --name-only main)" = VERSION
done

release v0.2.0
before="$(mirror_git jest rev-parse main)"
expect 0 "dry run: nothing pushed" "--dry-run prints the splits and pushes nothing" -- in_src bash "$mirror" v0.2.0 --dry-run
expect 0 "verified: every mirror can take v0.2.0; nothing pushed" "--verify checks and pushes nothing" -- \
  in_src bash "$mirror" v0.2.0 --verify
check "--dry-run and --verify left main unchanged" test "$(mirror_git jest rev-parse main)" = "$before"
check "--dry-run and --verify pushed no tag" lacks_tag jest v0.2.0
expect 0 "is an ancestor of the split" "a later release fast-forwards" -- in_src bash "$mirror" v0.2.0
check "the fast-forward kept the earlier main" mirror_git jest merge-base --is-ancestor "$before" main
check "the fast-forward moved main" test "$(mirror_git jest rev-parse main)" != "$before"
expect 0 "pushed main and v0.2.0" "re-running a release that already pushed succeeds" -- in_src bash "$mirror" v0.2.0

# A commit made directly in the ExUnit mirror fails the next release before
# either mirror is pushed.
git clone --quiet "$work/mirrors/agoodway/goodflows-exunit-reporter.git" "$work/edit" 2>/dev/null
(cd "$work/edit" && echo edit >EDIT && git add EDIT && git commit --quiet -m "direct edit" && git push --quiet origin main)
release v0.3.0
jest_before="$(mirror_git jest rev-parse main)"
expect 1 "goodflows-exunit-reporter main .* is not an ancestor of the split" \
  "a mirror with commits of its own fails the release" -- in_src bash "$mirror" v0.3.0
check "the other mirror was not pushed either" test "$(mirror_git jest rev-parse main)" = "$jest_before"
check "no mirror got the failed tag" lacks_tag jest v0.3.0
# Undo the direct edit for the remaining cases.
(cd "$work/edit" && git reset --quiet --hard HEAD~1 && git push --quiet --force origin main)

# A tag on a branch that is not on main fails, in --verify too.
(
  cd "$work/src"
  git checkout --quiet -b maintenance
  echo hotfix >reporters/jest/HOTFIX
  git add -A
  git commit --quiet -m hotfix
  git tag v0.2.1
)
expect 1 "v0.2.1 .* is not on origin/main" "a tag that is not on main fails" -- in_src bash "$mirror" v0.2.1
expect 1 "is not on origin/main" "--verify refuses a tag that is not on main" -- in_src bash "$mirror" v0.2.1 --verify
expect 0 "warning: .* is not on origin/main" "--dry-run only warns about a branch off main" -- \
  in_src bash "$mirror" v0.2.1 --dry-run
check "the off-main tag reached no mirror" lacks_tag jest v0.2.1

# A tag that does not name the checked-out commit fails.
in_src git checkout --quiet main
expect 1 "tag v0.1.0 .* does not name the checked-out commit" "a tag on another commit fails" -- \
  in_src bash "$mirror" v0.1.0
expect 1 "tag v0.9.0 does not name the checked-out commit" "a tag that does not exist fails" -- \
  in_src bash "$mirror" v0.9.0

# The source remote and branch are configurable.
expect 1 "cannot fetch main from nowhere" "an unreachable source remote fails" -- \
  in_src env MIRROR_SOURCE_REMOTE=nowhere bash "$mirror" v0.3.0 --verify

echo "$passed passed, $failed failed"
[ "$failed" -eq 0 ]
