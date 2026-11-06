#!/usr/bin/env bash
# Run from src-tauri (or a fixture workspace); preserve each platform's cohort.
set -e -o pipefail
case "${1:-}" in
    linux|macos) platform="$1" ;;
    *) echo "Usage: workspace-coverage.sh linux|macos" >&2; exit 2 ;;
esac
if [ "$platform" = linux ]; then
    ulimit -c 262144
fi
run_coverage() {
    # Finish every selected case in a partition. A failed Host rejects the
    # candidate before compiling or executing another partition.
    if
        cargo llvm-cov nextest --no-report --locked -p mesh-talk --no-tests fail --no-fail-fast --retries 0 --user-config-file none --profile default --success-output immediate --test-threads 4
    then :; else return "$?"; fi
    if [ "$platform" = linux ]; then
        if
            cargo llvm-cov nextest --no-report --locked --workspace --exclude mesh-talk --no-tests fail --no-fail-fast --retries 0 --user-config-file none --profile default --success-output immediate --test-threads 4
        then :; else return "$?"; fi
    else
        # macOS instruments Host only; all remaining default targets still run.
        if
            cargo nextest run --locked --workspace --exclude mesh-talk --no-tests fail --no-fail-fast --retries 0 --user-config-file none --profile default --success-output immediate --test-threads 4
        then :; else return "$?"; fi
    fi
    cargo llvm-cov report --lcov --output-path lcov.info
}
if [ "${MESH_TALK_TEST_TIMING:-0}" = 1 ]; then
    run_coverage 2>&1 | tee "$RUNNER_TEMP/mesh-talk-test-timing.log"
else
    run_coverage
fi
