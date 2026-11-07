#!/usr/bin/env bash
# Own manual installer collection, checksum roles and signed application ZIPs.
set -euo pipefail

collect() {
  : "${ARTIFACT_TAG:?release tag required}"
  mkdir -p release
  TAG="$ARTIFACT_TAG"

  for file in \
    target/*/release/bundle/nsis/*.exe \
    target/*/release/bundle/msi/*.msi \
    target/*/release/bundle/dmg/*.dmg \
    target/*/release/bundle/appimage/*.AppImage \
    target/*/release/bundle/deb/*.deb \
    target/*/release/bundle/rpm/*.rpm
  do
    [ -e "$file" ] || continue
    BASENAME=$(basename "$file")
    NEWNAME=$(echo "$BASENAME" | sed -E "s/_[0-9]+\.[0-9]+\.[0-9]+(-[a-zA-Z0-9.-]+)?/_${TAG}/")
    cp "$file" "release/$NEWNAME"
  done

  ls -lh release || true
}

checksums() {
  cd release
  shopt -s nullglob dotglob
  files=()
  updater_files=()
  for file in ./*; do
    if [[ -f "$file" && ! -L "$file" && "$file" != './SHA256SUMS' && "$file" != './UPDATER_SHA256SUMS' ]]; then
      case "$file" in
        ./mesh-talk-update_* | ./mesh-talk-update-raw_* | ./mesh-talk-patch_*) updater_files+=("$file") ;;
        *) files+=("$file") ;;
      esac
    fi
  done
  test "${#files[@]}" -gt 0

  write_checksums() {
    local manifest_name="$1"
    shift
    if command -v sha256sum >/dev/null 2>&1; then
      sha256sum "$@" > "$manifest_name"
      test -s "$manifest_name"
      sha256sum -c "$manifest_name"
    else
      shasum -a 256 "$@" > "$manifest_name"
      test -s "$manifest_name"
      shasum -a 256 -c "$manifest_name"
    fi
  }
  write_checksums SHA256SUMS "${files[@]}"
  if [[ "${#updater_files[@]}" -gt 0 ]]; then
    write_checksums UPDATER_SHA256SUMS "${updater_files[@]}"
  else
    rm -f UPDATER_SHA256SUMS
  fi

  echo "SHA256SUMS created:"
  cat SHA256SUMS

}

package() {
  : "${TARGET:?Rust target required}"
  : "${ARTIFACT_TAG:?release tag required}"
  TAG="$ARTIFACT_TAG"
  mkdir -p release_zips

  OS_NAME=""
  ARCH=""

  case "$TARGET" in
    x86_64-apple-darwin)
      OS_NAME="macos"
      ARCH="x86_64"
      ;;
    aarch64-apple-darwin)
      OS_NAME="macos"
      ARCH="arm64"
      ;;
    x86_64-pc-windows-msvc)
      OS_NAME="windows"
      ARCH="x86_64"
      ;;
    aarch64-pc-windows-msvc)
      OS_NAME="windows"
      ARCH="arm64"
      ;;
    *-linux-gnu | *-linux-musl)
      OS_NAME="linux"
      ARCH="${TARGET%%-*}"
      ;;
    *)
      echo "Unsupported target: $TARGET"
      exit 1
      ;;
  esac

  ZIP_NAME="release_zips/mesh-talk_${TAG}_${OS_NAME}_${ARCH}.zip"

  # Updaters share .exe/.AppImage suffixes with native installers.
  # Select by role before format so those bytes cannot enter the ZIP.
  installer_files=()
  for file in release/*; do
    [[ -f "$file" && ! -L "$file" ]] || continue
    case "$file" in
      release/mesh-talk-update_* | release/mesh-talk-update-raw_* | release/mesh-talk-patch_*) continue ;;
    esac
    case "$OS_NAME:$file" in
      macos:*.dmg | windows:*.exe | windows:*.msi | linux:*.AppImage | linux:*.deb | linux:*.rpm)
        installer_files+=("$file") ;;
    esac
  done
  test "${#installer_files[@]}" -gt 0
  # zip updates existing archives; start fresh to remove obsolete roles.
  rm -f "$ZIP_NAME"
  zip "$ZIP_NAME" "${installer_files[@]}" release/SHA256SUMS

  # UPDATER_SHA256SUMS inside the signed ZIP binds standalone updater roles.
  # Keep their payloads out of the manual installer download.
  shopt -s nullglob
  updater_files=(release/mesh-talk-update_* release/mesh-talk-update-raw_* release/mesh-talk-patch_*)
  if [[ "${#updater_files[@]}" -gt 0 ]]; then
    zip "$ZIP_NAME" release/UPDATER_SHA256SUMS
    cp "${updater_files[@]}" release_zips/
  fi

  BUNDLE_FILE="${ZIP_NAME}.bundle"
  node scripts/release/sign-blob.mjs "$ZIP_NAME" "$BUNDLE_FILE"

  echo "✅ Bundle signature generated: $BUNDLE_FILE"
  ls -lh release_zips
}

case "${1:-}" in
  collect|checksums|package) "$1" ;;
  *) echo "Usage: package-app.sh collect|checksums|package" >&2; exit 1 ;;
esac
