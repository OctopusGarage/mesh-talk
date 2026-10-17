#!/usr/bin/env python3
"""Coordinate local builds, retain release sets, and bound idle Cargo caches."""

import argparse
from contextlib import ExitStack, contextmanager
from datetime import datetime, timezone
import errno
import json
import hashlib
import math
import os
from pathlib import Path
import re
import shutil
import stat
import subprocess
import sys
import time
import tomllib
import uuid


REPOSITORY = "https://github.com/OctopusGarage/mesh-talk"
CACHE_NAMES = ("deps", ".fingerprint", "build", "incremental", "examples")


def git(root, *args):
    return (
        subprocess.check_output(
            ["git", "-C", str(root), *args], stderr=subprocess.DEVNULL
        )
        .decode()
        .strip()
    )


def project(root):
    """Require project metadata before considering any checkout for deletion."""
    try:
        return (
            REPOSITORY in (root / "Cargo.toml").read_text()
            and 'name = "mesh-talk"' in (root / "src-tauri/Cargo.toml").read_text()
            and (root / "frontend").is_dir()
        )
    except OSError:
        return False


def safe_directory(path, root):
    """Reject symlinks at every level, including the target directory itself."""
    try:
        relative = path.relative_to(root)
        current = root
        for part in relative.parts:
            current = current / part
            if current.is_symlink():
                return False
        return path.is_dir()
    except ValueError:
        return False


@contextmanager
def lock(state, name="builds.lock", shared=False, blocking=True):
    """OS locks release on process exit; Windows conservatively serializes builds."""
    state.mkdir(parents=True, exist_ok=True)
    with (state / name).open("a+b") as handle:
        acquired = False
        try:
            if os.name == "nt":
                import msvcrt

                # Windows permits byte-range locks beyond EOF. Do not read or
                # write a byte another Cargo process may already have locked.
                while True:
                    try:
                        handle.seek(0)
                        msvcrt.locking(handle.fileno(), msvcrt.LK_NBLCK, 1)
                        break
                    except OSError as error:
                        if error.errno not in {
                            errno.EACCES,
                            errno.EAGAIN,
                            errno.EDEADLK,
                        }:
                            raise
                        if not blocking:
                            raise BlockingIOError("active build") from error
                        time.sleep(0.1)
            else:
                import fcntl

                mode = fcntl.LOCK_SH if shared else fcntl.LOCK_EX
                fcntl.flock(handle, mode | (0 if blocking else fcntl.LOCK_NB))
            acquired = True
            yield
        finally:
            if acquired:
                if os.name == "nt":
                    handle.seek(0)
                    msvcrt.locking(handle.fileno(), msvcrt.LK_UNLCK, 1)
                else:
                    fcntl.flock(handle, fcntl.LOCK_UN)


def worktree_lock_name(root):
    return hashlib.sha256(os.fsencode(root)).hexdigest() + ".lock"


@contextmanager
def cargo_profile_locks(profile, root):
    """Coordinate with raw Cargo; preserve profile directories and lock inodes."""
    if not safe_directory(profile, root):
        raise ValueError(f"Unsafe Cargo profile: {profile}")
    with ExitStack() as stack:
        for name in (".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"):
            path = profile / name
            if path.is_symlink():
                raise ValueError(f"Symlinked Cargo lock: {path}")
            stack.enter_context(lock(profile, name=name, blocking=False))
        yield


def checkouts(root, extra):
    roots = {root}
    for candidate in [root, *extra]:
        candidate = Path(candidate).expanduser().resolve()
        if not project(candidate):
            print(f"Skipping unverified checkout: {candidate}")
            continue
        roots.add(candidate)
        # NUL-delimited worktree records preserve spaces and other unusual paths.
        output = subprocess.check_output(
            ["git", "-C", str(candidate), "worktree", "list", "--porcelain", "-z"]
        )
        for field in output.split(b"\0"):
            if field.startswith(b"worktree "):
                path = Path(os.fsdecode(field[9:])).resolve()
                if project(path):
                    roots.add(path)
    return sorted(roots)


def tree_stats(path):
    """One stat per entry; tolerate files replaced by a concurrent busy build."""
    size = 0
    newest = 0
    seen = set()
    pending = [str(path)]
    initial = path.lstat()

    def account(info):
        nonlocal size, newest
        newest = max(newest, info.st_mtime)
        inode = (info.st_dev, info.st_ino)
        if inode not in seen:
            size += info.st_blocks * 512 if hasattr(info, "st_blocks") else info.st_size
            seen.add(inode)

    account(initial)
    while pending:
        directory = pending.pop()
        try:
            with os.scandir(directory) as entries:
                for entry in entries:
                    try:
                        info = entry.stat(follow_symlinks=False)
                    except FileNotFoundError:
                        continue
                    account(info)
                    if stat.S_ISDIR(info.st_mode):
                        pending.append(entry.path)
        except FileNotFoundError:
            continue
    return size, newest


