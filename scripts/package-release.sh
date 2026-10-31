#!/usr/bin/env bash
set -euo pipefail

# Invoke by path. The script finds the repository from its own location.
# Native runners build each target.
# Linux binaries are linked in Debian 12 so they run on Ampcode orbs.
# GLIBC_MAX is Debian 12's glibc. Keep it aligned with debian:bookworm-slim
# in scripts/debian12-builder.Dockerfile.
target="${1:?usage: package-release.sh TARGET TAG}"
tag="${2:?usage: package-release.sh TARGET TAG}"
glibc_max=2.36
rust_version=1.88.0
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin) ;;
  *) echo "Unsupported release target: $target" >&2; exit 1 ;;
esac

cd "$(dirname "$0")/.."
version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')"
if [ "$tag" != "v$version" ]; then
  echo "Release tag $tag must match Cargo.toml version v$version" >&2
  exit 1
fi

mkdir -p dist
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT

verify_glibc() {
  local bin=$1
  local limit=$2
  if ! command -v objdump >/dev/null 2>&1; then
    echo "objdump is required to verify the binary runs on glibc $limit" >&2
    exit 1
  fi
  python3 - "$bin" "$limit" <<'PY'
import re, subprocess, sys
path, limit_s = sys.argv[1], sys.argv[2]
limit = tuple(int(part) for part in limit_s.split("."))
text = subprocess.check_output(["objdump", "-T", path], text=True, errors="replace")
found = [
    tuple(int(part) for part in match.split("."))
    for match in re.findall(r"GLIBC_([0-9]+(?:\.[0-9]+)*)", text)
]
if not found:
    raise SystemExit(f"{path} has no glibc symbol versions")

def show(version):
    return ".".join(str(part) for part in version)

worst = max(found)
print(f"glibc requirement GLIBC_{show(worst)} (maximum GLIBC_{show(limit)})")
if worst > limit:
    raise SystemExit(
        f"{path} requires GLIBC_{show(worst)}; Ampcode orbs provide GLIBC_{show(limit)}"
    )
PY
}

build_linux() {
  local platform machine host image
  case "$target" in
    x86_64-unknown-linux-gnu) platform=linux/amd64; machine=x86_64 ;;
    aarch64-unknown-linux-gnu) platform=linux/arm64; machine=aarch64 ;;
  esac
  host="$(uname -m)"
  case "$host" in
    arm64) host=aarch64 ;;
  esac
  if [ "$host" != "$machine" ]; then
    echo "Build $target on a $machine host (this host is $(uname -m))." >&2
    exit 1
  fi
  if ! command -v docker >/dev/null 2>&1 || ! docker info >/dev/null 2>&1; then
    echo "Docker is required to link Linux release binaries on Debian 12." >&2
    exit 1
  fi
  image="goodflows-debian12-builder:${rust_version}"
  docker build \
    -f scripts/debian12-builder.Dockerfile \
    --build-arg "RUST_VERSION=${rust_version}" \
    -t "$image" \
    scripts
  docker run --rm -i \
    --platform "$platform" \
    -v "$PWD":/work:ro \
    -v "$staging":/out \
    -e "TARGET=${target}" \
    -e "HOST_UID=$(id -u)" \
    -e "HOST_GID=$(id -g)" \
    "$image" \
    bash -s <<'EOS'
set -euo pipefail
cargo build --release --locked \
  --manifest-path /work/Cargo.toml \
  --target "$TARGET" \
  --target-dir /tmp/target
install -m 755 "/tmp/target/$TARGET/release/goodflows" /out/goodflows
chown "$HOST_UID:$HOST_GID" /out/goodflows
EOS
  verify_glibc "$staging/goodflows" "$glibc_max"
}

case "$target" in
  *-unknown-linux-gnu) build_linux ;;
  *)
    cargo build --release --locked --target "$target"
    cp "target/$target/release/goodflows" "$staging/goodflows"
    ;;
esac

"$staging/goodflows" --version
cp README.md LICENSE "$staging/"
archive="goodflows-$tag-$target.tar.gz"
tar -czf "dist/$archive" -C "$staging" goodflows README.md LICENSE
(
  cd dist
  shasum -a 256 "$archive" > "$archive.sha256"
)
