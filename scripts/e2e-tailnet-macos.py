#!/usr/bin/env python3
"""Real two-Mac routed discovery, Noise DM, and consented file transfer.

Build both test binaries first:
  cargo build -p mesh-talk-core --bin mesh-talk-node --features fast-test-kdf
  cargo build -p mesh-talk-core --bin mesh-talk-node \
      --target x86_64-apple-darwin --features fast-test-kdf
Then run: python3 scripts/e2e-tailnet-macos.py mac2015

The two nodes use different discovery ports, so LAN multicast and /24 scans
cannot make the test pass. All temporary identities and files are removed.
"""

import hashlib
import json
import os
from pathlib import Path
import queue
import random
import re
import shlex
import subprocess
import sys
import tempfile
import threading
import time


ROOT = Path(__file__).resolve().parents[1]
LOCAL_BINARY = ROOT / "target/debug/mesh-talk-node"
REMOTE_BINARY = ROOT / "target/x86_64-apple-darwin/debug/mesh-talk-node"
REMOTE_TAILSCALE = "/Applications/Tailscale.app/Contents/MacOS/Tailscale"


def ssh(host, command, **kwargs):
    return subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", host, command],
        check=True,
        **kwargs,
    )


def tailscale_status(host=None):
    if host:
        result = ssh(
            host,
            f"TAILSCALE_BE_CLI=1 {shlex.quote(REMOTE_TAILSCALE)} status --json",
            capture_output=True,
            text=True,
        )
    else:
        result = subprocess.run(
            ["tailscale", "status", "--json"], check=True, capture_output=True, text=True
        )
    status = json.loads(result.stdout)
    if status.get("BackendState") != "Running":
        raise AssertionError(f"Tailscale is not running on {host or 'local Mac'}")
    ips = status.get("TailscaleIPs") or status.get("Self", {}).get("TailscaleIPs") or []
    ipv4 = next((ip for ip in ips if ip.startswith("100.")), None)
    if not ipv4:
        raise AssertionError(f"No Tailscale IPv4 address on {host or 'local Mac'}")
    return status, ipv4


class Node:
    def __init__(self, name, command):
        self.name = name
        self.process = subprocess.Popen(
            command, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT, text=True, bufsize=1,
        )
        self.lines = queue.Queue()

        def collect():
            for line in self.process.stdout:
                line = line.rstrip("\n")
                print(f"[{name}] {line}", flush=True)
                self.lines.put(line)

        threading.Thread(target=collect, daemon=True).start()

    def send(self, command):
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()

    def expect(self, predicate, timeout=20):
        end = time.monotonic() + timeout
        while time.monotonic() < end:
            if self.process.poll() is not None and self.lines.empty():
                raise AssertionError(f"{self.name} exited: {self.process.returncode}")
            try:
                line = self.lines.get(timeout=min(0.5, end - time.monotonic()))
            except queue.Empty:
                continue
            if predicate(line):
                return line
        raise AssertionError(f"Timed out waiting for {self.name} output")

    def stop(self):
        if self.process.poll() is None:
            try:
                self.send("/quit")
                self.process.wait(timeout=5)
            except (BrokenPipeError, subprocess.TimeoutExpired):
                self.process.terminate()
                self.process.wait(timeout=5)


def await_peer(node, identity, expected_ip, probe_target):
    end = time.monotonic() + 30
    next_probe = time.monotonic() + 5
    while time.monotonic() < end:
        if time.monotonic() >= next_probe:
            node.send(f"/probe {probe_target}")
            next_probe = time.monotonic() + 5
        node.send("/peers")
        try:
            line = node.expect(lambda value: value.startswith(f"peer {identity} "), timeout=1)
        except AssertionError:
            continue
        if f" {expected_ip}:" not in line:
            raise AssertionError(f"Peer found over wrong path: {line}")
        return
    raise AssertionError(f"{node.name} did not discover {identity} over {expected_ip}")


