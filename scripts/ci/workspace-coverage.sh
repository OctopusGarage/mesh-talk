#!/usr/bin/env bash
# Run from src-tauri (or a fixture workspace); preserve each platform's cohort.
set -e -o pipefail
usage() {
    echo "Usage: workspace-coverage.sh linux | macos | macos-host" >&2
    exit 2
}
case "${1:-}" in
    linux|macos|macos-host) platform="$1" ;;
    *) usage ;;
esac
[ "$#" = 1 ] || usage
if [ "$platform" = linux ]; then
    ulimit -c 262144
else
    # Hosted ARM has 3 CPUs/7 GiB. The complete four-job cohort accumulated
    # substantial compression/swap; bound compilation independently of tests.
    # Scope admission here, after cache restore; test workers stay at four.
    export CARGO_BUILD_JOBS=2
fi
run_phase() {
    local phase="$1"
    shift
    if [ "$platform" != linux ] && [ "$(uname -s)" = Darwin ]; then
        # Diagnostic snapshots are not acceptance gates. time retains the actual
        # command's exit status and reports CPU time and maximum child RSS;
        # that RSS is not the peak aggregate memory of concurrent compilers.
        echo "native-resource phase=$phase"
        /usr/sbin/sysctl hw.ncpu hw.memsize vm.swapusage || echo "native-resource sysctl unavailable"
        /usr/bin/vm_stat || echo "native-resource vm_stat unavailable"
        /usr/bin/time -l "$@"
    else
        "$@"
    fi
}
run_coverage() {
    # Finish every selected case in a partition. A failed Host rejects the
    # candidate before compiling or executing another partition.
    if
        run_phase host cargo llvm-cov nextest --no-report --locked -p mesh-talk --no-tests fail --no-fail-fast --retries 0 --user-config-file none --profile default --success-output immediate --test-threads 4
    then :; else return "$?"; fi
    if [ "$platform" = linux ]; then
        if
            run_phase remaining cargo llvm-cov nextest --no-report --locked --workspace --exclude mesh-talk --no-tests fail --no-fail-fast --retries 0 --user-config-file none --profile default --success-output immediate --test-threads 4
        then :; else return "$?"; fi
    elif [ "$platform" = macos ]; then
        # macOS instruments Host only; all remaining default targets still run.
        if
            run_phase remaining cargo nextest run --locked --workspace --exclude mesh-talk --no-tests fail --no-fail-fast --retries 0 --user-config-file none --profile default --success-output immediate --test-threads 4
        then :; else return "$?"; fi
    fi
    run_phase report cargo llvm-cov report --lcov --output-path lcov.info
}
if [ "${MESH_TALK_TEST_TIMING:-0}" = 1 ]; then
    run_coverage 2>&1 | tee "$RUNNER_TEMP/mesh-talk-test-timing.log"
else
    run_coverage
fi
