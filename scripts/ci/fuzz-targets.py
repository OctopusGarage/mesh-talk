"""Run the complete manifest-owned cargo-fuzz inventory, failing closed."""
import os
from pathlib import Path
import re
import subprocess
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def expected_targets(manifest):
    with Path(manifest).open("rb") as source:
        bins = tomllib.load(source).get("bin", [])
    if not isinstance(bins, list) or not bins:
        raise ValueError("Fuzz manifest must declare a nonempty [[bin]] inventory")
    names = [entry.get("name") if isinstance(entry, dict) else None for entry in bins]
    if any(not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_][A-Za-z0-9_-]*", name) for name in names):
        raise ValueError("Fuzz manifest contains an absent or invalid target name")
    if len(set(names)) != len(names):
        raise ValueError("Fuzz manifest contains duplicate target names")
    return names


def run_fuzz(manifest, seconds, cargo=("cargo",)):
    if not isinstance(seconds, str) or not re.fullmatch(r"[1-9][0-9]*", seconds):
        raise ValueError("FUZZ_MAX_TOTAL_TIME must be a positive whole number of seconds")
    expected = expected_targets(manifest)
    discovered = subprocess.run([*cargo, "+nightly", "fuzz", "list"], check=True, capture_output=True, text=True)
    # Keep discovery diagnostics visible even when discovery returned success.
    if discovered.stderr:
        print(discovered.stderr, file=sys.stderr, end="")
    actual = discovered.stdout.splitlines()
    if len(actual) != len(set(actual)) or set(actual) != set(expected):
        raise ValueError(f"Fuzz inventory mismatch: expected={expected!r}, discovered={actual!r}")
    for target in expected:
        print(f"::group::fuzz {target} ({seconds}s)", flush=True)
        try:
            subprocess.run([*cargo, "+nightly", "fuzz", "run", "--target", "x86_64-unknown-linux-gnu", target, "--", f"-max_total_time={seconds}", "-rss_limit_mb=4096"], check=True)
        finally:
            print("::endgroup::", flush=True)


if __name__ == "__main__":
    try:
        run_fuzz(ROOT / "fuzz/Cargo.toml", os.environ.get("FUZZ_MAX_TOTAL_TIME", "120"))
    except subprocess.CalledProcessError as error:
        if error.stderr:
            print(error.stderr, file=sys.stderr, end="")
        print(f"Fuzz command failed: {error}", file=sys.stderr)
        sys.exit(1)
    except (ValueError, OSError) as error:
        print(f"Fuzz inventory rejected: {error}", file=sys.stderr)
        sys.exit(1)
