#!/usr/bin/env bash
# Publishes one bundled reporter to its read-only mirror for its release tag.
#
#   scripts/mirror-reporters.sh <tag> [--verify | --dry-run | --check-keys]
#
# The tag names the reporter and its version: `exunit-reporter-v<version>` or
# `jest-reporter-v<version>`. Each reporter is versioned on its own, apart from
# the CLI, and its mirror is tagged with the plain `v<version>`.
#
# `git subtree split` produces the reporter's history as a standalone branch
# (deterministic for the same history, so releases fast-forward). The mirror is
# checked before anything is pushed: when its `main` exists it must be an
# ancestor of the new split, so commits made directly in the mirror fail the
# release instead of being overwritten. Then the mirror receives `main` (never
# forced) and the tag.
#
# Releases are cut from main only: the tag must name the checked-out commit,
# and that commit must be on the source repository's main (fetched from
# MIRROR_SOURCE_REMOTE, default origin, branch MIRROR_SOURCE_BRANCH, default
# main). A tag on another branch fails, because the mirrors' main could not
# fast-forward to it later.
#
# Modes:
#   (none)        check everything, then push.
#   --verify      the same checks with the deploy keys, then stop: nothing is
#                 pushed. Release runs this before building and publishing.
#   --dry-run     print the split SHAs and ancestry results without pushing; a
#                 mirror is only fetched when its key is set, and the source
#                 checks only warn.
#   --check-keys  only check that the reporter's deploy key is set.
#
# Deploy keys come from EXUNIT_REPORTER_DEPLOY_KEY and JEST_REPORTER_DEPLOY_KEY
# (private keys with write access to one mirror each, stored as secrets of the
# `reporter-mirrors` environment). A push or --verify fails before splitting
# anything when the released reporter's key is missing, naming the secret.
#
# SSH trusts only GitHub's published host keys (pinned below), never a key
# learned on first use. MIRROR_URL_BASE (default git@github.com:) replaces the
# remote prefix, for testing against local repositories, for example
# MIRROR_URL_BASE=/tmp/mirrors/ with a bare repository at
# /tmp/mirrors/agoodway/goodflows-exunit-reporter.git; keys are then not
# required. scripts/test-release-scripts.sh tests every mode that way.
set -euo pipefail

usage="usage: mirror-reporters.sh <tag> [--verify | --dry-run | --check-keys]"
tag="${1:?$usage}"
mode="${2:-push}"
case "$mode" in
  push | --verify | --dry-run | --check-keys) ;;
  *)
    echo "$usage" >&2
    exit 2
    ;;
esac
dry_run=false
[ "$mode" = "--dry-run" ] && dry_run=true
github="git@github.com:"
base="${MIRROR_URL_BASE:-$github}"
source_remote="${MIRROR_SOURCE_REMOTE:-origin}"
source_branch="${MIRROR_SOURCE_BRANCH:-main}"

case "$tag" in
  exunit-reporter-v[0-9]*)
    prefix=reporters/exunit mirror=agoodway/goodflows-exunit-reporter var=EXUNIT_REPORTER_DEPLOY_KEY
    ;;
  jest-reporter-v[0-9]*)
    prefix=reporters/jest mirror=agoodway/goodflows-jest-reporter var=JEST_REPORTER_DEPLOY_KEY
    ;;
  *)
    echo "error: $tag is not a reporter release tag (exunit-reporter-v<version> or jest-reporter-v<version>)" >&2
    exit 2
    ;;
esac
mirror_tag="v${tag#*-reporter-v}"

# Prints an error, as a workflow annotation too when running in GitHub Actions.
# Inside a dry run's source check (warn_only set) it is a warning instead.
fail() {
  local level="${warn_only:+warning}"
  level="${level:-error}"
  echo "$level: $1" >&2
  if [ "${GITHUB_ACTIONS:-}" = true ]; then
    echo "::$level title=Reporter mirrors::$1"
  fi
  exit 1
}

# Pushing to GitHub needs the mirror's deploy key.
check_keys() {
  if [ -z "${!var:-}" ]; then
    fail "missing secret: $var (write access to $mirror). Add a write-enabled deploy key to the mirror and its private half as that secret of the reporter-mirrors environment in agoodway/goodflows. Nothing was pushed."
  fi
  echo "deploy key: $var is set"
}

if [ "$mode" = "--check-keys" ]; then
  check_keys
  exit 0
fi
if ! $dry_run && [ "$base" = "$github" ]; then
  check_keys
fi

# The tag must name HEAD, and HEAD must be on the source main. A dry run only
# warns, since it may run on a branch or before the tag exists.
check_source() {
  local head tagged main
  head="$(git rev-parse HEAD)"
  if ! git fetch --quiet "$source_remote" "$source_branch"; then
    fail "cannot fetch $source_branch from $source_remote to check that $tag is on it"
  fi
  main="$(git rev-parse FETCH_HEAD)"
  if ! git merge-base --is-ancestor "$head" "$main"; then
    fail "$tag ($head) is not on $source_remote/$source_branch; releases are tagged on $source_branch only (a fix for an older release ships as the next version from $source_branch). Nothing was pushed."
  fi
  if ! $dry_run; then
    tagged="$(git rev-parse -q --verify "refs/tags/$tag^{commit}" || true)"
    if [ "$tagged" != "$head" ]; then
      fail "tag $tag ${tagged:+($tagged) }does not name the checked-out commit $head. Nothing was pushed."
    fi
  fi
  echo "source: $head is on $source_remote/$source_branch"
}