def cache_groups(root):
    target = root / "target"
    if not safe_directory(target, root):
        return []
    profiles = []
    containers = [target]
    coverage = target / "llvm-cov-target"
    if safe_directory(coverage, root):
        containers.append(coverage)
    for container in containers:
        profiles.extend([container / "debug", container / "release"])
        for child in container.iterdir():
            if safe_directory(child, root) and child.name not in {
                "debug",
                "release",
                "llvm-cov-target",
            }:
                profiles.extend([child / "debug", child / "release"])
    groups = []
    for profile in profiles:
        if safe_directory(profile, root):
            groups.extend(
                profile / name
                for name in CACHE_NAMES
                if safe_directory(profile / name, root)
            )
    return groups


def log_removal(state, root, paths, size, kind):
    with (state / "cleanup.jsonl").open("a") as handle:
        handle.write(
            json.dumps(
                {
                    "time": datetime.now(timezone.utc).isoformat(),
                    "root": str(root),
                    "paths": [str(path) for path in paths],
                    "bytes": size,
                    "kind": kind,
                }
            )
            + "\n"
        )


def prune_packages(state, keep, dry_run):
    directory = state / "packages"
    if not safe_directory(directory, state):
        return
    sets = []
    for entry in directory.iterdir():
        if not safe_directory(entry, state):
            continue
        if re.fullmatch(r"\.partial-\d{20}-[a-f0-9]{8}", entry.name):
            print(f"{'DRY-RUN: ' if dry_run else ''}Remove incomplete archive: {entry}")
            if not dry_run:
                size, _ = tree_stats(entry)
                log_removal(state, state, [entry], size, "partial-package")
                shutil.rmtree(entry)
            continue
        manifest = entry / "manifest.json"
        if manifest.is_file() and not manifest.is_symlink():
            data = json.loads(manifest.read_text())
            if data.get("schema") == 1 and data.get("id") == entry.name:
                sets.append(entry)
    for entry in sorted(sets, reverse=True)[keep:]:
        print(f"{'DRY-RUN: ' if dry_run else ''}Remove archived build: {entry}")
        if not dry_run:
            size, _ = tree_stats(entry)
            log_removal(state, state, [entry], size, "package")
            shutil.rmtree(entry)


