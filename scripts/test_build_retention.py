"""Exercise retention against real disposable worktrees and subprocesses."""

import json
import importlib.util
import os
from pathlib import Path
import plistlib
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch


SCRIPT = Path(__file__).with_name("build-retention.py")
SPEC = importlib.util.spec_from_file_location("build_retention", SCRIPT)
RETENTION = importlib.util.module_from_spec(SPEC)
if SCRIPT.exists():
    SPEC.loader.exec_module(RETENTION)


class RetentionTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.base = Path(self.temp.name).resolve()
        self.root = self.base / "repo with spaces"
        self.root.mkdir()
        self.state = self.base / "state"
        self.env = dict(os.environ, MT_BUILD_STATE_DIR=str(self.state))
        # Disposable CLI fixtures are independent supervisors, even when the
        # Python suite runs inside an affected-validation managed group.
        self.env.pop("MESH_VALIDATION_PROCESS_GROUP_OWNER", None)
        for key in list(self.env):
            if key.startswith("MT_TARGET_") or key.startswith("MT_BUILD_"):
                del self.env[key]
        self.env["MT_BUILD_STATE_DIR"] = str(self.state)
        self.env["CARGO_HOME"] = str(self.base / "cargo")
        for name in (
            "CARGO_TARGET_DIR",
            "CARGO_BUILD_TARGET_DIR",
            "CARGO_BUILD_TARGET",
            "CARGO_BUILD_BUILD_DIR",
        ):
            self.env.pop(name, None)
        self.git("init", "-q")
        (self.root / "src-tauri").mkdir()
        (self.root / "frontend").mkdir()
        (self.root / "frontend/package.json").write_text("{}\n")
        (self.root / "Cargo.toml").write_text(
            '[workspace.package]\nrepository = "https://github.com/OctopusGarage/mesh-talk"\n'
        )
        (self.root / "src-tauri/Cargo.toml").write_text(
            '[package]\nname = "mesh-talk"\n'
        )
        self.git("add", ".")
        self.git(
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.org",
            "commit",
            "-qm",
            "fixture",
        )

    def git(self, *args):
        return subprocess.check_output(
            ["git", "-C", str(self.root), *args], text=True
        ).strip()

    def run_cli(self, *args, root=None, check=True, env=None):
        result = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--root",
                str(root or self.root),
                *args,
            ],
            env=env or self.env,
            text=True,
            capture_output=True,
        )
        if check:
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        return result

    def cache(self, root, stamp=1):
        directory = root / "target/debug/deps"
        directory.mkdir(parents=True)
        (directory / "large.rlib").write_bytes(b"x" * 8192)
        os.utime(directory / "large.rlib", (stamp, stamp))
        os.utime(directory, (stamp, stamp))
        binary = root / "target/debug/mesh-talk"
        binary.write_text("runnable")
        return directory

    def symlink(self, path, target):
        try:
            path.symlink_to(target, target_is_directory=True)
        except OSError as error:
            if os.name == "nt":
                self.skipTest(f"Windows symlink privilege unavailable: {error}")
            raise

    def test_utf8_project_metadata_under_windows_gbk_locale(self):
        # Python on Chinese Windows defaults text files to GBK, while Cargo
        # manifests/configuration are UTF-8 even when comments contain smart quotes.
        for name in ("Cargo.toml", "src-tauri/Cargo.toml"):
            path = self.root / name
            content = path.read_text(encoding="utf-8")
            path.write_text(content + "# “本地构建”\n", encoding="utf-8")
        config = self.root / ".cargo/config.toml"
        config.parent.mkdir()
        config.write_text("# “默认构建目录”\n", encoding="utf-8")
        original_open = Path.open

        def windows_gbk_open(
            path, mode="r", buffering=-1, encoding=None, errors=None, newline=None
        ):
            if "b" not in mode and (encoding is None or encoding == "locale"):
                encoding = "gbk"
            return original_open(path, mode, buffering, encoding, errors, newline)

        with patch.object(Path, "open", windows_gbk_open):
            self.assertTrue(RETENTION.project(self.root))
            with patch.dict(os.environ, self.env, clear=True):
                self.assertFalse(RETENTION.custom_cargo_output(self.root, []))

    def test_budget_discovers_worktrees_and_preserves_binaries(self):
        other = self.base / "other"
        self.git("worktree", "add", "-qb", "other", str(other))
        old = self.cache(other)
        new = self.cache(self.root, time.time())
        preview = self.run_cli("clean", "--budget-gb", "0", "--dry-run")
        self.assertIn(str(old), preview.stdout)
        self.assertTrue(old.exists())
        self.run_cli("clean", "--budget-gb", "0")
        self.assertFalse(old.exists())
        self.assertFalse(new.exists())
        self.assertTrue((other / "target/debug/mesh-talk").exists())
        log = [
            json.loads(line)
            for line in (self.state / "cleanup.jsonl").read_text().splitlines()
        ]
        self.assertEqual(log[0]["root"], str(other))

    def age_target(self, root, stamp=1):
        target = root / "target"
        for path in sorted(target.rglob("*"), reverse=True):
            os.utime(path, (stamp, stamp))
        os.utime(target, (stamp, stamp))

    def test_automatic_cleanup_keeps_warm_cache_but_evicts_old_worktree(self):
        other = self.base / "old worktree"
        self.git("worktree", "add", "-qb", "old", str(other))
        old = self.cache(other)
        self.age_target(other)
        current = self.cache(self.root)
        self.age_target(self.root)
        for _ in range(2):
            result = self.run_cli(
                "run", "--budget-gb", "0", "--interval-seconds", "0",
                "--", sys.executable, "-c", "pass",
            )
            self.assertTrue(current.exists(), result.stdout + result.stderr)
            self.assertFalse(old.exists(), result.stdout + result.stderr)
            self.assertIn("recently used Cargo profile", result.stdout)
            self.assertIn("Budget still exceeded", result.stdout)
        self.assertTrue((other / "target/debug/mesh-talk").exists())

    def test_automatic_cleanup_protects_recent_custom_shared_target(self):
        other = self.base / "shared target worktree"
        self.git("worktree", "add", "-qb", "shared", str(other))
        shared = self.cache(other)
        self.age_target(other)
        result = self.run_cli(
            "run", "--budget-gb", "0", "--interval-seconds", "0", "--",
            sys.executable, "-c",
            f"from pathlib import Path; Path({str(shared / 'large.rlib')!r}).write_bytes(b'new compilation')",
            env=dict(self.env, CARGO_TARGET_DIR=str(other / "target")),
        )
        self.assertTrue(shared.exists(), result.stdout + result.stderr)
        self.assertIn("recently used Cargo profile", result.stdout)
        self.age_target(other)
        # A fresh Cargo cache hit need not update artifact mtimes. The explicit
        # shared-target use must refresh its own verified checkout marker too.
        result = self.run_cli(
            "run", "--budget-gb", "0", "--interval-seconds", "0", "--",
            sys.executable, "-c", "pass",
            env=dict(self.env, CARGO_TARGET_DIR=str(other / "target")),
        )
        self.assertTrue(shared.exists(), result.stdout + result.stderr)
        self.assertTrue((other / "target/.mesh-talk-build-use").is_file())

    def test_automatic_cleanup_reclaims_cache_after_idle_window(self):
        other = self.base / "idle worktree"
        self.git("worktree", "add", "-qb", "idle", str(other))
        idle = self.cache(other, time.time())
        first = self.run_cli(
            "run", "--budget-gb", "0", "--interval-seconds", "0",
            "--", sys.executable, "-c", "pass",
        )
        self.assertTrue(idle.exists(), first.stdout + first.stderr)
        # Move real artifact timestamps beyond the window, without sleeping or
        # relying on compiler coordination lock timestamps as build activity.
        self.age_target(other)
        second = self.run_cli(
            "run", "--budget-gb", "0", "--interval-seconds", "0",
            "--", sys.executable, "-c", "pass",
        )
        self.assertFalse(idle.exists(), second.stdout + second.stderr)

    def test_zero_idle_window_and_explicit_cleanup_can_reclaim_warm_cache(self):
        for explicit in (False, True):
            with self.subTest(explicit=explicit):
                cache = self.cache(self.root, time.time())
                if explicit:
                    self.run_cli("clean", "--budget-gb", "0", env=dict(
                        self.env, MT_BUILD_CACHE_MIN_IDLE_SECONDS="600"))
                else:
                    self.run_cli("run", "--budget-gb", "0", "--interval-seconds", "0",
                                 "--min-idle-seconds", "0", "--", sys.executable, "-c", "pass")
                self.assertFalse(cache.exists())

    def test_idle_window_environment_override_and_invalid_policy(self):
        cache = self.cache(self.root, time.time())
        for value in ("-1", "invalid", "1.5"):
            result = self.run_cli("run", "--", sys.executable, "-c", "pass", check=False,
                                  env=dict(self.env, MT_BUILD_CACHE_MIN_IDLE_SECONDS=value))
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse(self.state.exists())
            self.assertTrue(cache.exists())
        result = self.run_cli("run", "--min-idle-seconds", "-1", "--", sys.executable, "-c", "pass", check=False)
        self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.state.exists())
        self.run_cli("run", "--budget-gb", "0", "--interval-seconds", "0",
                     "--", sys.executable, "-c", "pass",
                     env=dict(self.env, MT_BUILD_CACHE_MIN_IDLE_SECONDS="0"))
        self.assertFalse(cache.exists())

    def test_custom_target_marker_does_not_write_unverified_output(self):
        outside = self.base / "unverified checkout"
        cache = self.cache(outside, time.time())
        self.run_cli("run", "--budget-gb", "0", "--interval-seconds", "0", "--",
                     sys.executable, "-c", "pass",
                     env=dict(self.env, CARGO_BUILD_TARGET_DIR=str(outside / "target")))
        self.assertTrue(cache.exists())
        self.assertFalse((outside / "target/.mesh-talk-build-use").exists())

    def test_symlinked_marker_neither_warms_cache_nor_touches_external_file(self):
        other = self.base / "shared worktree"
        self.git("worktree", "add", "-qb", "marker", str(other))
        cache = self.cache(other)
        self.age_target(other)
        outside = self.base / "external-marker"
        outside.write_text("keep metadata")
        before = outside.stat().st_mtime_ns
        try:
            (other / "target/.mesh-talk-build-use").symlink_to(outside)
        except OSError as error:
            if os.name == "nt":
                self.skipTest(f"Windows symlink privilege unavailable: {error}")
            raise
        self.run_cli("run", "--budget-gb", "0", "--interval-seconds", "0", "--",
                     sys.executable, "-c", "pass",
                     env=dict(self.env, CARGO_TARGET_DIR=str(other / "target")))
        self.assertFalse(cache.exists())
        self.assertEqual(outside.stat().st_mtime_ns, before)
        self.assertEqual(outside.read_text(), "keep metadata")

    def test_tree_stats_counts_files_once_across_hard_links(self):
        tree = self.base / "stat-tree"
        tree.mkdir()
        nested = tree / "deps"
        nested.mkdir()
        artifact = nested / "artifact.rlib"
        artifact.write_bytes(b"x" * 8192)
        os.link(artifact, nested / "artifact-copy.rlib")
        expected = sum(
            info.st_blocks * 512 if hasattr(info, "st_blocks") else info.st_size
            for info in (tree.stat(), nested.stat(), artifact.stat())
        )
        self.assertEqual(RETENTION.tree_stats(tree)[0], expected)

    def test_retains_five_successful_sets_globally(self):
        other = self.base / "other"
        self.git("worktree", "add", "-qb", "other", str(other))
        for index in range(7):
            root = self.root if index % 2 else other
            binary = root / "target/release/mesh-talk"
            binary.parent.mkdir(parents=True, exist_ok=True)
            binary.write_text(str(index))
            self.run_cli(
                "run",
                "--archive",
                "binary",
                "--",
                sys.executable,
                "-c",
                "pass",
                root=root,
            )
        sets = sorted((self.state / "packages").iterdir())
        self.assertEqual(len(sets), 5)
        self.assertEqual(
            [int((entry / "mesh-talk").read_text()) for entry in sets],
            list(range(2, 7)),
        )
        self.assertIn("commit", json.loads((sets[-1] / "manifest.json").read_text()))

    def test_failed_build_preserves_exit_status_and_cache(self):
        cache = self.cache(self.root)
        result = self.run_cli(
            "run", "--", sys.executable, "-c", "raise SystemExit(7)", check=False
        )
        self.assertEqual(result.returncode, 7)
        self.assertTrue(cache.exists())
        self.assertFalse((self.state / "packages").exists())

    def test_symlink_target_and_unrelated_clone_are_never_cleaned(self):
        outside = self.base / "outside"
        outside.mkdir()
        cache = self.cache(outside)
        self.symlink(self.root / "target", outside / "target")
        self.run_cli("clean", "--budget-gb", "0", "--checkout", str(outside))
        self.assertTrue(cache.exists())

    def test_disable_switch(self):
        cache = self.cache(self.root)
        self.run_cli(
            "clean", "--budget-gb", "0", env=dict(self.env, MT_TARGET_CLEAN="0")
        )
        self.assertTrue(cache.exists())

    def test_budget_keeps_newer_cache_when_old_eviction_is_enough(self):
        other = self.base / "other"
        self.git("worktree", "add", "-qb", "other", str(other))
        old = self.cache(other)
        time.sleep(0.02)
        fresh = self.cache(self.root, time.time())

        def disk_bytes(path):
            stat = path.stat()
            return stat.st_blocks * 512 if hasattr(stat, "st_blocks") else stat.st_size

        total = sum(
            disk_bytes(path)
            for root in (other, self.root)
            for path in [root / "target", *(root / "target").rglob("*")]
        )
        reclaimed = sum(disk_bytes(path) for path in [old, *old.rglob("*")])
        self.run_cli("clean", "--budget-gb", str((total - reclaimed + 1) / 2**30))
        self.assertFalse(old.exists())
        self.assertTrue(fresh.exists())

    def test_bundle_archive_is_complete_and_original_is_preserved(self):
        bundle = self.root / "target/release/bundle/macos/mesh-talk.app/Contents"
        bundle.mkdir(parents=True)
        (bundle / "app").write_text("application")
        stale = self.root / "target/release/bundle/old.dmg"
        stale.write_text("old version")
        (self.root / "target/release/mesh-talk-node").write_text("old CLI")
        os.utime(stale, (1, 1))
        self.run_cli(
            "run",
            "--archive",
            "bundle",
            "--",
            sys.executable,
            "-c",
            f"from pathlib import Path; Path({str(bundle / 'app')!r}).write_text('application')",
        )
        archive = next((self.state / "packages").iterdir())
        self.assertEqual(
            (archive / "bundle/macos/mesh-talk.app/Contents/app").read_text(),
            "application",
        )
        self.assertTrue((bundle / "app").exists())
        self.assertFalse((archive / "bundle/old.dmg").exists())
        self.assertFalse((archive / "mesh-talk-node").exists())

    def test_archive_failure_does_not_prune_or_change_success(self):
        cache = self.cache(self.root)
        result = self.run_cli(
            "run",
            "--archive",
            "bundle",
            "--budget-gb",
            "0",
            "--",
            sys.executable,
            "-c",
            "pass",
        )
        self.assertIn("archive failed", result.stderr)
        self.assertTrue(cache.exists())

    def test_explicit_clone_is_included(self):
        clone = self.base / "clone"
        subprocess.check_call(["git", "clone", "-q", str(self.root), str(clone)])
        cache = self.cache(clone)
        self.run_cli("clean", "--budget-gb", "0", "--checkout", str(clone))
        self.assertFalse(cache.exists())

    def test_registered_clone_is_remembered_for_future_builds(self):
        clone = self.base / "clone"
        subprocess.check_call(["git", "clone", "-q", str(self.root), str(clone)])
        self.run_cli("clean", "--checkout", str(clone))
        cache = self.cache(clone)
        self.run_cli("clean", "--budget-gb", "0")
        self.assertFalse(cache.exists())

    def test_nested_symlink_and_cross_target_profiles(self):
        outside = self.base / "outside"
        outside.mkdir()
        (outside / "important").write_text("keep")
        cache = self.cache(self.root)
        self.symlink(cache / "external", outside)
        triple = self.root / "target/aarch64-apple-darwin/release/incremental"
        triple.mkdir(parents=True)
        (triple / "data").write_text("rebuildable")
        self.run_cli("clean", "--budget-gb", "0")
        self.assertFalse(cache.exists())
        self.assertFalse(triple.exists())
        self.assertEqual((outside / "important").read_text(), "keep")

    def test_symlinked_profile_is_preserved(self):
        outside = self.base / "outside"
        outside.mkdir()
        cache = self.cache(outside)
        (self.root / "target").mkdir()
        self.symlink(self.root / "target/debug", outside / "target/debug")
        self.run_cli("clean", "--budget-gb", "0")
        self.assertTrue(cache.exists())

    def test_invalid_policy_is_rejected_without_mutation(self):
        for options in (("--keep", "0"), ("--budget-gb", "nan"), ("--budget-gb", "-1")):
            result = self.run_cli("clean", *options, check=False)
            self.assertNotEqual(result.returncode, 0)
        self.assertFalse(self.state.exists())

    def test_frequent_builds_throttle_cache_scan_but_still_keep_five_archives(self):
        release = self.root / "target/release"
        release.mkdir(parents=True)
        (release / "mesh-talk").write_text("binary")
        self.run_cli("run", "--archive", "binary", "--", sys.executable, "-c", "pass")
        self.cache(self.root)
        for _ in range(5):
            result = self.run_cli(
                "run",
                "--archive",
                "binary",
                "--budget-gb",
                "0",
                "--",
                sys.executable,
                "-c",
                "pass",
            )
        self.assertIn("recently", result.stdout)
        self.assertTrue((self.root / "target/debug/deps").exists())
        self.assertEqual(len(list((self.state / "packages").iterdir())), 5)

    def test_custom_target_configuration_skips_stale_archive(self):
        release = self.root / "target/release"
        release.mkdir(parents=True)
        (release / "mesh-talk").write_text("stale")
        config = self.root / ".cargo"
        config.mkdir()
        (config / "config.toml").write_text('[build]\ntarget-dir = "elsewhere"\n')
        result = self.run_cli(
            "run", "--archive", "binary", "--", sys.executable, "-c", "pass"
        )
        self.assertIn("Custom Cargo", result.stderr)
        self.assertFalse((self.state / "packages").exists())

    @unittest.skipIf(os.name == "nt", "launchd installer is a Unix shell script")
    def test_scheduler_installs_a_stable_runner_and_captures_configuration(self):
        home = self.base / "home"
        bin_dir = self.base / "bin"
        bin_dir.mkdir()
        for name, body in (("uname", "echo Darwin"), ("launchctl", "exit 0")):
            path = bin_dir / name
            path.write_text(f"#!/bin/sh\n{body}\n")
            path.chmod(0o755)
        env = dict(
            self.env,
            HOME=str(home),
            PATH=str(bin_dir) + os.pathsep + os.environ["PATH"],
            MT_BUILD_CACHE_GB="20",
        )
        subprocess.run(
            ["bash", str(SCRIPT.with_name("install-auto-clean-macos.sh"))],
            env=env,
            check=True,
            capture_output=True,
        )
        plist = (
            home / "Library/LaunchAgents/org.octopusgarage.mesh-talk.target-gc.plist"
        )
        with plist.open("rb") as handle:
            job = plistlib.load(handle)
        runner = Path(job["ProgramArguments"][1])
        self.assertTrue(runner.is_file())
        self.assertIn(home, runner.parents)
        self.assertEqual(job["EnvironmentVariables"]["MT_BUILD_CACHE_GB"], "20")
        self.assertEqual(job["StartCalendarInterval"], {"Hour": 3, "Minute": 0})
        self.assertEqual(job["StartInterval"], 600)

    def test_crashed_partial_archives_are_reclaimed(self):
        partial = self.state / "packages/.partial-00000000000000000001-abcdef01"
        partial.mkdir(parents=True)
        (partial / "payload").write_text("incomplete")
        self.run_cli("clean", "--dry-run")
        self.assertTrue(partial.exists())
        self.run_cli("clean")
        self.assertFalse(partial.exists())

    def test_active_managed_build_blocks_cleanup(self):
        cache = self.cache(self.root)
        idle_root = self.base / "idle"
        self.git("worktree", "add", "-qb", "idle", str(idle_root))
        idle_cache = self.cache(idle_root)
        ready = self.base / "ready"
        process = subprocess.Popen(
            [
                sys.executable,
                str(SCRIPT),
                "--root",
                str(self.root),
                "run",
                "--",
                sys.executable,
                "-c",
                f"from pathlib import Path; import time\np = Path({str(ready)!r})\np.touch()\nwhile not p.read_text(): time.sleep(0.02)",
            ],
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        try:
            deadline = time.monotonic() + 5
            while (
                not ready.exists()
                and time.monotonic() < deadline
                and process.poll() is None
            ):
                time.sleep(0.02)
            self.assertTrue(ready.exists(), "build wrapper failed to start")
            result = self.run_cli("clean", "--budget-gb", "0")
            self.assertIn("active", result.stdout.lower())
            self.assertTrue(cache.exists())
            self.assertFalse(
                idle_cache.exists(),
                "An active worktree must not block cleanup of idle worktrees",
            )
        finally:
            # Release the child normally so the build lock is not orphaned.
            ready.write_text("stop")
            process.communicate(timeout=5)

    @unittest.skipIf(os.name == "nt" or not shutil.which("node"), "POSIX managed group and Node required")
    def test_managed_cancellation_retains_worktree_lock_until_group_cleanup(self):
        (self.root / ".gitignore").write_text("target/\n")
        target = self.root / "target"
        target.mkdir()
        ready = target / "worker-pid"
        cancel = target / "cancel"
        aborting = target / "aborting"
        worker = (
            "import os,signal,time; from pathlib import Path\n"
            "signal.signal(signal.SIGTERM, lambda *_: None)\n"
            f"Path({str(ready)!r}).write_text(str(os.getpid()))\n"
            "while True: time.sleep(.05)"
        )
        command = [sys.executable, str(SCRIPT), "--root", str(self.root),
                   "run", "--", sys.executable, "-c", worker]
        stage = SCRIPT.with_name("validation") / "stage.mjs"
        program = f"""
            import {{ runStage }} from {json.dumps(stage.as_uri())};
            import {{ existsSync, writeFileSync }} from 'node:fs';
            const controller = new AbortController();
            const timer = setInterval(() => {{
              if (existsSync({json.dumps(str(cancel))})) {{
                clearInterval(timer);
                controller.abort();
                writeFileSync({json.dumps(str(aborting))}, 'yes');
              }}
            }}, 10);
            const result = await runStage({{
              root: {json.dumps(str(self.root))}, name: 'retention-cancellation',
              command: {json.dumps(command)}, signal: controller.signal,
              env: {{ ...process.env, MESH_VALIDATION_PROCESS_GROUP_OWNER: undefined }},
              log: () => {{}}
            }});
            clearInterval(timer);
            console.log('RESULT ' + result.code);
        """
        process = subprocess.Popen(
            [shutil.which("node"), "--input-type=module", "-e", program],
            env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True,
        )
        try:
            deadline = time.monotonic() + 10
            while not ready.exists() and time.monotonic() < deadline and process.poll() is None:
                time.sleep(.02)
            if not ready.exists():
                output, error = process.communicate(timeout=2)
                self.fail("managed worker did not start: " + output + error)
            worker_pid = int(ready.read_text())
            cancel.touch()
            while not aborting.exists() and time.monotonic() < deadline and process.poll() is None:
                time.sleep(.01)
            self.assertTrue(aborting.exists(), "outer supervisor did not cancel")
            time.sleep(.1)
            os.kill(worker_pid, 0)  # Work is still running in the grace period.
            with self.assertRaises(BlockingIOError):
                with RETENTION.lock(self.state, name=RETENTION.worktree_lock_name(self.root), blocking=False):
                    pass
            output, error = process.communicate(timeout=10)
            self.assertEqual(process.returncode, 0, output + error)
            self.assertIn("RESULT 130", output)
            state = subprocess.run(["ps", "-p", str(worker_pid), "-o", "stat="], capture_output=True, text=True)
            self.assertFalse(state.returncode == 0 and state.stdout.strip() and not state.stdout.strip().startswith("Z"),
                             "worker survived managed group cleanup")
            with RETENTION.lock(self.state, name=RETENTION.worktree_lock_name(self.root), blocking=False):
                pass
        finally:
            cancel.touch()
            try:
                process.communicate(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.communicate(timeout=5)
            if ready.exists():
                try:
                    os.kill(int(ready.read_text()), signal.SIGKILL)
                except ProcessLookupError:
                    pass

    @unittest.skipIf(os.name == "nt", "POSIX signal dispositions")
    def test_unmanaged_or_invalid_owner_preserves_signal_handlers(self):
        previous = {value: signal.getsignal(value) for value in (signal.SIGTERM, signal.SIGINT)}
        for owner in ("", "invalid", "0", str(os.getpid()), "9" * 100):
            with self.subTest(owner=owner), patch.dict(os.environ, {"MESH_VALIDATION_PROCESS_GROUP_OWNER": owner}):
                with RETENTION.managed_cancellation():
                    self.assertEqual(previous, {value: signal.getsignal(value) for value in previous})
                self.assertEqual(previous, {value: signal.getsignal(value) for value in previous})

    @unittest.skipIf(os.name == "nt", "POSIX exec signal dispositions")
    def test_managed_handler_restores_parent_and_keeps_child_default(self):
        previous = signal.getsignal(signal.SIGTERM)
        with patch.dict(os.environ, {"MESH_VALIDATION_PROCESS_GROUP_OWNER": "123456"}), patch.object(
            RETENTION.os, "getpgid", return_value=os.getpgrp() + 1
        ):
            with RETENTION.managed_cancellation():
                self.assertTrue(callable(signal.getsignal(signal.SIGTERM)))
                result = subprocess.run(
                    [sys.executable, "-c", "import signal; print(signal.getsignal(signal.SIGTERM))"],
                    capture_output=True, text=True, check=True,
                )
                self.assertEqual(result.stdout.strip(), str(int(signal.SIG_DFL)))
        self.assertEqual(signal.getsignal(signal.SIGTERM), previous)

    def test_raw_cargo_profile_lock_protects_only_that_profile(self):
        busy = self.cache(self.root)
        idle = self.root / "target/release/deps"
        for name in (".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock"):
            with self.subTest(lock=name):
                idle.mkdir(parents=True)
                (idle / "rebuildable").write_bytes(b"x" * 8192)
                with RETENTION.lock(busy.parent, name=name):
                    result = self.run_cli("clean", "--budget-gb", "0")
                    self.assertTrue(
                        busy.exists(), "Raw Cargo's profile must remain intact"
                    )
                    self.assertFalse(
                        idle.exists(), "An idle profile must still be cleaned"
                    )
                    self.assertIn("active", result.stdout.lower())

    def test_coverage_profile_lock_preserves_coverage_while_native_cache_is_cleaned(
        self,
    ):
        native = self.cache(self.root)
        coverage = self.root / "target/llvm-cov-target/debug/deps"
        coverage.mkdir(parents=True)
        (coverage / "data").write_text("rebuildable")
        with RETENTION.lock(coverage.parent, name=".cargo-lock"):
            self.run_cli("clean", "--budget-gb", "0")
            self.assertTrue(coverage.exists())
            self.assertFalse(native.exists())
        self.run_cli("clean", "--budget-gb", "0")
        self.assertFalse(coverage.exists())
        self.assertTrue((coverage.parent / ".cargo-lock").exists())

    def test_symlinked_cargo_lock_cannot_redirect_cleanup(self):
        busy = self.cache(self.root)
        external = self.base / "external"
        external.mkdir()
        self.symlink(busy.parent / ".cargo-lock", external)
        self.run_cli("clean", "--budget-gb", "0")
        self.assertTrue(busy.exists())
        self.assertEqual(list(external.iterdir()), [])

    def test_archive_capture_only_defers_package_pruning(self):
        cache = self.cache(self.root)
        partial = self.state / "packages/.partial-00000000000000000001-abcdef01"
        partial.mkdir(parents=True)
        (partial / "data").write_text("being copied")
        with RETENTION.lock(self.state, name="packages.lock"):
            self.run_cli("clean", "--budget-gb", "0")
            self.assertFalse(cache.exists())
            self.assertTrue(partial.exists())
        self.run_cli("clean", "--budget-gb", "0")
        self.assertFalse(partial.exists())

    @unittest.skipUnless(
        shutil.which("cargo"), "Cargo required for lock interoperability"
    )
    def test_real_cargo_and_cleanup_obey_each_others_locks(self):
        crate = self.base / "tiny"
        (crate / "src").mkdir(parents=True)
        (crate / "Cargo.toml").write_text(
            '[package]\nname="retention-lock-fixture"\nversion="0.1.0"\nedition="2021"\n'
        )
        (crate / "src/main.rs").write_text("fn main() {}\n")
        (crate / "build.rs").write_text("""
fn main() {
    let ready = std::env::var("RETENTION_READY").unwrap();
    let release = std::env::var("RETENTION_RELEASE").unwrap();
    std::fs::write(ready, "ready").unwrap();
    while !std::path::Path::new(&release).exists() {
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
""")
        ready, release = self.base / "ready", self.base / "release"
        env = dict(
            self.env,
            CARGO_TARGET_DIR=str(self.root / "target"),
            RETENTION_READY=str(ready),
            RETENTION_RELEASE=str(release),
        )
        command = [
            shutil.which("cargo"),
            "build",
            "--offline",
            "--manifest-path",
            str(crate / "Cargo.toml"),
        ]
        process = subprocess.Popen(
            command, env=env, stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True
        )
        profile = self.root / "target/debug"
        try:
            deadline = time.monotonic() + 30
            while (
                not ready.exists()
                and process.poll() is None
                and time.monotonic() < deadline
            ):
                time.sleep(0.02)
            self.assertTrue(
                ready.exists(), "tiny Cargo build did not reach its build script"
            )
            idle = self.root / "target/release/deps"
            idle.mkdir(parents=True)
            (idle / "cache").write_text("rebuildable")
            with self.assertRaises(BlockingIOError):
                with RETENTION.cargo_profile_locks(profile, self.root):
                    pass
            self.run_cli("clean", "--budget-gb", "0")
            self.assertTrue((profile / "deps").exists())
            self.assertFalse(idle.exists())
        finally:
            release.touch()
            _, errors = process.communicate(timeout=60)
        self.assertEqual(process.returncode, 0, errors)
        # Also prove a raw Cargo process that starts during cleanup waits.
        log = self.base / "cargo.log"
        process = None
        try:
            with log.open("w") as output:
                with RETENTION.cargo_profile_locks(profile, self.root):
                    process = subprocess.Popen(
                        command, env=env, stdout=output, stderr=output
                    )
                    deadline = time.monotonic() + 15
                    while (
                        "file lock" not in log.read_text()
                        and process.poll() is None
                        and time.monotonic() < deadline
                    ):
                        time.sleep(0.02)
                    self.assertIn("file lock", log.read_text())
        finally:
            # Release the profile locks even when the assertion fails.
            if process is not None:
                process.wait(timeout=60)
        self.assertEqual(process.returncode, 0, log.read_text())
        self.run_cli("clean", "--budget-gb", "0")
        self.assertFalse((profile / "deps").exists())
        self.assertTrue((profile / ".cargo-lock").exists())


if __name__ == "__main__":
    unittest.main()
