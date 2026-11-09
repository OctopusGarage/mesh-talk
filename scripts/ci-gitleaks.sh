#!/usr/bin/env bash
# Pinned prebuilt scanner for the Linux x64 CI job. No source-build fallback.
set -euo pipefail
umask 077

readonly version='8.30.1'
readonly digest='551f6fc83ea457d62a0d98237cbad105af8d557003051f41f3e7ca7b3f2470eb'
readonly asset="gitleaks_${version}_linux_x64.tar.gz"
stage="$(mktemp -d "${TMPDIR:-/tmp}/mesh-gitleaks.XXXXXX")"
cleanup() {
    local status=$?
    trap - EXIT
    if ! rm -rf -- "$stage"; then
        printf '%s\n' 'Failed to remove owned Gitleaks staging' >&2
        if [[ "$status" == 0 ]]; then status=1; fi
    fi
    exit "$status"
}
trap cleanup EXIT

downloaded=false
for attempt in 1 2; do
    # A failed transfer may leave partial bytes. Never reuse them on retry.
    rm -f -- "$stage/$asset"
    if timeout --foreground --kill-after=5s 120s \
        gh release download "v${version}" --repo gitleaks/gitleaks \
        --pattern "$asset" --dir "$stage" --clobber; then
        downloaded=true
        break
    fi
    printf 'Gitleaks download attempt %s failed\n' "$attempt" >&2
done
if [[ "$downloaded" != true ]]; then exit 1; fi

archive="$stage/$asset"
if [[ ! -f "$archive" || -L "$archive" ]]; then
    printf '%s\n' 'Expected a regular Gitleaks archive' >&2
    exit 1
fi
# Integrity failure is terminal: never retry or extract different bytes.
printf '%s  %s\n' "$digest" "$archive" | sha256sum -c -
tar -xzf "$archive" -C "$stage" -- gitleaks
scanner="$stage/gitleaks"
if [[ ! -f "$scanner" || -L "$scanner" || ! -x "$scanner" ]]; then
    printf '%s\n' 'Expected a regular executable Gitleaks binary' >&2
    exit 1
fi
reported_version="$("$scanner" version)"
if [[ "$reported_version" != "$version" ]]; then
    printf '%s\n' 'Unexpected Gitleaks version' >&2
    exit 1
fi
"$scanner" git . --config .gitleaks.toml --redact -v
