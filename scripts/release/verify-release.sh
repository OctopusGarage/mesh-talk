#!/usr/bin/env bash
# Verify signed archives without running application binaries or installing packages.
set -euo pipefail

asset_dir="${1:?asset directory required}"
tag="${2:?artifact tag required}"
version="${3:?application version required}"
repository="${4:?repository required}"
source_ref="${5:?source ref required}"
source_sha="${6:?source commit required}"

[[ "$repository" =~ ^[a-zA-Z0-9_.-]+/[a-zA-Z0-9_.-]+$ ]]
[[ "$tag" =~ ^v[0-9]+\.[0-9]+\.[0-9]+(-ci\.[0-9]+)?$ ]]
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]]
[[ "$source_sha" =~ ^[a-f0-9]{40}$ ]]
[[ "$source_ref" == refs/tags/* || "$source_ref" == refs/heads/* ]]

for platform in macos_arm64 macos_x86_64 windows_arm64 windows_x86_64 linux_aarch64 linux_x86_64; do
    archive="$asset_dir/mesh-talk_${tag}_${platform}.zip"
    test -f "$archive" && test ! -L "$archive" && test -s "$archive"
    test -f "$archive.bundle" && test ! -L "$archive.bundle" && test -s "$archive.bundle"
    cosign verify-blob --bundle "$archive.bundle" \
        --certificate-identity "https://github.com/$repository/.github/workflows/release.yml@$source_ref" \
        --certificate-oidc-issuer https://token.actions.githubusercontent.com "$archive"
    gh attestation verify "$archive" --repo "$repository" \
        --source-ref "$source_ref" --source-digest "$source_sha" \
        --signer-workflow "$repository/.github/workflows/release.yml" \
        --deny-self-hosted-runners
done

script_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
python3 -B "$script_dir/verify_assets.py" "$asset_dir" --tag "$tag" --version "$version"
