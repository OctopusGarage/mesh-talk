#!/usr/bin/env python3
"""A/E on this Mac, C on a second Mac, optional B on Windows.

Build the test binaries first:
  cargo build -p mesh-talk-core --bin mesh-talk-node --features fast-test-kdf
  RUSTC=$(rustup which rustc) rustup run stable cargo build -p mesh-talk-core \
      --bin mesh-talk-node --features fast-test-kdf --target x86_64-apple-darwin
Then run: python3 scripts/e2e-tailnet-bridge.py mac2015
For the three-machine scenario, cross-build target/x86_64-pc-windows-gnu/debug/
mesh-talk-node.exe with --features fast-test-kdf (use the Windows build recipe
in docs/post-office-windows-service.md, without --release), then run:
  python3 scripts/e2e-tailnet-bridge.py mac2015 --windows-host windows10
"""

import base64
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


def windows_ssh(host, command):
    encoded = base64.b64encode(command.encode("utf-16le")).decode("ascii")
    return subprocess.run(
        ["ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", host,
         "powershell -NoProfile -EncodedCommand " + encoded],
        check=True, capture_output=True, text=True, errors="replace",
    )


def main(host, windows_host=None):
    local_binary = TAILNET.LOCAL_BINARY
    remote_binary = TAILNET.REMOTE_BINARY
    if not local_binary.is_file() or not remote_binary.is_file():
        raise SystemExit("Build both test binaries shown in the script header first")
    windows_binary = ROOT / "target/x86_64-pc-windows-gnu/debug/mesh-talk-node.exe"
    if windows_host and not windows_binary.is_file():
        raise SystemExit("Build the Windows GNU test binary shown in the script header first")
    _, local_ip = TAILNET.tailscale_status()
    _, remote_ip = TAILNET.tailscale_status(host)
    print(f"Tailnet: E {local_ip}, C {remote_ip}", flush=True)
    remote_dir = TAILNET.ssh(
        host, "mktemp -d /tmp/mesh-talk-bridge-e2e.XXXXXX", capture_output=True, text=True
    ).stdout.strip()
    if not remote_dir.startswith("/tmp/mesh-talk-bridge-e2e."):
        raise AssertionError("Remote temp path was unexpected")
    nodes = []
    windows_dir = None
    firewall_name = None
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
            if windows_host:
                windows_dir = windows_ssh(
                    windows_host,
                    f"$path = Join-Path 'D:\\' 'mesh-talk-bridge-e2e-{base}'; "
                    "New-Item -ItemType Directory -Force -Path $path | Out-Null; "
                    "Write-Output $path",
                ).stdout.strip().replace("\\", "/")
                if not re.match(r"^D:/mesh-talk-bridge-e2e-\d+$", windows_dir):
                    raise AssertionError("Unexpected Windows temporary directory")
                subprocess.run(
                    ["scp", "-q", str(windows_binary),
                     f"{windows_host}:{windows_dir}/mesh-talk-node.exe"], check=True,
                )
                firewall_name = f"MeshTalk Bridge E2E {base}"
                for protocol, port in [("UDP", base + 2), ("TCP", base + 12)]:
                    windows_ssh(
                        windows_host,
                        f"New-NetFirewallRule -DisplayName '{firewall_name}' -Direction Inbound "
                        f"-Action Allow -Protocol {protocol} -LocalPort {port} "
                        "-RemoteAddress LocalSubnet | Out-Null",
                    )
                windows_args = [
                    f"{windows_dir}/mesh-talk-node.exe", "--keystore",
                    f"{windows_dir}/identity.keystore", "--password", "e2e-only",
                    "--name", "B", "--discovery-port", str(base + 2),
                    "--port", str(base + 12), "--bridge-discovery",
                ]
                powershell_command = "& '" + windows_args[0] + "' " + " ".join(
                    "'" + arg + "'" for arg in windows_args[1:]
                )
                encoded = base64.b64encode(powershell_command.encode("utf-16le")).decode("ascii")
                b = TAILNET.Node("B-Windows", [
                    "ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
                    windows_host, "powershell -NoProfile -EncodedCommand " + encoded,
                ])
            else:
                b = local("B", base + 2, ["--bridge-discovery"])
            if windows_host:
                nodes.append(b)
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
            lan_ip = "127.0.0.1"
            if windows_host:
                lan_ip = subprocess.run(
                    ["ipconfig", "getifaddr", "en0"], check=True, capture_output=True, text=True,
                ).stdout.strip()
            probe_until(b, e_id, f"{lan_ip}:{base}")
            probe_until(c, e_id, f"{local_ip}:{base}")
            b_ip = "127.0.0.1"
            if windows_host:
                b_ip = windows_ssh(
                    windows_host,
                    "(Get-NetIPAddress -InterfaceAlias WLAN -AddressFamily IPv4).IPAddress",
                ).stdout.strip()
            probe_until(a, b_id, f"{b_ip}:{base + 2}")
            probe_until(b, a_id, f"{lan_ip}:{base + 1}")
            peer(c, a_id)
            peer(c, b_id)
            peer(a, c_id)
            peer(b, c_id)
            print("PASS: E introduced A/B to C and C to A/B", flush=True)

            c.send(f"/msg {a_id[:8]} bridge-C-to-A")
            a.expect(lambda line: "bridge-C-to-A" in line, 45)
            b.send(f"/msg {c_id[:8]} bridge-B-to-C")
            c.expect(lambda line: "bridge-B-to-C" in line, 45)
            a.send(f"/msg {c_id[:8]} bridge-A-to-C")
            c.expect(lambda line: "bridge-A-to-C" in line, 45)
            c.send(f"/msg {b_id[:8]} bridge-C-to-B")
            b.expect(lambda line: "bridge-C-to-B" in line, 45)
            a.send(f"/msg {b_id[:8]} lan-A-to-B")
            b.expect(lambda line: "lan-A-to-B" in line, 45)
            b.send(f"/msg {a_id[:8]} lan-B-to-A")
            a.expect(lambda line: "lan-B-to-A" in line, 45)
            print("PASS: A/B LAN and both directions of A/C and B/C encrypted DMs", flush=True)

            # A bridge outage must leave the sender's event durable for retry.
            e.stop()
            c.send(f"/msg {b_id[:8]} queued-while-E-offline")
            e = TAILNET.Node("E-restarted", [
                str(local_binary), "--keystore", str(root / "E" / "identity.keystore"),
                "--password", "e2e-only", "--name", "E",
                "--discovery-port", str(base), "--post-office", "--bridge",
            ])
            nodes.append(e)
            e_start = e.expect(lambda line: line.startswith("post-office "), 120)
            if re.match(r"post-office ([0-9a-f]+) listening ", e_start).group(1) != e_id:
                raise AssertionError("Bridge identity changed after restart")
            restarted_port = re.search(r"tcp/(\d+)", e_start).group(1)
            for node, address in [(b, lan_ip), (c, local_ip)]:
                end = time.monotonic() + 35
                while time.monotonic() < end:
                    node.send(f"/probe {address}:{base}")
                    node.send("/peers")
                    try:
                        node.expect(
                            lambda line: line.startswith(f"peer {e_id} ")
                            and f"{address}:{restarted_port}" in line, 2,
                        )
                        break
                    except AssertionError:
                        continue
                else:
                    raise AssertionError(f"{node.name} retained the stale bridge port")
            b.expect(lambda line: "queued-while-E-offline" in line, 90)
            print("PASS: C-to-B DM survived bridge outage and same-identity restart", flush=True)
    finally:
        for node in reversed(nodes):
            node.stop()
        if windows_host:
            if firewall_name:
                windows_ssh(
                    windows_host,
                    f"Get-NetFirewallRule -DisplayName '{firewall_name}' "
                    "-ErrorAction SilentlyContinue | Remove-NetFirewallRule",
                )
            if windows_dir:
                windows_ssh(windows_host, f"Remove-Item -Recurse -Force '{windows_dir}'")
        TAILNET.ssh(host, f"rm -rf -- {shlex.quote(remote_dir)}")


if __name__ == "__main__":
    if len(sys.argv) not in (2, 4) or (len(sys.argv) == 4 and sys.argv[2] != "--windows-host"):
        raise SystemExit("usage: e2e-tailnet-bridge.py <ssh-host> [--windows-host <ssh-host>]")
    main(sys.argv[1], sys.argv[3] if len(sys.argv) == 4 else None)
