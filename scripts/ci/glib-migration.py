"""Require every resolved glib branch to complete the stable 0.20 migration.

RUSTSEC-2024-0429 is patched at >=0.20.0; seeing a newer parallel branch does
not remove the affected 0.18 branch. This migration watcher conservatively also
keeps pre-0.15 (advisory-unaffected) branches and prereleases gated.
https://rustsec.org/advisories/RUSTSEC-2024-0429.html
"""
import os
from pathlib import Path
import re
import sys
import tomllib

ROOT = Path(__file__).resolve().parents[2]


def probe_glib(lock):
    with Path(lock).open("rb") as source:
        packages = tomllib.load(source).get("package")
    if not isinstance(packages, list) or not all(isinstance(package, dict) for package in packages):
        raise ValueError("Cargo.lock must contain a package array")
    resolved = [package.get("version") for package in packages if package.get("name") == "glib"]
    if not all(isinstance(version, str) for version in resolved):
        raise ValueError("Resolved glib package has no valid version string")
    versions = sorted(set(resolved))
    stable = []
    for version in versions:
        match = re.fullmatch(r"(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(?:-([0-9A-Za-z.-]+))?(?:\+[0-9A-Za-z.-]+)?", version)
        if not match:
            raise ValueError(f"Invalid resolved glib version: {version!r}")
        stable.append(match[4] is None and tuple(map(int, match.group(1, 2, 3))) >= (0, 20, 0))
    return {"lifted": bool(versions) and all(stable), "versions": versions}


if __name__ == "__main__":
    try:
        result = probe_glib(ROOT / "Cargo.lock")
        versions = ", ".join(result["versions"]) or "none"
        print(f"Resolved glib branches: {versions}")
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            output.write(f"lifted={str(result['lifted']).lower()}\n")
            if result["lifted"]:
                output.write(f"glib_version={versions}\n")
        if not result["lifted"]:
            print("::notice::glib migration requires every resolved branch >=0.20.0 stable; no change.")
    except (ValueError, OSError, KeyError) as error:
        print(f"glib migration probe rejected: {error}", file=sys.stderr)
        sys.exit(1)
