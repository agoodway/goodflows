#!/usr/bin/env bash
# Publishes each bundled reporter to its read-only mirror for a release tag.
#
#   scripts/mirror-reporters.sh <tag> [--verify | --dry-run | --check-keys]
#
# For each reporter, `git subtree split` produces its history as a standalone
# branch (deterministic for the same history, so releases fast-forward). Every
# mirror is checked before anything is pushed: when its `main` exists it must be
# an ancestor of the new split, so commits made directly in a mirror fail the
# release instead of being overwritten. Then each mirror receives `main`
# (never forced) and the tag.
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
#   --check-keys  only check that both deploy keys are set.
#
# Deploy keys come from EXUNIT_REPORTER_DEPLOY_KEY and JEST_REPORTER_DEPLOY_KEY
# (private keys with write access to one mirror each, stored as secrets of the
# `reporter-mirrors` environment). A push or --verify fails before splitting
# anything when either key is missing, naming the missing secrets.
#
# SSH trusts only GitHub's published host keys (pinned below), never a key
# learned on first use. MIRROR_URL_BASE (default git@github.com:) replaces the
# remote prefix, for testing against local repositories, for example
# MIRROR_URL_BASE=/tmp/mirrors/ with bare repositories at
# /tmp/mirrors/agoodway/goodflows-{exunit,jest}-reporter.git; keys are then not
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

# prefix|mirror|deploy key variable
mirrors=(
  "reporters/exunit|agoodway/goodflows-exunit-reporter|EXUNIT_REPORTER_DEPLOY_KEY"
  "reporters/jest|agoodway/goodflows-jest-reporter|JEST_REPORTER_DEPLOY_KEY"
)

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

# Pushing to GitHub needs every mirror's deploy key; name each missing secret.
check_keys() {
  local missing=() entry prefix mirror var
  for entry in "${mirrors[@]}"; do
    IFS='|' read -r prefix mirror var <<<"$entry"
    [ -n "${!var:-}" ] || missing+=("$var (write access to $mirror)")
  done
  if [ "${#missing[@]}" -gt 0 ]; then
    local list
    list="$(printf '%s, ' "${missing[@]}")"
    fail "missing secret(s): ${list%, }. Add a write-enabled deploy key to each mirror and its private half as that secret of the reporter-mirrors environment in agoodway/goodflows. Nothing was pushed."
  fi
  echo "deploy keys: all set"
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

# Phase 1: split and check every mirror; push nothing yet.
declare -A splits
for entry in "${mirrors[@]}"; do
  IFS='|' read -r prefix mirror var <<<"$entry"
  name="${prefix##*/}"
  branch="mirror-split-$name"
  git branch -D "$branch" >/dev/null 2>&1 || true
  git subtree split --quiet --prefix "$prefix" -b "$branch" >/dev/null
  split="$(git rev-parse "$branch")"
  splits[$name]="$split"
  echo "$mirror: split $split"
  ssh="$(ssh_for "$var")"
  if [ -z "$ssh" ] && [ "$base" = "$github" ]; then
    if $dry_run; then
      echo "$mirror: secret $var is not set; skipping the mirror fetch"
      continue
    fi
    fail "secret $var is not set in the reporter-mirrors environment"
  fi
  url="$base$mirror.git"
  if ! heads="$(GIT_SSH_COMMAND="${ssh:-ssh}" git ls-remote --heads "$url" main)"; then
    fail "cannot reach $mirror; check that the deploy key in $var is added to that repository with write access"
  fi
  if [ -z "$heads" ]; then
    echo "$mirror: no main yet (first release)"
    continue
  fi
  GIT_SSH_COMMAND="${ssh:-ssh}" git fetch --quiet "$url" main
  current="$(git rev-parse FETCH_HEAD)"
  if git merge-base --is-ancestor "$current" "$split"; then
    echo "$mirror: main $current is an ancestor of the split"
  else
    fail "$mirror main $current is not an ancestor of the split; it was edited directly"
  fi
done

if $dry_run; then
  echo "dry run: nothing pushed"
  exit 0
fi
if [ "$mode" = "--verify" ]; then
  echo "verified: every mirror can take $tag; nothing pushed"
  exit 0
fi

# Phase 2: every mirror passed its checks, so push.
for entry in "${mirrors[@]}"; do
  IFS='|' read -r prefix mirror var <<<"$entry"
  name="${prefix##*/}"
  ssh="$(ssh_for "$var")"
  GIT_SSH_COMMAND="${ssh:-ssh}" git push "$base$mirror.git" \
    "${splits[$name]}:refs/heads/main" "${splits[$name]}:refs/tags/$tag"
  echo "$mirror: pushed main and $tag"
done
