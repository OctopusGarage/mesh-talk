"""Executable negative controls for scheduled inventory probes."""
import importlib.util
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]


def load(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / "scripts/ci" / f"{name}.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class FuzzInventoryTests(unittest.TestCase):
    def setUp(self):
        self.owner = load("fuzz-targets")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.directory = Path(self.temporary.name)
        self.manifest = ROOT / "fuzz/Cargo.toml"
        self.expected = self.owner.expected_targets(self.manifest)
        self.fake = self.directory / "cargo.py"
        self.state = self.directory / "state.json"
        self.calls = self.directory / "calls.jsonl"
        self.fake.write_text('''import json, pathlib, sys
root = pathlib.Path(__file__).parent
state = json.loads((root / "state.json").read_text())
args = sys.argv[1:]
with (root / "calls.jsonl").open("a") as output:
    output.write(json.dumps(args) + "\\n")
if args == ["+nightly", "fuzz", "list"]:
    print(state["inventory"])
    sys.exit(state.get("list_status", 0))
if state.get("fail_target") == args[5]:
    sys.exit(77)
''', encoding="utf-8")

    def run_inventory(self, inventory, **state):
        self.state.write_text(json.dumps({"inventory": inventory, **state}), encoding="utf-8")
        return self.owner.run_fuzz(self.manifest, "120", cargo=(sys.executable, str(self.fake)))

    def test_complete_inventory_runs_every_manifest_target_with_structured_arguments(self):
        self.assertEqual(len(self.expected), 15, "review target-count changes with the fuzz contract")
        self.run_inventory("\n".join(reversed(self.expected)))
        calls = [json.loads(line) for line in self.calls.read_text().splitlines()]
        self.assertEqual(calls[0], ["+nightly", "fuzz", "list"])
        self.assertEqual(calls[1:], [["+nightly", "fuzz", "run", "--target", "x86_64-unknown-linux-gnu", name, "--", "-max_total_time=120", "-rss_limit_mb=4096"] for name in self.expected])

    def test_failed_discovery_cannot_succeed_even_with_complete_stdout(self):
        with self.assertRaises(subprocess.CalledProcessError):
            self.run_inventory("\n".join(self.expected), list_status=42)
        self.assertEqual(len(self.calls.read_text().splitlines()), 1)

    def test_empty_missing_duplicate_and_unknown_inventories_are_rejected_before_execution(self):
        for inventory in ["", "\n".join(self.expected[:-1]), "\n".join(self.expected + self.expected[:1]), "\n".join(self.expected + ["unknown_target"])]:
            with self.subTest(inventory=inventory), self.assertRaises(ValueError):
                self.run_inventory(inventory)
        self.assertTrue(all(json.loads(line)[-1] == "list" for line in self.calls.read_text().splitlines()))

    def test_target_crash_propagates_and_does_not_report_success(self):
        with self.assertRaises(subprocess.CalledProcessError) as caught:
            self.run_inventory("\n".join(self.expected), fail_target=self.expected[0])
        self.assertEqual(caught.exception.returncode, 77)
        self.assertEqual(len(self.calls.read_text().splitlines()), 2)

    def test_invalid_duration_is_rejected_before_cargo_runs(self):
        for value in ["0", "-1", "1; echo injected", "$(touch injected)", "1\n2", "1.5", ""]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                self.owner.run_fuzz(self.manifest, value, cargo=(sys.executable, str(self.fake)))
        self.assertFalse(self.calls.exists())

    def test_cli_rejects_dispatch_input_as_data(self):
        completed = subprocess.run([sys.executable, "-B", str(ROOT / "scripts/ci/fuzz-targets.py")], env={**os.environ, "FUZZ_MAX_TOTAL_TIME": "$(touch injected)"}, capture_output=True, text=True, cwd=self.directory)
        self.assertNotEqual(completed.returncode, 0)
        self.assertIn("positive whole number", completed.stderr)
        self.assertFalse((self.directory / "injected").exists())

    def test_manifest_must_have_unique_nonempty_safe_target_inventory(self):
        for content in ['[package]\nname="empty"\n', '[[bin]]\nname="a"\n[[bin]]\nname="a"\n', '[[bin]]\nname="--help"\n', '[[bin]]\npath="missing-name.rs"\n', 'not valid TOML']:
            manifest = self.directory / "Cargo.toml"
            manifest.write_text(content)
            with self.subTest(content=content), self.assertRaises(ValueError):
                self.owner.expected_targets(manifest)


class GlibMigrationTests(unittest.TestCase):
    def setUp(self):
        self.owner = load("glib-migration")
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.lock = Path(self.temporary.name) / "Cargo.lock"

    def probe(self, versions):
        packages = ['[[package]]\nname="glib"\nversion=' + json.dumps(version) for version in versions]
        self.lock.write_text("\n".join(packages) if versions else '[[package]]\nname="other"\nversion="1.0.0"\n')
        return self.owner.probe_glib(self.lock)

    def test_mixed_old_and_new_branches_are_not_a_completed_migration(self):
        result = self.probe(["0.18.5", "0.21.5"])
        self.assertFalse(result["lifted"])
        self.assertEqual(result["versions"], ["0.18.5", "0.21.5"])

    def test_old_only_safe_only_and_absent(self):
        self.assertFalse(self.probe(["0.18.5"])["lifted"])
        self.assertTrue(self.probe(["0.20.0", "0.21.5"])["lifted"])
        self.assertFalse(self.probe([])["lifted"])
        self.assertFalse(self.probe(["0.14.0"])["lifted"], "advisory-unaffected old branch is not migration evidence")
        self.assertFalse(self.probe(["0.20.0-rc.1"])["lifted"])

    def test_invalid_lock_or_version_is_rejected(self):
        for content in ['not valid TOML', 'package="not an array"', '[[package]]\nname="glib"', '[[package]]\nname="glib"\nversion="0.20"', '[[package]]\nname="glib"\nversion=[]']:
            self.lock.write_text(content)
            with self.subTest(content=content), self.assertRaises(ValueError):
                self.owner.probe_glib(self.lock)

    def test_cli_emits_only_validated_migration_outputs(self):
        # Copy the actual entry point into an isolated checkout shape so the CLI
        # reads real fixture lock files without touching the product lock.
        script = self.lock.parent / "scripts/ci/glib-migration.py"
        script.parent.mkdir(parents=True)
        shutil.copyfile(ROOT / "scripts/ci/glib-migration.py", script)
        output = self.lock.parent / "outputs"
        for versions in [["0.18.5", "0.21.5"], ["0.20.0", "0.21.5"], []]:
            expected = self.probe(versions)
            output.unlink(missing_ok=True)
            completed = subprocess.run([sys.executable, "-B", str(script)], env={**os.environ, "GITHUB_OUTPUT": str(output)}, capture_output=True, text=True)
            self.assertEqual(completed.returncode, 0, completed.stderr)
            lines = output.read_text().splitlines()
            self.assertEqual(lines[0], f"lifted={str(expected['lifted']).lower()}")
            self.assertEqual(len(lines), 2 if expected["lifted"] else 1)
        output.unlink()
        self.lock.write_text('[[package]]\nname="glib"\nversion="malformed"')
        completed = subprocess.run([sys.executable, "-B", str(script)], env={**os.environ, "GITHUB_OUTPUT": str(output)}, capture_output=True, text=True)
        self.assertNotEqual(completed.returncode, 0)
        self.assertFalse(output.exists(), "invalid data must not publish a lifted output")


if __name__ == "__main__":
    unittest.main()
