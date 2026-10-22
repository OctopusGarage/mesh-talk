#!/usr/bin/env bash
# Extract only Linux relay binaries whose release ZIP hashes appear in verified evidence.
set -euo pipefail

input_dir="${1:?artifact directory required}"
evidence="${2:?verification evidence required}"
tag="${3:?release tag required}"
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-ci\.[0-9]+|-nightly\.[1-9][0-9]*\.[1-9][0-9]*)?$ ]]
test -f "$evidence" && test ! -L "$evidence"
test -d "$input_dir"

for platform in linux_x86_64 linux_aarch64; do
  archive="mesh-talk-post-office_${tag}_${platform}.zip"
  source="$input_dir/$archive"
  test -f "$source" && test ! -L "$source" && test -s "$source"
  digest="$(awk -v name="workflow-assets/$archive" '$2 == name { print $1 }' "$evidence")"
  [[ "$digest" =~ ^[a-f0-9]{64}$ ]]
  actual="$(sha256sum "$source" | cut -d ' ' -f 1)"
  if [[ "$actual" != "$digest" ]]; then
    echo "Verified release evidence does not match $archive" >&2
    exit 1
  fi
  if [[ "$platform" == linux_x86_64 ]]; then arch=amd64; else arch=arm64; fi
  destination="docker/post-office/binaries/$arch"
  mkdir -p "$destination"
  unzip -p "$source" release/mesh-talk-node > "$destination/mesh-talk-node"
  test -s "$destination/mesh-talk-node"
  chmod 755 "$destination/mesh-talk-node"
done
