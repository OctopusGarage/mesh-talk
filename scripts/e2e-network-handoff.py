#!/usr/bin/env python3
"""Opt-in real Linux interface/IP handoff with disposable Docker profiles.

Build mesh-talk-node and the network-acceptance example for the Linux image with
fast-test-kdf. Supply the Docker volume/subdirectory containing those binaries.
No host network settings, installed profiles, contacts or services are changed.
This validates network namespaces; it is not a Wi-Fi hardware or WebView test.
"""
import argparse
import base64
import json
from pathlib import Path
import re
import subprocess
import threading
import time
import uuid


def docker(*args, check=True):
    return subprocess.run(["docker", *args], check=check, capture_output=True,
                          text=True, timeout=30).stdout.strip()


class Peer:
    def __init__(self, container, executable):
        self.lines = []
        self.condition = threading.Condition()
        self.child = subprocess.Popen(
            ["docker", "exec", "-i", container, executable, "--data", "/tmp/profile",
             "--name", container, "--discovery-port", "47474"],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, bufsize=1)
        threading.Thread(target=self.read, daemon=True).start()
        ready = self.until(lambda: next((s for s in self.lines if s.startswith("ready ")), None))
        _, self.device, self.account, _, _ = ready.split()

    def read(self):
        for line in self.child.stdout:
            with self.condition:
                self.lines.append(line.strip())
                self.condition.notify_all()

    def until(self, predicate, timeout=20):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            value = predicate()
            if value:
                return value
            if self.child.poll() is not None:
                raise AssertionError("Acceptance process exited: " + "\n".join(self.lines[-15:]))
            with self.condition:
                self.condition.wait(0.1)
        raise AssertionError("Acceptance condition timed out: " + "\n".join(self.lines[-15:]))

    def command(self, text):
        start = len(self.lines)
        self.child.stdin.write(text + "\n")
        self.child.stdin.flush()
        operation = text.split()[0]
        self.until(lambda: any(s == "ok " + operation or s.startswith("error ")
                              for s in self.lines[start:]))
        result = self.lines[start:]
        assert not any(s.startswith("error ") for s in result), result
        return result

    def sees(self, device):
        return any(s.startswith("peer " + device + " ") for s in self.command("peers"))

    def enqueue(self, account, text):
        payload = base64.b64encode(text.encode()).decode()
        lines = self.command(f"enqueue {account} {payload}")
        return next(s.split()[1] for s in lines if s.startswith("queued "))

    def delivered(self, event):
        return "delivery-status Some(Delivered)" in self.command("delivery-status " + event)

    def received(self, sender_account, text):
        payload = base64.b64encode(text.encode()).decode()
        return sum(s.startswith("entry ") and s.split()[2] == payload
                   for s in self.command("history " + sender_account))


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--image", required=True)
    parser.add_argument("--binaries-volume", required=True)
    parser.add_argument("--binaries-subdir", required=True)
    parser.add_argument("--output", default="target/network-handoff")
    parser.add_argument("--recovery-seconds", type=float, default=15)
    args = parser.parse_args()
    assert re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9_.-]*", args.binaries_volume)
    assert not Path(args.binaries_subdir).is_absolute() and ".." not in Path(args.binaries_subdir).parts
    assert args.recovery_seconds > 0
    output = Path(args.output)
    output.mkdir(parents=True, exist_ok=True)
    (output / "report.json").unlink(missing_ok=True)
    source_sha = subprocess.check_output(["git", "rev-parse", "HEAD"], text=True).strip()
    clean_source = not subprocess.check_output(["git", "status", "--porcelain"], text=True).strip()
    prefix = "mesh-handoff-" + uuid.uuid4().hex[:10]
    old, new = prefix + "-old", prefix + "-new"
    containers, peers, relay_processes, timings = [], [], [], []
    binary = "/build/" + args.binaries_subdir.rstrip("/") + "/"

    def container(name, network):
        name = prefix + "-" + name
        docker("run", "-d", "--name", name, "--network", network,
               "--mount", f"type=volume,src={args.binaries_volume},dst=/build,readonly",
               args.image, "sleep", "1800")
        containers.append(name)
        return name

    def move(name, source, target):
        docker("network", "disconnect", source, name)
        docker("network", "connect", target, name)

    def confirmed(a, b, event, text, started):
        b.until(lambda: b.received(a.account, text) == 1, args.recovery_seconds)
        a.until(lambda: a.delivered(event), args.recovery_seconds)
        elapsed = time.monotonic() - started
        assert elapsed <= args.recovery_seconds, elapsed
        return elapsed

    try:
        docker("network", "create", old)
        docker("network", "create", new)
        ac, bc = container("alice", old), container("bob", old)
        a, b = Peer(ac, binary + "network-acceptance"), Peer(bc, binary + "network-acceptance")
        peers.extend([a, b])
        a.until(lambda: a.sees(b.device))
        b.until(lambda: b.sees(a.device))
        baseline = "baseline-" + prefix
        event = a.enqueue(b.account, baseline)
        confirmed(a, b, event, baseline, time.monotonic())
        for source, target in [(old, new), (new, old)]:
            # Both real OS interfaces change, while application processes and
            # identity stores stay alive. Queue during the disconnected period.
            docker("network", "disconnect", source, bc)
            queued = "queued-" + target
            event = a.enqueue(b.account, queued)
            assert not a.delivered(event), "Disconnected recipient cannot confirm delivery"
            docker("network", "connect", target, bc)
            move(ac, source, target)
            started = time.monotonic()
            elapsed = confirmed(a, b, event, queued, started)
            fresh = "fresh-" + target
            fresh_id = a.enqueue(b.account, fresh)
            fresh_elapsed = confirmed(a, b, fresh_id, fresh, time.monotonic())
            assert a.sees(b.device) and b.sees(a.device)
            assert all(sum(s.startswith("ready ") for s in p.lines) == 1 for p in [a, b])
            timings.append({"scenario": "client-interface-handoff", "network": target,
                            "queued_receipt_seconds": elapsed, "new_receipt_seconds": fresh_elapsed})
            print(json.dumps(timings[-1]), flush=True)

        # A retained copy must recover through a relocated post office with the
        # sender disconnected. Returning the sender then proves receipt recovery.
        pc = container("relay", old)
        docker("exec", pc, "sh", "-c", "umask 077; mkdir -p /tmp/relay; printf 'disposable-handoff-only' > /tmp/password")
        relay = subprocess.Popen(["docker", "exec", pc, binary + "mesh-talk-node",
                                  "--post-office", "--keystore", "/tmp/relay/identity.keystore",
                                  "--password-file", "/tmp/password", "--name", prefix + "-relay"],
                                 stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        relay_processes.append(relay)
        relay_lines = []
        def read_relay():
            for line in relay.stdout:
                relay_lines.append(line.strip())
        threading.Thread(target=read_relay, daemon=True).start()
        relay_id = a.until(lambda: next((s.split()[1] for s in relay_lines
            if s.startswith("post-office ") and "listening on tcp/47479" in s), None))
        a.until(lambda: a.sees(relay_id))
        a.command("verify-peer " + b.device)
        b.command("verify-peer " + a.device)
        a.command("relay on")
        b.command("relay on")
        docker("network", "disconnect", old, bc)
        queued = "relay-custody-" + prefix
        before = int(docker("exec", pc, "stat", "-c", "%s", "/tmp/relay/relay.log"))
        event = a.enqueue(b.account, queued)
        a.until(lambda: int(docker("exec", pc, "stat", "-c", "%s", "/tmp/relay/relay.log")) > before)
        # Sender unavailable before the recipient returns: only the post office
        # can supply the exact queued payload; log growth alone cannot pass it.
        docker("network", "disconnect", old, ac)
        move(pc, old, new)
        docker("network", "connect", new, bc)
        started = time.monotonic()
        b.until(lambda: b.received(a.account, queued) == 1, args.recovery_seconds)
        recovery = time.monotonic() - started
        assert not a.delivered(event), "Custody alone cannot establish recipient confirmation"
        docker("network", "connect", new, ac)
        started = time.monotonic()
        a.until(lambda: a.delivered(event), args.recovery_seconds)
        assert relay.poll() is None, "Post office process must survive its handoff"
        timings.append({"scenario": "post-office-interface-handoff",
                        "recipient_recovery_seconds": recovery,
                        "sender_receipt_seconds": time.monotonic() - started})
        print(json.dumps(timings[-1]), flush=True)
        (output / "report.json").write_text(json.dumps({"status": "passed", "source_sha": source_sha, "clean_source": clean_source,
            "network_namespace": True, "physical_wifi": False, "restart": False, "manual_rescan": False,
            "recovery_limit_seconds": args.recovery_seconds, "timings": timings}, indent=2) + "\n")
    finally:
        for index, peer in enumerate(peers):
            (output / f"peer-{index}.log").write_text("\n".join(peer.lines) + "\n")
        for name in reversed(containers):
            docker("rm", "-f", name, check=False)
        for process in [p.child for p in peers] + relay_processes:
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.terminate()
                process.wait(timeout=10)
        for network in [new, old]:
            docker("network", "rm", network, check=False)


if __name__ == "__main__":
    main()