def clean(args, state):
    if os.environ.get("MT_TARGET_CLEAN") == "0":
        print("Build cleanup disabled by MT_TARGET_CLEAN=0")
        return
    try:
        with lock(state, name="cleanup.lock", blocking=False):
            registry = state / "checkouts.json"
            registered = json.loads(registry.read_text()) if registry.exists() else []
            roots = checkouts(args.root, [*args.checkout, *registered])
            if not args.dry_run:
                configured = sorted(
                    {
                        str(path.expanduser().resolve())
                        for path in [
                            args.root,
                            *args.checkout,
                            *(Path(item) for item in registered),
                        ]
                        if project(path.expanduser().resolve())
                    }
                )
                temporary = state / "checkouts.tmp"
                temporary.write_text(json.dumps(configured, indent=2) + "\n")
                temporary.replace(registry)
            try:
                with lock(state, name="packages.lock", blocking=False):
                    prune_packages(state, args.keep, args.dry_run)
            except BlockingIOError:
                print("Package capture is active; deferring archive retention only.")
            stamp = state / "last-cache-scan"
            if (
                args.action == "run"
                and stamp.exists()
                and time.time() - stamp.stat().st_mtime < args.interval_seconds
            ):
                print(
                    "Compiler cache scanned recently; package retention checked, cache scan skipped."
                )
                return
            candidates = []
            total = 0
            for root in roots:
                target = root / "target"
                if not safe_directory(target, root):
                    continue
                size, newest = tree_stats(target)
                total += size
                marker = target / ".mesh-talk-build-use"
                if marker.is_file():
                    newest = max(newest, marker.stat().st_mtime)
                candidates.append((newest, str(root)))
            print(
                f"Cargo targets: {total / 2**30:.2f} GiB; budget: {args.budget_gb:g} GiB"
            )
            for _, name in sorted(candidates):
                if total <= args.budget_gb * 2**30:
                    break
                root = Path(name)
                try:
                    with lock(state, name=worktree_lock_name(root), blocking=False):
                        if not project(root):
                            continue
                        profiles = {}
                        for cache in cache_groups(root):
                            profiles.setdefault(cache.parent, []).append(cache)
                        for profile, paths in profiles.items():
                            if total <= args.budget_gb * 2**30:
                                break
                            try:
                                with cargo_profile_locks(profile, root):
                                    if not all(
                                        safe_directory(path, root) for path in paths
                                    ):
                                        raise ValueError(
                                            f"Cache paths changed: {profile}"
                                        )
                                    before, _ = tree_stats(profile)
                                    reclaimable = sum(
                                        tree_stats(path)[0] for path in paths
                                    )
                                    for cache in paths:
                                        print(
                                            f"{'DRY-RUN: ' if args.dry_run else ''}Remove compiler cache: {cache}",
                                            flush=True,
                                        )
                                    if args.dry_run:
                                        total -= reclaimable
                                    else:
                                        log_removal(
                                            state, root, paths, reclaimable, "cache"
                                        )
                                        for cache in paths:
                                            shutil.rmtree(cache)
                                        # Account for final binaries hardlinked to deps.
                                        total -= before - tree_stats(profile)[0]
                            except BlockingIOError:
                                print(
                                    f"Keeping active Cargo profile: {profile}",
                                    flush=True,
                                )
                            except (OSError, ValueError) as error:
                                print(
                                    f"Keeping unverified Cargo profile {profile}: {error}",
                                    file=sys.stderr,
                                )
                except BlockingIOError:
                    print(f"Keeping active managed worktree: {root}", flush=True)
            if not args.dry_run:
                # Busy worktrees can grow during inspection. Report fresh totals,
                # and don't throttle the next attempt if we're still over budget.
                total = sum(
                    tree_stats(root / "target")[0]
                    for root in roots
                    if safe_directory(root / "target", root)
                )
                if total <= args.budget_gb * 2**30:
                    stamp.touch()
                elif stamp.exists():
                    stamp.unlink()
            print(
                f"{'Estimated remaining' if args.dry_run else 'Remaining'} Cargo targets: {total / 2**30:.2f} GiB"
            )
            if total > args.budget_gb * 2**30:
                print(
                    "Budget still exceeded: active, preserved, or unverified outputs remain; scheduled cleanup will retry."
                )
    except BlockingIOError:
        print("Another cleanup is active; skipping this attempt.")


def custom_cargo_output(root, command):
    if any(
        os.environ.get(key)
        for key in (
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_TARGET",
            "CARGO_BUILD_BUILD_DIR",
        )
    ):
        return True
    if any(
        arg.split("=")[0] in {"--target-dir", "--target", "--profile", "--config"}
        for arg in command
    ):
        return True
    directories = [root, *root.parents]
    configs = [
        directory / ".cargo" / name
        for directory in directories
        for name in ("config", "config.toml")
    ]
    cargo_home = Path(os.environ.get("CARGO_HOME", Path.home() / ".cargo"))
    configs.extend(cargo_home / name for name in ("config", "config.toml"))
    for config in configs:
        if config.is_file():
            build = tomllib.loads(config.read_text()).get("build", {})
            if any(key in build for key in ("target-dir", "target", "build-dir")):
                return True
    return False


