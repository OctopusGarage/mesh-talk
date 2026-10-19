#!/usr/bin/env python3
"""E/F are the only tailnet members; A/B and C/D stay on their own Macs.

Build both binaries first:
  cargo build -p mesh-talk-core --bin mesh-talk-node --features fast-test-kdf
  cargo build -p mesh-talk-core --bin mesh-talk-node --features fast-test-kdf \
      --target x86_64-apple-darwin
Run: python3 scripts/e2e-tailnet-two-bridges.py mac2015
With Windows B on LAN X, build the Windows GNU binary with the recipe in
docs/post-office-windows-service.md and run:
  python3 scripts/e2e-tailnet-two-bridges.py mac2015 --windows-host windows10
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
SPEC = importlib.util.spec_from_file_location("tailnet_e2e", ROOT / "scripts/e2e-tailnet-macos.py")
TAILNET = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TAILNET)


def identity(node, role):
    line = node.expect(lambda text: text.startswith(role + " "), 120)
    return re.match(rf"{role} ([0-9a-f]{{32}}) listening ", line).group(1)


def peer(node, fingerprint, timeout=90):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        node.send("/peers")
        try:
            return node.expect(lambda line: line.startswith(f"peer {fingerprint} "), 1)
        except AssertionError:
            pass
    raise AssertionError(f"{node.name} did not discover {fingerprint}")


def probe_local(node, fingerprint, discovery_port):
    end = time.monotonic() + 40
    while time.monotonic() < end:
        node.send(f"/probe 127.0.0.1:{discovery_port}")
        try:
            return peer(node, fingerprint, 2)
        except AssertionError:
            pass
    raise AssertionError(f"{node.name} did not discover its local post office")


def windows_ssh(host, command):
    encoded = base64.b64encode(command.encode("utf-16le")).decode("ascii")
    return subprocess.run([
        "ssh", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes", host,
        "powershell -NoProfile -EncodedCommand " + encoded,
    ], check=True, capture_output=True, text=True, errors="replace")


def main(host, windows_host=None):
    if not TAILNET.LOCAL_BINARY.is_file() or not TAILNET.REMOTE_BINARY.is_file():
        raise SystemExit("Build both binaries shown in the script header first")
    windows_binary = ROOT / "target/x86_64-pc-windows-gnu/debug/mesh-talk-node.exe"
    if windows_host and not windows_binary.is_file():
        raise SystemExit("Build the Windows GNU test binary first")
    _, e_ip = TAILNET.tailscale_status()
    _, f_ip = TAILNET.tailscale_status(host)
    remote_dir = TAILNET.ssh(
        host, "mktemp -d /tmp/mesh-talk-two-bridges.XXXXXX",
        capture_output=True, text=True,
    ).stdout.strip()
    if not remote_dir.startswith("/tmp/mesh-talk-two-bridges."):
        raise AssertionError("Unexpected remote temporary path")
    nodes = []
    remote_names = set()
    windows_dir = None
    firewall_name = None
    try:
        subprocess.run([
            "scp", "-q", str(TAILNET.REMOTE_BINARY), f"{host}:{remote_dir}/mesh-talk-node",
        ], check=True)
        TAILNET.ssh(host, f"chmod 700 {shlex.quote(remote_dir)}/mesh-talk-node")
        with tempfile.TemporaryDirectory(prefix="mesh-talk-two-bridges-") as local_dir:
            root = Path(local_dir)
            base = random.randint(49000, 55000)
            e_udp, f_udp = base, base + 1
            e_tcp, f_tcp = base + 2, base + 3

            def local(name, udp, tcp=0, extra=()):
                folder = root / name
                folder.mkdir(exist_ok=True)
                node = TAILNET.Node(name, [
                    str(TAILNET.LOCAL_BINARY), "--keystore",
                    str(folder / "identity.keystore"), "--password", "e2e-only",
                    "--name", name, "--discovery-port", str(udp),
                    "--port", str(tcp), *extra,
                ])
                nodes.append(node)
                return node

            def remote(name, udp, tcp=0, extra=()):
                remote_names.add(name)
                args = [
                    f"{remote_dir}/mesh-talk-node", "--keystore",
                    f"{remote_dir}/{name}/identity.keystore", "--password", "e2e-only",
                    "--name", name, "--discovery-port", str(udp),
                    "--port", str(tcp), *extra,
                ]
                command = (f"mkdir -p {shlex.quote(remote_dir + '/' + name)}; "
                           f"echo $$ > {shlex.quote(remote_dir + '/' + name + '.pid')}; exec ") + \
                    " ".join(map(shlex.quote, args))
                node = TAILNET.Node(name, [
                    "ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
                    host, command,
                ])
                nodes.append(node)
                return node

            def stop_remote(name):
                pid_path = shlex.quote(remote_dir + "/" + name + ".pid")
                result = TAILNET.ssh(host, f"cat {pid_path} 2>/dev/null || true",
                                         capture_output=True, text=True)
                pid = result.stdout.strip()
                if not pid.isdecimal():
                    return
                command = TAILNET.ssh(
                    host, f"ps -p {pid} -o command= || true",
                    capture_output=True, text=True,
                ).stdout
                if (remote_dir + "/mesh-talk-node" in command and
                        remote_dir + "/" + name + "/identity.keystore" in command):
                    TAILNET.ssh(host, f"kill {pid}")

            # First boot obtains each relay's persistent Mesh Talk fingerprint.
            e = local("E", e_udp, e_tcp, ("--post-office", "--bridge"))
            e_id = identity(e, "post-office")
            f = remote("F", f_udp, f_tcp, ("--post-office", "--bridge"))
            f_id = identity(f, "post-office")
            e.stop()
            f.stop()
            stop_remote("F")
            nodes.remove(e)
            nodes.remove(f)

            e = local("E", e_udp, e_tcp, (
                "--post-office", "--bridge", "--bridge-peer", f"{f_ip}:{f_udp}",
                "--bridge-peer-id", f_id,
            ))
            identity(e, "post-office")
            f = remote("F", f_udp, f_tcp, (
                "--post-office", "--bridge", "--bridge-peer", f"{e_ip}:{e_udp}",
                "--bridge-peer-id", e_id,
            ))
            identity(f, "post-office")

            a = local("A", base + 4, extra=("--bridge-discovery",))
            a_id = identity(a, "node")
            if windows_host:
                windows_dir = windows_ssh(
                    windows_host,
                    f"$path = Join-Path 'D:\\' 'mesh-talk-two-bridges-{base}'; "
                    "New-Item -ItemType Directory -Force -Path $path | Out-Null; "
                    "Write-Output $path",
                ).stdout.strip().replace("\\", "/")
                if not re.match(r"^D:/mesh-talk-two-bridges-\d+$", windows_dir):
                    raise AssertionError("Unexpected Windows temporary directory")
                subprocess.run([
                    "scp", "-q", str(windows_binary),
                    f"{windows_host}:{windows_dir}/mesh-talk-node.exe",
                ], check=True)
                firewall_name = f"MeshTalk Two Bridges E2E {base}"
                windows_ssh(
                    windows_host,
                    f"New-NetFirewallRule -DisplayName '{firewall_name}' "
                    "-Direction Inbound -Action Allow -Protocol UDP "
                    f"-LocalPort {base + 5} -RemoteAddress LocalSubnet | Out-Null",
                )
                args = [
                    f"{windows_dir}/mesh-talk-node.exe", "--keystore",
                    f"{windows_dir}/identity.keystore", "--password", "e2e-only",
                    "--name", "B", "--discovery-port", str(base + 5),
                    "--bridge-discovery",
                ]
                command = "& '" + args[0] + "' " + " ".join(
                    "'" + arg.replace("'", "''") + "'" for arg in args[1:]
                )
                encoded = base64.b64encode(command.encode("utf-16le")).decode("ascii")
                b = TAILNET.Node("B-Windows", [
                    "ssh", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
                    windows_host, "powershell -NoProfile -EncodedCommand " + encoded,
                ])
                nodes.append(b)
            else:
                b = local("B", base + 5, extra=("--bridge-discovery",))
            b_id = identity(b, "node")
            c = remote("C", base + 6, extra=("--bridge-discovery",))
            c_id = identity(c, "node")
            d = remote("D", base + 7, extra=("--bridge-discovery",))
            d_id = identity(d, "node")

            probe_local(a, e_id, e_udp)
            if windows_host:
                local_ip = subprocess.run(
                    ["ipconfig", "getifaddr", "en0"], check=True,
                    capture_output=True, text=True,
                ).stdout.strip()
                end = time.monotonic() + 40
                while time.monotonic() < end:
                    b.send(f"/probe {local_ip}:{e_udp}")
                    try:
                        peer(b, e_id, 2)
                        break
                    except AssertionError:
                        pass
                else:
                    raise AssertionError("Windows B did not discover E on LAN X")
            else:
                probe_local(b, e_id, e_udp)
            for node in (c, d):
                probe_local(node, f_id, f_udp)
            for node in (a, b, c, d):
                peer(node, e_id)
                peer(node, f_id)
            for node in (a, b):
                peer(node, c_id)
                peer(node, d_id)
            for node in (c, d):
                peer(node, a_id)
                peer(node, b_id)
            for node, own in ((a, a_id), (b, b_id), (c, c_id), (d, d_id)):
                for target in (a_id, b_id, c_id, d_id):
                    if target != own:
                        peer(node, target)
            print("PASS: A/B and C/D discover one another through E/F", flush=True)

            clients = (("A", a, a_id), ("B", b, b_id),
                       ("C", c, c_id), ("D", d, d_id))
            for sender_name, sender, sender_id in clients:
                for recipient_name, recipient, recipient_id in clients:
                    if sender_id == recipient_id:
                        continue
                    token = f"two-bridges-{sender_name}-to-{recipient_name}-{base}"
                    sender.send(f"/msg {recipient_id[:8]} {token}")
                    recipient.expect(lambda line, token=token: token in line, 90)
                    print(f"PASS: {sender_name} -> {recipient_name} encrypted DM", flush=True)
            print("PASS: all 12 directed encrypted DM paths", flush=True)
    finally:
        for node in reversed(nodes):
            node.stop()
        if windows_host and firewall_name:
            windows_ssh(
                windows_host,
                f"Get-NetFirewallRule -DisplayName '{firewall_name}' "
                "-ErrorAction SilentlyContinue | Remove-NetFirewallRule",
            )
        if windows_host and windows_dir:
            windows_ssh(
                windows_host,
                f"Remove-Item -LiteralPath '{windows_dir}' -Recurse -Force "
                "-ErrorAction SilentlyContinue",
            )
        for name in remote_names:
            pid_path = shlex.quote(remote_dir + "/" + name + ".pid")
            pid = TAILNET.ssh(
                host, f"cat {pid_path} 2>/dev/null || true",
                capture_output=True, text=True,
            ).stdout.strip()
            if pid.isdecimal():
                command = TAILNET.ssh(
                    host, f"ps -p {pid} -o command= || true",
                    capture_output=True, text=True,
                ).stdout
                if (remote_dir + "/mesh-talk-node" in command and
                        remote_dir + "/" + name + "/identity.keystore" in command):
                    TAILNET.ssh(host, f"kill {pid}")
        TAILNET.ssh(host, f"rm -rf -- {shlex.quote(remote_dir)}")


if __name__ == "__main__":
    if len(sys.argv) not in (2, 4) or (len(sys.argv) == 4 and sys.argv[2] != "--windows-host"):
        raise SystemExit(
            "Usage: python3 scripts/e2e-tailnet-two-bridges.py <ssh-host> "
            "[--windows-host <ssh-host>]"
        )
    main(sys.argv[1], sys.argv[3] if len(sys.argv) == 4 else None)
