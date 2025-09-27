#!/usr/bin/env bash
set -euo pipefail

# Usage: ./verify_release.sh <zip-file> <bundle-file> <release-dir> [identity-regex]
# default: identity-regex would match GitHub Actions workflow signature

ZIP_FILE="${1:-}"
BUNDLE_FILE="${2:-}"
RELEASE_DIR="${3:-}"
IDENTITY_REGEX="${4:-https://github.com/.*/.*/\.github/workflows/.*@refs/tags/.*}"
OIDC_ISSUER="https://token.actions.githubusercontent.com"

if [[ -z "$ZIP_FILE" || -z "$BUNDLE_FILE" || -z "$RELEASE_DIR" ]]; then
  echo "Usage: $0 <zip-file> <bundle-file> <release-dir> [identity-regex]"
  exit 1
fi

echo "🔹 Verifying Cosign signature..."
cosign verify-blob \
  --bundle "$BUNDLE_FILE" \
  --certificate-identity-regexp "$IDENTITY_REGEX" \
  --certificate-oidc-issuer "$OIDC_ISSUER" \
  "$ZIP_FILE"
echo "✅ Cosign signature verified!"

echo "🔹 Verifying SHA256 checksums in $RELEASE_DIR..."
if [[ ! -f "$RELEASE_DIR/SHA256SUMS" ]]; then
  echo "❌ SHA256SUMS file not found in $RELEASE_DIR"
  exit 1
fi

pushd "$RELEASE_DIR" > /dev/null
shasum -a 256 -c SHA256SUMS
popd > /dev/null
echo "✅ SHA256 verification passed!"

#  ./scripts/verify.sh \
#  ~/Downloads/mesh-talk_v0.0.2-dev-20250928.3_macos_arm64.zip \
# ~/Downloads/mesh-talk_v0.0.2-dev-20250928.3_macos_arm64.zip.bundle \
#  ~/Downloads/release/

# 🔹Verifying Cosign signature...
#Verified OK
#✅ Cosign signature verified!
#🔹 Verifying SHA256 checksums in /Users/kingsonwu/Downloads/release/...
#./mesh-talk_v0.0.2-dev-20250928.3_aarch64.dmg: OK
#✅ SHA256 verification passed!
