#!/usr/bin/env python3
"""Two-Mac EasyTier E2E: signed discovery, encrypted DMs, consented files.

Build the local and x86_64 mesh-talk-node binaries with fast-test-kdf first.
Usage: python3 scripts/e2e-easytier-macos.py mac2015 10.253.77.1 10.253.77.2

Both EasyTier instances must already be connected. The test first exercises
automatic desktop inventory discovery and messaging, then explicit CLI probes.
Distinct discovery ports and virtual-IP route checks exclude LAN coincidence. SSH controls
the remote process; application payloads travel through EasyTier. Disposable
identities and files are removed; installed services are left running.
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


def ssh(host, command, **kwargs):
    return subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", host, command],
        check=True,
        **kwargs,
    )


def verify_easytier_inventory(host, expected_ip):
    if host:
        result = ssh(host, "~/.local/bin/easytier-cli -p 127.0.0.1:15888 -o json route",
                     capture_output=True, text=True)
    else:
        result = subprocess.run(
            ["easytier-cli", "-p", "127.0.0.1:15888", "-o", "json", "route"],
            capture_output=True, text=True, check=True)
    inventory = json.loads(result.stdout)

    def addresses(value):
        if isinstance(value, dict):
            return [ip for item in value.values() for ip in addresses(item)]
        if isinstance(value, list):
            return [ip for item in value for ip in addresses(item)]
        if isinstance(value, str):
            return [value.split("/")[0]]
        return []

    if expected_ip not in addresses(inventory):
        raise AssertionError(f"EasyTier route to {expected_ip} missing on {host or 'local Mac'}")
    command = ["route", "-n", "get", expected_ip]
    if host:
        route = ssh(host, " ".join(map(shlex.quote, command)), capture_output=True, text=True)
    else:
        route = subprocess.run(command, capture_output=True, text=True, check=True)
    if not re.search(r"interface: utun[0-9]+", route.stdout):
        raise AssertionError(f"Expected a TUN route to {expected_ip}: {route.stdout}")



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


def main(remote_host, local_ip, remote_ip):
    if not LOCAL_BINARY.is_file() or not REMOTE_BINARY.is_file():
        raise SystemExit("Build the arm64 and x86_64 test binaries shown in the docstring first")
    verify_easytier_inventory(None, remote_ip)
    verify_easytier_inventory(remote_host, local_ip)
    print(f"EasyTier endpoints: local {local_ip}, {remote_host} {remote_ip}")

    remote_dir = ssh(remote_host, "mktemp -d /tmp/mesh-talk-easytier-e2e.XXXXXX",
                     capture_output=True, text=True).stdout.strip()
    if not remote_dir.startswith("/tmp/mesh-talk-easytier-e2e."):
        raise AssertionError(f"Unexpected remote temporary directory: {remote_dir}")
    nodes = []
    try:
        subprocess.run(["scp", "-q", str(REMOTE_BINARY), f"{remote_host}:{remote_dir}/mesh-talk-node"],
                       check=True)
        ssh(remote_host, f"chmod 700 {shlex.quote(remote_dir)}/mesh-talk-node")
        with tempfile.TemporaryDirectory(prefix="mesh-talk-easytier-e2e-") as local_dir:
            local_dir = Path(local_dir)
            payload = os.urandom(130000)
            fixture = local_dir / "easytier-payload.bin"
            fixture.write_bytes(payload)
            port_local = random.randint(49000, 59000)
            port_remote = port_local + 1

            local = Node("local", [str(LOCAL_BINARY), "--keystore",
                str(local_dir / "identity.keystore"), "--password", "e2e-only",
                "--name", "EasyTier Local", "--discovery-port", str(port_local)])
            nodes.append(local)
            local_id = re.match(r"node ([0-9a-f]+) listening ",
                                local.expect(lambda line: line.startswith("node "), 120)).group(1)
            remote_command = "exec " + " ".join(map(shlex.quote, [
                f"{remote_dir}/mesh-talk-node", "--keystore", f"{remote_dir}/identity.keystore",
                "--password", "e2e-only", "--name", "EasyTier Remote",
                "--discovery-port", str(port_remote),
            ]))
            remote = Node("remote", ["ssh", "-T", "-o", "BatchMode=yes",
                "-o", "StrictHostKeyChecking=yes", remote_host, remote_command])
            nodes.append(remote)
            remote_id = re.match(r"node ([0-9a-f]+) listening ",
                                 remote.expect(lambda line: line.startswith("node "), 120)).group(1)

            # A fresh desktop-host runtime must discover and message this node
            # using actual provider inventory, before either CLI sends /probe.
            automatic_token = f"automatic-easytier-{random.getrandbits(64):016x}"
            environment = os.environ.copy()
            environment["MESH_TALK_E2E_REMOTE_IP"] = remote_ip
            environment["MESH_TALK_E2E_REMOTE_ID"] = remote_id
            environment["MESH_TALK_E2E_DISCOVERY_PORT"] = str(port_remote)
            environment["MESH_TALK_E2E_AUTOMATIC_TOKEN"] = automatic_token
            subprocess.run([
                "cargo", "test", "-p", "mesh-talk", "--lib",
                "live_easytier_inventory_automatically_discovers_remote_node",
                "--", "--ignored", "--nocapture",
            ], cwd=ROOT, env=environment, check=True)
            remote.expect(lambda line: line.startswith("from ")
                          and "(Automatic EasyTier Probe):" in line
                          and automatic_token in line, 30)
            print("PASS: production desktop inventory automatically discovered and messaged the EasyTier peer")

            # Allow the startup multicast/scan burst to settle before the
            # one-shot diagnostic query.
            time.sleep(3)
            local.send(f"/probe {remote_ip}:{port_remote}")
            remote.send(f"/probe {local_ip}:{port_local}")
            await_peer(local, remote_id, remote_ip, f"{remote_ip}:{port_remote}")
            await_peer(remote, local_id, local_ip, f"{local_ip}:{port_local}")
            print("PASS: signed routed discovery on EasyTier virtual IPs")

            token = f"easytier-dm-{random.getrandbits(64):016x}"
            local.send(f"/msg {remote_id[:8]} {token}")
            remote.expect(lambda line: line.startswith(f"from {local_id} ") and token in line, 30)
            remote.send(f"/msg {local_id[:8]} reply-{token}")
            local.expect(lambda line: line.startswith(f"from {remote_id} ") and f"reply-{token}" in line, 30)
            print("PASS: Noise-encrypted DMs in both directions")

            local.send(f"/channel-new easytier-group {remote_id[:8]}")
            created = local.expect(
                lambda line: line.startswith("channel ") and line.endswith(" created"), 30)
            channel_id = created.split()[1]
            local.send(f"/channel-msg {channel_id[:8]} group-{token}")
            remote.expect(lambda line: line.startswith(f"channel {channel_id} from {local_id}:")
                          and f"group-{token}" in line, 30)
            remote.send(f"/channel-msg {channel_id[:8]} group-reply-{token}")
            local.expect(lambda line: line.startswith(f"channel {channel_id} from {remote_id}:")
                         and f"group-reply-{token}" in line, 30)
            print("PASS: encrypted group messages in both directions")

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

            remote.send(f"/sendfile {local_id[:8]} {saved_path}")
            reverse_offer = local.expect(
                lambda line: line.startswith(f"file offer from {remote_id}:"), 30)
            reverse_id = re.search(r" id ([0-9a-f]{64})", reverse_offer).group(1)
            local.send(f"/receive {reverse_id}")
            reverse_saved = local.expect(
                lambda line: line.startswith(f"file from {remote_id}:") and " saved " in line, 60)
            reverse_path = Path(reverse_saved.split(" saved ", 1)[1])
            if hashlib.sha256(reverse_path.read_bytes()).hexdigest() != hashlib.sha256(payload).hexdigest():
                raise AssertionError("Reverse transferred file hash mismatch")
            print("PASS: reverse consented file transfer and SHA-256 match")

    finally:
        for node in reversed(nodes):
            node.stop()
        ssh(remote_host, f"rm -rf -- {shlex.quote(remote_dir)}")


if __name__ == "__main__":
    if len(sys.argv) != 4:
        raise SystemExit("Usage: python3 scripts/e2e-easytier-macos.py <ssh-host> <local-ip> <remote-ip>")
    main(*sys.argv[1:])
