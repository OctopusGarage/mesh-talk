import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import Mock

from resource_benchmark import evaluate, private_kib, wait_ready


class ResourceBenchmarkTest(unittest.TestCase):
    def test_private_memory_requires_both_kernel_counters(self):
        self.assertEqual(private_kib("Private_Clean: 2 kB\nPrivate_Dirty: 37 kB\n"), 39)
        with self.assertRaises(ValueError):
            private_kib("Private_Dirty: 37 kB\n")

    def test_reports_each_breach_and_preserves_values(self):
        result = evaluate((2, 1024, 0), [(32, 70000, 200000)], 10, 100)
        metrics = result["metrics"]
        self.assertEqual(metrics["cpu_percent"], 3)
        self.assertEqual(metrics["private_resident_kib"], 70000)
        self.assertEqual(metrics["write_bytes"], 200000)
        self.assertEqual(set(result["breaches"]), set(metrics))

    def test_quiet_window_passes_and_invalid_counter_fails(self):
        self.assertFalse(evaluate((2, 1024, 0), [(4, 1030, 4096)], 10, 100)["breaches"])
        with self.assertRaises(ValueError):
            evaluate((3, 1024, 0), [(2, 1030, 0)], 10, 100)

    def test_early_exit_and_timeout_are_distinct(self):
        with tempfile.TemporaryDirectory() as directory:
            log = Path(directory) / "node.log"
            log.write_text("Error: fixture bind failed\n")
            process = Mock()
            process.poll.return_value = 7
            with self.assertRaisesRegex(RuntimeError, "exit 7.*fixture bind failed"):
                wait_ready(process, log, 0)
            process.poll.return_value = None
            with self.assertRaisesRegex(RuntimeError, "readiness timed out"):
                wait_ready(process, log, 0)

    def test_missing_binary_publishes_error_and_fails(self):
        entrypoint = Path(__file__).with_name("resource_benchmark.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            report, summary = root / "report.json", root / "summary.md"
            result = subprocess.run(
                [sys.executable, "-B", entrypoint, root / "missing", report],
                env={**os.environ, "GITHUB_STEP_SUMMARY": str(summary)},
                capture_output=True, text=True, timeout=5,
            )
            self.assertEqual(result.returncode, 1)
            self.assertIn("::warning", result.stdout)
            recorded = json.loads(report.read_text())
            self.assertEqual(recorded["status"], "error")
            self.assertNotIn("metrics", recorded)
            self.assertIn("Status: **error**", summary.read_text())


if __name__ == "__main__":
    unittest.main()