def archive(root, state, mode, started):
    target = root / "target"
    if not safe_directory(target, root):
        raise ValueError("Release target is missing or symlinked; archive skipped")
    release = target / "release"
    names = ["mesh-talk", "mesh-talk.exe"]
    if mode == "binary":
        names.extend(["mesh-talk-node", "mesh-talk-node.exe"])
    paths = [release / name for name in names]
    outputs = [
        (path, Path(path.name))
        for path in paths
        if path.is_file() and not path.is_symlink()
    ]
    if mode == "bundle":
        bundle = release / "bundle"
        if not safe_directory(bundle, root):
            raise ValueError("No release bundle found; preserving existing archives")
        bundles = []
        for directory, dirs, files in os.walk(bundle, followlinks=False):
            parent = Path(directory)
            for name in list(dirs):
                path = parent / name
                if path.suffix == ".app":
                    dirs.remove(name)
                    if safe_directory(path, root) and tree_stats(path)[1] >= started:
                        bundles.append(path)
            bundles.extend(
                parent / name
                for name in files
                if not (parent / name).is_symlink()
                and (parent / name).stat().st_mtime >= started
            )
        if not bundles:
            raise ValueError(
                "No newly generated bundles found; preserving existing archives"
            )
        outputs.extend(
            (path, Path("bundle") / path.relative_to(bundle)) for path in bundles
        )
    if not outputs:
        raise ValueError("No release binaries found; preserving existing archives")
    directory = state / "packages"
    directory.mkdir(parents=True, exist_ok=True)
    if not safe_directory(directory, state):
        raise ValueError("Archive directory must not be a symlink")
    identity = f"{time.time_ns():020d}-{uuid.uuid4().hex[:8]}"
    staging = directory / f".partial-{identity}"
    staging.mkdir()
    try:
        for path, relative in outputs:
            if not safe_directory(path.parent, root):
                raise ValueError(f"Unsafe release path: {path}")
            destination = staging / relative
            destination.parent.mkdir(parents=True, exist_ok=True)
            if path.is_dir():
                shutil.copytree(path, destination, symlinks=True)
            else:
                shutil.copy2(path, destination)
        manifest = {
            "schema": 1,
            "id": identity,
            "commit": git(root, "rev-parse", "HEAD"),
            "branch": git(root, "rev-parse", "--abbrev-ref", "HEAD"),
            "dirty": bool(git(root, "status", "--porcelain")),
            "root": str(root),
            "created": datetime.now(timezone.utc).isoformat(),
        }
        (staging / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        staging.rename(directory / identity)
        print(f"Archived successful build: {directory / identity}")
    finally:
        if staging.exists():
            shutil.rmtree(staging)


def run_build(args, state):
    command = args.command
    if command[:1] == ["--"]:
        command = command[1:]
    if not command:
        raise ValueError("run requires a command after --")
    with lock(state, name=worktree_lock_name(args.root)):
        started = time.time()
        result = subprocess.run(command, cwd=args.root)
        target = args.root / "target"
        if safe_directory(target, args.root):
            (target / ".mesh-talk-build-use").touch()
        if result.returncode:
            return (
                result.returncode if result.returncode > 0 else 128 - result.returncode
            )
        if args.archive:
            try:
                if custom_cargo_output(args.root, command):
                    raise ValueError(
                        "Custom Cargo output configuration: automatic archiving is unsupported"
                    )
                with lock(state, name="packages.lock"):
                    archive(args.root, state, args.archive, started)
            except (OSError, ValueError, subprocess.SubprocessError) as error:
                print(
                    f"Build succeeded; archive failed, cleanup skipped: {error}",
                    file=sys.stderr,
                )
                return 0
    try:
        clean(args, state)
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"Build succeeded; cleanup skipped: {error}", file=sys.stderr)
    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--root", type=Path, default=Path(__file__).resolve().parent.parent
    )
    subparsers = parser.add_subparsers(dest="action", required=True)
    for name in ("clean", "run"):
        sub = subparsers.add_parser(name)
        sub.add_argument("--checkout", type=Path, action="append", default=[])
        sub.add_argument(
            "--budget-gb", type=float, default=os.environ.get("MT_BUILD_CACHE_GB", "40")
        )
        sub.add_argument(
            "--keep", type=int, default=os.environ.get("MT_BUILD_KEEP", "5")
        )
        sub.add_argument("--dry-run", action="store_true")
        sub.add_argument(
            "--interval-seconds",
            type=int,
            default=os.environ.get("MT_BUILD_SCAN_INTERVAL_SECONDS", "600"),
        )
        if name == "run":
            sub.add_argument("--archive", choices=("binary", "bundle"))
            sub.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    args.root = args.root.expanduser().resolve()
    if not project(args.root):
        parser.error("--root must be a verified Mesh Talk checkout")
    if (
        not math.isfinite(args.budget_gb)
        or args.budget_gb < 0
        or args.keep < 1
        or args.interval_seconds < 0
    ):
        parser.error("budget must be finite and nonnegative; keep must be at least one")
    args.checkout.extend(
        Path(path)
        for path in os.environ.get("MT_BUILD_CHECKOUTS", "").split(os.pathsep)
        if path
    )
    default = Path.home() / ".cache/mesh-talk/build-retention"
    state = Path(os.environ.get("MT_BUILD_STATE_DIR", default)).expanduser().resolve()
    # Archives must live outside all source/target trees to avoid recursive copies.
    registry = state / "checkouts.json"
    registered = json.loads(registry.read_text()) if registry.exists() else []
    for root in checkouts(args.root, [*args.checkout, *registered]):
        if state == root or root in state.parents or state in root.parents:
            parser.error("build state must be separate from project checkouts")
    if args.action == "run":
        return run_build(args, state)
    clean(args, state)
    return 0


if __name__ == "__main__":
    try:
        sys.exit(main())
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f"Build retention failed safely: {error}", file=sys.stderr)
        sys.exit(1)
