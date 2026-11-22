"""Linux headless-node resource measurement for the existing CI build."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import socket
import subprocess
import tempfile
import time

WARMUP_SECONDS = 5
SAMPLE_SECONDS = 3
SAMPLES = 5
BUDGETS = {
    "cpu_percent": 2.0,
    "private_resident_kib": 64 * 1024,
    "write_bytes": 128 * 1024,
}


def private_kib(rollup):
    counters = {}
    for line in rollup.splitlines():
        fields = line.split()
        if fields and fields[0] in ("Private_Clean:", "Private_Dirty:"):
            if len(fields) != 3 or fields[2] != "kB":
                raise ValueError("unexpected private memory units")
            counters[fields[0]] = int(fields[1])
    if len(counters) != 2 or min(counters.values()) < 0:
        raise ValueError("private memory counters unavailable")
    return sum(counters.values())


def evaluate(start, samples, elapsed, ticks_per_second):
    previous = start
    peak = start[1]
    for sample in samples:
        if sample[0] < previous[0] or sample[2] < previous[2] or sample[1] < 0:
            raise ValueError("invalid process counter")
        peak = max(peak, sample[1])
        previous = sample
    if not samples or elapsed <= 0 or ticks_per_second <= 0:
        raise ValueError("invalid sampling window")
    metrics = {
        "cpu_percent": (previous[0] - start[0]) / ticks_per_second / elapsed * 100,
        "private_resident_kib": peak,
        "write_bytes": previous[2] - start[2],
    }
    return {
        "metrics": metrics,
        "budgets": BUDGETS,
        "breaches": [key for key, value in metrics.items() if value > BUDGETS[key]],
    }


def snapshot(process):
    if process.poll() is not None:
        raise RuntimeError("node exited during measurement")
    proc = Path("/proc") / str(process.pid)
    fields = (proc / "stat").read_text().rsplit(")", 1)[1].split()
    cpu = int(fields[11]) + int(fields[12])
    memory = private_kib((proc / "smaps_rollup").read_text())
    io = dict(line.split(":", 1) for line in (proc / "io").read_text().splitlines())
    return cpu, memory, int(io["write_bytes"])


def measure(binary):
    if platform.system() != "Linux":
        raise RuntimeError("Linux process counters are required")
    with tempfile.TemporaryDirectory(prefix="mesh-resource-") as directory:
        root = Path(directory)
        password = root / "password"
        password.touch(mode=0o600)
        password.write_text("synthetic-resource-fixture\n")
        with socket.socket(socket.AF_INET, socket.SOCK_DGRAM) as reservation:
            reservation.bind(("0.0.0.0", 0))
            discovery_port = reservation.getsockname()[1]
        stdout = root / "node.log"
        with stdout.open("wb") as output, subprocess.Popen(
            [binary, "--keystore", root / "identity.keystore", "--password-file", password,
             "--name", "resource-fixture", "--discovery-port", str(discovery_port)],
            stdin=subprocess.PIPE, stdout=output, stderr=subprocess.STDOUT,
        ) as process:
            try:
                deadline = time.monotonic() + 30
                while True:
                    with stdout.open("rb") as stream:
                        ready = b"listening on tcp/" in stream.read(65536)
                    if ready:
                        break
                    if process.poll() is not None or time.monotonic() >= deadline:
                        raise RuntimeError("node did not reach readiness")
                    time.sleep(0.05)
                time.sleep(WARMUP_SECONDS)
                started = time.monotonic()
                start = snapshot(process)
                samples = []
                for index in range(1, SAMPLES + 1):
                    time.sleep(max(0, started + index * SAMPLE_SECONDS - time.monotonic()))
                    samples.append(snapshot(process))
                elapsed = time.monotonic() - started
                result = evaluate(start, samples, elapsed, os.sysconf("SC_CLK_TCK"))
                process.communicate(b"/quit\n", timeout=5)
                if process.returncode:
                    raise RuntimeError("node failed to shut down")
                return {**result, "elapsed_seconds": elapsed, "samples": len(samples)}
            finally:
                if process.poll() is None:
                    process.kill()
                    process.wait()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("binary", type=Path)
    parser.add_argument("report", type=Path)
    args = parser.parse_args()
    report = {"schema": 1, "scenario": "fresh-headless-idle", "source": os.getenv("GITHUB_SHA"),
              "warmup_seconds": WARMUP_SECONDS, "kernel": platform.release(),
              "architecture": platform.machine(), "runner_image": os.getenv("ImageVersion")}
    try:
        binary = args.binary.resolve(strict=True)
        with binary.open("rb") as stream:
            report["binary_sha256"] = hashlib.file_digest(stream, "sha256").hexdigest()
        report.update(measure(binary))
        report["status"] = "breach" if report["breaches"] else "pass"
    except (OSError, RuntimeError, ValueError, IndexError, KeyError, subprocess.TimeoutExpired) as error:
        report.update(status="error", error=f"{type(error).__name__}: {error}")
    args.report.write_text(json.dumps(report, indent=2) + "\n")
    lines = ["### Linux headless idle resource benchmark", f"Status: **{report['status']}**"]
    if "metrics" in report:
        lines += ["", "| Metric | Observed | Budget |", "| --- | ---: | ---: |"]
        for key, value in report["metrics"].items():
            lines.append(f"| {key} | {value:.2f} | {BUDGETS[key]} |")
    if report["status"] != "pass":
        reason = ", ".join(report.get("breaches", [])) or report.get("error", "unknown error")
        lines += ["", reason]
        # Never treat unavailable counters as zero or a passing run.
        print("::warning title=Linux idle resource benchmark::See the job summary and benchmark artifact.")
    summary = "\n".join(lines) + "\n"
    print(summary)
    if summary_file := os.getenv("GITHUB_STEP_SUMMARY"):
        with Path(summary_file).open("a") as output:
            output.write(summary)
    return int(report["status"] != "pass")


if __name__ == "__main__":
    raise SystemExit(main())