if $dry_run; then
  (warn_only=1 && check_source) || echo "dry run: the source check above would fail a release"
else
  check_source
fi

keys="$(mktemp -d)"
trap 'rm -rf "$keys"' EXIT

# GitHub's published SSH host keys:
# https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/githubs-ssh-key-fingerprints
cat >"$keys/known_hosts" <<'HOSTS'
github.com ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIOMqqnkVzrm0SdG6UOoqKLsabgH5C9okWi0dh2l9GKJl
github.com ecdsa-sha2-nistp256 AAAAE2VjZHNhLXNoYTItbmlzdHAyNTYAAAAIbmlzdHAyNTYAAABBBEmKSENjQEezOmxkZMy7opKgwFB9nkt5YRrYMjNuG5N87uRgg6CLrbo5wAdT/y6v0mKV0U2w0WZ2YB/++Tpockg=
github.com ssh-rsa AAAAB3NzaC1yc2EAAAADAQABAAABgQCj7ndNxQowgcQnjshcLrqPEiiphnt+VTTvDP6mHBL9j1aNUkY4Ue1gvwnGLVlOhGeYrnZaMgRK6+PKCUXaDbC7qtbW8gIkhL7aGCsOr/C56SJMy/BCZfxd1nWzAOxSDPgVsmerOBYfNqltV9/hWCqBywINIR+5dIg6JTJ72pcEpEjcYgXkE2YEFXV1JHnsKgbLWNlhScqb2UmyRkQyytRLtL+38TGxkxCflmO+5Z8CSSNY7GidjMIZ7Q4zMjA2n1nGrlTDkzwDCsw+wqFPGQA179cnfGWOWRVruj16z6XyvxvjJwbz0wQZ75XK5tKSb7FNyeIEs4TT4jk+S4dhPeAUC5y+bDYirYgM4GC7uEnztnZyaVWQ7B381AK4Qdrwt51ZqExKbQpTUNn+EjqoTwvqNj4kqx5QUCI0ThS/YkOxJCXmPUWZbhjpCg56i+2aB6CmK2JGhn57K5mj0MNdBXA4/WnwH6XoPWJzK5Nyu2zB3nAZp+S5hpQs+p1vN1/wsjk=
HOSTS

ssh_for() {
  local var="$1" key="$keys/$1"
  if [ -n "${!var:-}" ] && [ ! -f "$key" ]; then
    printf '%s\n' "${!var}" >"$key"
    chmod 600 "$key"
  fi
  if [ -f "$key" ]; then
    echo "ssh -i $key -o IdentitiesOnly=yes -o StrictHostKeyChecking=yes -o UserKnownHostsFile=$keys/known_hosts"
  fi
}

# Split and check the mirror; push nothing yet.
branch="mirror-split-${prefix##*/}"
git branch -D "$branch" >/dev/null 2>&1 || true
git subtree split --quiet --prefix "$prefix" -b "$branch" >/dev/null
split="$(git rev-parse "$branch")"
echo "$mirror: split $split for $mirror_tag"
ssh="$(ssh_for "$var")"
url="$base$mirror.git"
if [ -z "$ssh" ] && [ "$base" = "$github" ]; then
  if $dry_run; then
    echo "$mirror: secret $var is not set; skipping the mirror fetch"
    echo "dry run: nothing pushed"
    exit 0
  fi
  fail "secret $var is not set in the reporter-mirrors environment"
fi
if ! heads="$(GIT_SSH_COMMAND="${ssh:-ssh}" git ls-remote --heads "$url" main)"; then
  fail "cannot reach $mirror; check that the deploy key in $var is added to that repository with write access"
fi
if [ -z "$heads" ]; then
  echo "$mirror: no main yet (first release)"
else
  GIT_SSH_COMMAND="${ssh:-ssh}" git fetch --quiet "$url" main
  current="$(git rev-parse FETCH_HEAD)"
  if git merge-base --is-ancestor "$current" "$split"; then
    echo "$mirror: main $current is an ancestor of the split"
  else
    fail "$mirror main $current is not an ancestor of the split; it was edited directly"
  fi
fi

if $dry_run; then
  echo "dry run: nothing pushed"
  exit 0
fi
if [ "$mode" = "--verify" ]; then
  echo "verified: $mirror can take $mirror_tag; nothing pushed"
  exit 0
fi

GIT_SSH_COMMAND="${ssh:-ssh}" git push "$url" "$split:refs/heads/main" "$split:refs/tags/$mirror_tag"
echo "$mirror: pushed main and $mirror_tag"