def main(remote_host):
    if not LOCAL_BINARY.is_file() or not REMOTE_BINARY.is_file():
        raise SystemExit("Build the arm64 and x86_64 test binaries shown in the docstring first")
    local_status, local_ip = tailscale_status()
    remote_status, remote_ip = tailscale_status(remote_host)
    for status, other_ip in [(local_status, remote_ip), (remote_status, local_ip)]:
        if not any(other_ip in peer.get("TailscaleIPs", []) and peer.get("Online")
                   for peer in (status.get("Peer") or {}).values()):
            raise AssertionError(f"{other_ip} is not an online Tailscale peer")
    print(f"Tailnet endpoints: local {local_ip}, {remote_host} {remote_ip}")

    remote_dir = ssh(remote_host, "mktemp -d /tmp/mesh-talk-tailnet-e2e.XXXXXX",
                     capture_output=True, text=True).stdout.strip()
    if not remote_dir.startswith("/tmp/mesh-talk-tailnet-e2e."):
        raise AssertionError(f"Unexpected remote temporary directory: {remote_dir}")
    nodes = []
    try:
        subprocess.run(["scp", "-q", str(REMOTE_BINARY), f"{remote_host}:{remote_dir}/mesh-talk-node"],
                       check=True)
        ssh(remote_host, f"chmod 700 {shlex.quote(remote_dir)}/mesh-talk-node")
        with tempfile.TemporaryDirectory(prefix="mesh-talk-tailnet-e2e-") as local_dir:
            local_dir = Path(local_dir)
            payload = os.urandom(4096)
            fixture = local_dir / "tailnet-payload.bin"
            fixture.write_bytes(payload)
            port_local = random.randint(49000, 59000)
            port_remote = port_local + 1

            local = Node("local", [str(LOCAL_BINARY), "--keystore",
                str(local_dir / "identity.keystore"), "--password", "e2e-only",
                "--name", "Tailnet Local", "--discovery-port", str(port_local)])
            nodes.append(local)
            local_id = re.match(r"node ([0-9a-f]+) listening ",
                                local.expect(lambda line: line.startswith("node "), 120)).group(1)
            remote_command = "exec " + " ".join(map(shlex.quote, [
                f"{remote_dir}/mesh-talk-node", "--keystore", f"{remote_dir}/identity.keystore",
                "--password", "e2e-only", "--name", "Tailnet Remote",
                "--discovery-port", str(port_remote),
            ]))
            remote = Node("remote", ["ssh", "-T", "-o", "BatchMode=yes",
                "-o", "StrictHostKeyChecking=yes", remote_host, remote_command])
            nodes.append(remote)
            remote_id = re.match(r"node ([0-9a-f]+) listening ",
                                 remote.expect(lambda line: line.startswith("node "), 120)).group(1)

            # Allow the startup multicast/scan burst to settle before the
            # one-shot diagnostic query.
            time.sleep(3)
            local.send(f"/probe {remote_ip}:{port_remote}")
            remote.send(f"/probe {local_ip}:{port_local}")
            await_peer(local, remote_id, remote_ip, f"{remote_ip}:{port_remote}")
            await_peer(remote, local_id, local_ip, f"{local_ip}:{port_local}")
            print("PASS: signed routed discovery on Tailscale IPs")

            token = f"tailnet-dm-{random.getrandbits(64):016x}"
            local.send(f"/msg {remote_id[:8]} {token}")
            remote.expect(lambda line: line.startswith(f"from {local_id} ") and token in line, 30)
            remote.send(f"/msg {local_id[:8]} reply-{token}")
            local.expect(lambda line: line.startswith(f"from {remote_id} ") and f"reply-{token}" in line, 30)
            print("PASS: Noise-encrypted DMs in both directions")

            local.send(f"/sendfile {remote_id[:8]} {fixture}")
            offer = remote.expect(lambda line: line.startswith(f"file offer from {local_id}:"), 30)
            file_id = re.search(r" id ([0-9a-f]{64})", offer).group(1)
            remote.send(f"/receive {file_id}")
            saved = remote.expect(lambda line: line.startswith(f"file from {local_id}:")
                                  and " saved " in line, 60)
            saved_path = saved.split(" saved ", 1)[1]
            actual = ssh(remote_host, f"shasum -a 256 {shlex.quote(saved_path)}",
                         capture_output=True, text=True).stdout.split()[0]
            if actual != hashlib.sha256(payload).hexdigest():
                raise AssertionError("Transferred file hash mismatch")
            print("PASS: consented file transfer and SHA-256 match")

            # Exercise the desktop host adapter and runtime together. The
            # ignored Rust test requires this remote node to stay online.
            environment = os.environ.copy()
            environment["MESH_TALK_E2E_REMOTE_IP"] = remote_ip
            environment["MESH_TALK_E2E_DISCOVERY_PORT"] = str(port_remote)
            subprocess.run([
                "cargo", "test", "-p", "mesh-talk", "--lib",
                "live_tailnet_inventory_automatically_discovers_remote_node",
                "--features", "mesh-talk-core/fast-test-kdf", "--", "--ignored",
            ], cwd=ROOT, env=environment, check=True)
            print("PASS: desktop Tailscale inventory triggered automatic routed discovery")
    finally:
        for node in reversed(nodes):
            node.stop()
        ssh(remote_host, f"rm -rf -- {shlex.quote(remote_dir)}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("Usage: python3 scripts/e2e-tailnet-macos.py <ssh-host>")
    main(sys.argv[1])
