#!/usr/bin/env bash
set -euo pipefail

# Run from the repository root. Native runners build and test each target.
target="${1:?usage: package-release.sh TARGET TAG}"
tag="${2:?usage: package-release.sh TARGET TAG}"
case "$target" in
  x86_64-unknown-linux-gnu|aarch64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin) ;;
  *) echo "Unsupported release target: $target" >&2; exit 1 ;;
esac
version="$(python3 -c 'import tomllib; print(tomllib.load(open("Cargo.toml", "rb"))["package"]["version"])')"
if [ "$tag" != "v$version" ]; then
  echo "Release tag $tag must match Cargo.toml version v$version" >&2
  exit 1
fi

cargo build --release --locked --target "$target"
mkdir -p dist
staging="$(mktemp -d)"
trap 'rm -rf "$staging"' EXIT
cp "target/$target/release/goodci" README.md LICENSE "$staging/"
archive="goodci-$tag-$target.tar.gz"
tar -czf "dist/$archive" -C "$staging" goodci README.md LICENSE
(
  cd dist
  shasum -a 256 "$archive" > "$archive.sha256"
)
