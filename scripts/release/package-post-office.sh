#!/usr/bin/env bash
# Create one self-contained, checksummed headless-relay release archive.
set -euo pipefail

target="${1:?Rust target required}"
tag="${2:?release tag required}"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-ci\.[0-9]+)?$ ]]

case "$target" in
  x86_64-apple-darwin) platform=macos_x86_64 ;;
  aarch64-apple-darwin) platform=macos_arm64 ;;
  x86_64-pc-windows-msvc) platform=windows_x86_64 ;;
  aarch64-pc-windows-msvc) platform=windows_arm64 ;;
  x86_64-unknown-linux-gnu) platform=linux_x86_64 ;;
  aarch64-unknown-linux-gnu) platform=linux_aarch64 ;;
  *) echo "Unsupported post-office target: $target" >&2; exit 1 ;;
esac

binary=mesh-talk-node
if [[ "$platform" == windows_* ]]; then binary=mesh-talk-node.exe; fi
source_binary="target/$target/release/$binary"
test -s "$source_binary"
test -s docs/post-office-release.md

mkdir -p release_zips
archive="$(pwd)/release_zips/mesh-talk-post-office_${tag}_${platform}.zip"
temporary="$(mktemp -d)"
trap 'rm -rf -- "$temporary"' EXIT
mkdir -p "$temporary/release"
cp "$source_binary" "$temporary/release/$binary"
sed "s@/blob/main/docs/@/blob/$tag/docs/@g" docs/post-office-release.md > "$temporary/release/README.md"
if [[ "$platform" == windows_* ]]; then
  cp scripts/install-post-office-windows.ps1 "$temporary/release/install-post-office-windows.ps1"
fi

(
  cd "$temporary/release"
  files=("$binary" README.md)
  if [[ "$platform" == windows_* ]]; then files+=(install-post-office-windows.ps1); fi
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "${files[@]}" > SHA256SUMS
    sha256sum -c SHA256SUMS >&2
  else
    shasum -a 256 "${files[@]}" > SHA256SUMS
    shasum -a 256 -c SHA256SUMS >&2
  fi
)
(
  cd "$temporary"
  zip -q "$archive" release/*
)
test -s "$archive"
printf '%s\n' "$archive"
