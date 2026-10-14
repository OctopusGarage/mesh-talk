#!/usr/bin/env python3
"""A/B/E on this Mac, C on a second Mac; E bridges discovery and opaque DMs.

Build the test binaries first:
  cargo build -p mesh-talk-core --bin mesh-talk-node --features fast-test-kdf
  RUSTC=$(rustup which rustc) rustup run stable cargo build -p mesh-talk-core \
      --bin mesh-talk-node --features fast-test-kdf --target x86_64-apple-darwin
Then run: python3 scripts/e2e-tailnet-bridge.py mac2015
"""

import importlib.util
from pathlib import Path
import random
import re
import shlex
import subprocess
import sys
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "scripts/e2e-tailnet-macos.py"
SPEC = importlib.util.spec_from_file_location("tailnet_e2e", MODULE_PATH)
TAILNET = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TAILNET)


def peer(node, fingerprint, timeout=50):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        node.send("/peers")
        try:
            return node.expect(lambda line: line.startswith(f"peer {fingerprint} "), 1)
        except AssertionError:
            continue
    raise AssertionError(f"{node.name} did not discover {fingerprint}")


def probe_until(node, fingerprint, endpoint):
    end = time.monotonic() + 35
    while time.monotonic() < end:
        node.send(f"/probe {endpoint}")
        try:
            return peer(node, fingerprint, 2)
        except AssertionError:
            continue
    raise AssertionError(f"{node.name} could not discover relay {endpoint}")


def node_id(node, prefix):
    return re.match(
        rf"{prefix} ([0-9a-f]+) listening ",
        node.expect(lambda line: line.startswith(f"{prefix} "), 120),
    ).group(1)


def main(host):
    local_binary = TAILNET.LOCAL_BINARY
    remote_binary = TAILNET.REMOTE_BINARY
    if not local_binary.is_file() or not remote_binary.is_file():
        raise SystemExit("Build both test binaries shown in the script header first")
    _, local_ip = TAILNET.tailscale_status()
    _, remote_ip = TAILNET.tailscale_status(host)
    print(f"Tailnet: E {local_ip}, C {remote_ip}", flush=True)
    remote_dir = TAILNET.ssh(
        host, "mktemp -d /tmp/mesh-talk-bridge-e2e.XXXXXX", capture_output=True, text=True
    ).stdout.strip()
    if not remote_dir.startswith("/tmp/mesh-talk-bridge-e2e."):
        raise AssertionError("Remote temp path was unexpected")
    nodes = []
    try:
        subprocess.run(
            ["scp", "-q", str(remote_binary), f"{host}:{remote_dir}/mesh-talk-node"],
            check=True,
        )
        TAILNET.ssh(host, f"chmod 700 {shlex.quote(remote_dir)}/mesh-talk-node")
        with tempfile.TemporaryDirectory(prefix="mesh-talk-bridge-e2e-") as local_dir:
            base = random.randint(50000, 59000)
            root = Path(local_dir)

            def local(name, port, flags):
                directory = root / name
                directory.mkdir()
                result = TAILNET.Node(name, [
                    str(local_binary), "--keystore", str(directory / "identity.keystore"),
                    "--password", "e2e-only", "--name", name,
                    "--discovery-port", str(port), *flags,
                ])
                nodes.append(result)
                return result

            e = local("E", base, ["--post-office", "--bridge"])
            e_id = node_id(e, "post-office")
            a = local("A", base + 1, ["--bridge-discovery"])
            a_id = node_id(a, "node")
            b = local("B", base + 2, ["--bridge-discovery"])
            b_id = node_id(b, "node")
            remote_args = [
                f"{remote_dir}/mesh-talk-node", "--keystore",
                f"{remote_dir}/identity.keystore", "--password", "e2e-only",
                "--name", "C", "--discovery-port", str(base + 3),
                "--bridge-discovery",
            ]
            c = TAILNET.Node("C", [
                "ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
                host, "exec " + " ".join(map(shlex.quote, remote_args)),
            ])
            nodes.append(c)
            c_id = node_id(c, "node")

            probe_until(a, e_id, f"127.0.0.1:{base}")
            probe_until(b, e_id, f"127.0.0.1:{base}")
            probe_until(c, e_id, f"{local_ip}:{base}")
            probe_until(a, b_id, f"127.0.0.1:{base + 2}")
            probe_until(b, a_id, f"127.0.0.1:{base + 1}")
            peer(c, a_id)
            peer(c, b_id)
            peer(a, c_id)
            peer(b, c_id)
            print("PASS: E introduced A/B to C and C to A/B", flush=True)

            c.send(f"/msg {a_id[:8]} bridge-C-to-A")
            a.expect(lambda line: "bridge-C-to-A" in line, 45)
            b.send(f"/msg {c_id[:8]} bridge-B-to-C")
            c.expect(lambda line: "bridge-B-to-C" in line, 45)
            print("PASS: both cross-LAN DMs delivered through E", flush=True)
    finally:
        for node in reversed(nodes):
            node.stop()
        TAILNET.ssh(host, f"rm -rf -- {shlex.quote(remote_dir)}")


if __name__ == "__main__":
    if len(sys.argv) != 2:
        raise SystemExit("usage: e2e-tailnet-bridge.py <ssh-host>")
    main(sys.argv[1])
