import json
import os
import platform
from pathlib import Path
import subprocess
import sys
import tempfile
import textwrap
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


    @unittest.skipUnless(
        platform.system() == "Linux" and os.getenv("MESH_RESOURCE_CONTROL") == "1",
        "requires opt-in Linux process-counter verification",
    )
    def test_real_process_counters_distinguish_quiet_and_resource_pressure(self):
        entrypoint = Path(__file__).with_name("resource_benchmark.py")
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            control = root / "resource-control"
            control.write_text(
                f"#!{sys.executable}\n" + textwrap.dedent("""\
                    import os
                    from pathlib import Path
                    import select
                    import sys
                    import time

                    pressure = os.environ["RESOURCE_CONTROL_MODE"] == "pressure"
                    if pressure:
                        retained = bytearray(80 * 1024 * 1024)
                        for index in range(0, len(retained), 4096):
                            retained[index] = 1
                        profile = Path(sys.argv[sys.argv.index("--keystore") + 1]).parent
                    print("listening on tcp/control", flush=True)
                    if pressure:
                        chunk = memoryview(retained)[:1024 * 1024]
                        next_write = time.monotonic()
                        remaining_writes = 32
                        with (profile / "pressure-write").open("wb") as output:
                            while not select.select([sys.stdin], [], [], 0)[0]:
                                if remaining_writes and time.monotonic() >= next_write:
                                    output.seek(0)
                                    output.write(chunk)
                                    output.flush()
                                    os.fsync(output.fileno())
                                    next_write = time.monotonic() + 1
                                    remaining_writes -= 1
                                sum(index * index for index in range(80_000))
                    sys.stdin.readline()
                    """),
            )
            control.chmod(0o700)

            for mode, expected_status, expected_exit in (
                ("quiet", "pass", 0),
                ("pressure", "breach", 1),
            ):
                with self.subTest(mode=mode):
                    report = root / f"{mode}.json"
                    summary = root / f"{mode}.md"
                    # The outer watchdog bounds bad fixture starts as well as sampling.
                    result = subprocess.run(
                        [sys.executable, "-B", entrypoint, control, report],
                        env={
                            **os.environ,
                            "RESOURCE_CONTROL_MODE": mode,
                            "GITHUB_STEP_SUMMARY": str(summary),
                        },
                        capture_output=True,
                        text=True,
                        timeout=125,
                    )
                    self.assertEqual(
                        result.returncode, expected_exit, result.stdout + result.stderr,
                    )
                    recorded = json.loads(report.read_text())
                    self.assertEqual(recorded["status"], expected_status)
                    self.assertEqual(set(recorded["metrics"]), set(recorded["budgets"]))
                    expected_breaches = set(recorded["budgets"]) if expected_exit else set()
                    self.assertEqual(set(recorded["breaches"]), expected_breaches)
                    self.assertIn(f"Status: **{expected_status}**", summary.read_text())
                    self.assertEqual("::warning" in result.stdout, bool(expected_exit))


if __name__ == "__main__":
    unittest.main()
