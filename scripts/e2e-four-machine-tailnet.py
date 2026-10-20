#!/usr/bin/env python3
"""Four physical machines: A/current Mac, B/Windows, E/mac2015, F+C+D/mac-mini.

E and F are pinned post-office bridges over their existing Tailscale path. A/B
reach E over the LAN; C/D reach F on mac-mini. Distinct discovery ports prevent
LAN multicast from satisfying cross-scope assertions. All identities are new
and temporary; the Windows firewall rule is removed on exit.

Build the arm64, x86_64 macOS, and Windows GNU debug CLI binaries with
--features fast-test-kdf before running this script. Then run
`python3 scripts/e2e-four-machine-tailnet.py` from this checkout. The script
uses SSH aliases mac2015/windows10 and reaches mac-mini through mac2015.
Set `MESH_TALK_E2E_FEDERATION=1` to use mutual relay federation instead of
the legacy gateway. `MESH_TALK_E2E_BASE_PORT` chooses a fixed test port range.
Set `MESH_TALK_E2E_INSTALLED_RELAYS=1` to test the installed mac2015 and
mac-mini services. Their pinned IDs can be overridden with
`MESH_TALK_E2E_MAC2015_RELAY_ID` and `MESH_TALK_E2E_MACMINI_RELAY_ID`.
`MESH_TALK_E2E_MAC2015_SSH` overrides the SSH alias when `.local` DNS fails.
Federation modes always verify consented cross-relay file bytes and SHA-256.
`MESH_TALK_E2E_BRIDGE_FILE=1` also enables that check in legacy gateway mode.
"""

import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import random
import re
import shlex
import subprocess
import tempfile
import time


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("tailnet_e2e", ROOT / "scripts/e2e-tailnet-macos.py")
TAILNET = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(TAILNET)
INTEL = ROOT / "target/x86_64-apple-darwin/debug/mesh-talk-node"
WINDOWS = ROOT / "target/x86_64-pc-windows-gnu/debug/mesh-talk-node.exe"
MAC2015 = os.environ.get("MESH_TALK_E2E_MAC2015_SSH", "mac2015")
MACMINI = "mac-mini"
WINDOWS_HOST = "windows10"


def mac2015(command):
    return TAILNET.ssh(MAC2015, command, capture_output=True, text=True).stdout.strip()


def macmini(command):
    inner = f"ssh -o BatchMode=yes {MACMINI} {shlex.quote(command)}"
    return TAILNET.ssh(MAC2015, inner, capture_output=True, text=True).stdout.strip()


def windows(command):
    encoded = base64.b64encode(command.encode("utf-16le")).decode("ascii")
    return subprocess.run(
        ["ssh", "-o", "BatchMode=yes", WINDOWS_HOST,
         "powershell -NoProfile -EncodedCommand " + encoded],
        check=True, capture_output=True, text=True, errors="replace",
    ).stdout.strip()


def remote_command(machine, command):
    if machine == MAC2015:
        return ["ssh", "-T", "-o", "BatchMode=yes", MAC2015, command]
    inner = f"ssh -T -o BatchMode=yes {MACMINI} {shlex.quote(command)}"
    return ["ssh", "-T", "-o", "BatchMode=yes", MAC2015, inner]


def identity(node, role):
    line = node.expect(lambda text: text.startswith(role + " "), 120)
    match = re.match(rf"{role} ([0-9a-f]{{32}}) listening ", line)
    if not match:
        raise AssertionError(f"Unexpected {role} startup line: {line}")
    return match.group(1)


def peer(node, fingerprint, timeout=120):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        node.send("/peers")
        try:
            return node.expect(lambda line: line.startswith(f"peer {fingerprint} "), 1)
        except AssertionError:
            pass
    raise AssertionError(f"{node.name} did not discover {fingerprint}")


def probe_until(node, fingerprint, endpoint):
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        node.send(f"/probe {endpoint}")
        try:
            return peer(node, fingerprint, 2)
        except AssertionError:
            pass
    raise AssertionError(f"{node.name} did not discover {fingerprint} at {endpoint}")


def run():
    installed = os.environ.get("MESH_TALK_E2E_INSTALLED_RELAYS") == "1"
    federated = installed or os.environ.get("MESH_TALK_E2E_FEDERATION") == "1"
    for binary in (TAILNET.LOCAL_BINARY, INTEL, WINDOWS):
        if not binary.is_file():
            raise SystemExit(f"Build the test binary first: {binary}")

    e_status, e_ip = TAILNET.tailscale_status(MAC2015)
    f_status = json.loads(macmini(
        f"TAILSCALE_BE_CLI=1 {shlex.quote(TAILNET.REMOTE_TAILSCALE)} status --json"
    ))
    if f_status.get("BackendState") != "Running":
        raise AssertionError("mac-mini Tailscale is not running")
    f_ip = next(ip for ip in f_status["TailscaleIPs"] if ip.startswith("100."))
    for status, other_ip in ((e_status, f_ip), (f_status, e_ip)):
        if not any(other_ip in item.get("TailscaleIPs", []) and item.get("Online")
                   for item in (status.get("Peer") or {}).values()):
            raise AssertionError(f"{other_ip} is not an online peer in both tailnets")
    e_lan_ip = mac2015("ipconfig getifaddr en0")
    a_lan_ip = subprocess.run(
        ["ipconfig", "getifaddr", "en0"], check=True, capture_output=True, text=True,
    ).stdout.strip()
    b_lan_ip = windows(
        "(Get-NetIPAddress -AddressFamily IPv4 -InterfaceAlias WLAN).IPAddress"
    ).splitlines()[-1].strip()
    print(f"Routes: A {a_lan_ip}, B {b_lan_ip}, E {e_lan_ip}/{e_ip}, F {f_ip}", flush=True)

    e_dir = mac2015("mktemp -d /tmp/mesh-talk-four-e2e.XXXXXX")
    f_dir = macmini("mktemp -d /tmp/mesh-talk-four-e2e.XXXXXX")
    if not all(path.startswith("/tmp/mesh-talk-four-e2e.") for path in (e_dir, f_dir)):
        raise AssertionError("Unexpected remote temporary directory")
    nodes = []
    remote_nodes = []
    windows_dir = None
    firewall_name = None

    def remote(machine, folder, name, udp, tcp, extra=()):
        directory = f"{folder}/{name}"
        binary = f"{folder}/mesh-talk-node"
        args = [binary, "--keystore", f"{directory}/identity.keystore",
                "--password", "e2e-only", "--name", name,
                "--discovery-port", str(udp), "--port", str(tcp), *extra]
        command = (f"mkdir -p {shlex.quote(directory)}; "
                   f"echo $$ > {shlex.quote(folder + '/' + name + '.pid')}; "
                   "exec " + " ".join(map(shlex.quote, args)))
        node = TAILNET.Node(name, remote_command(machine, command))
        nodes.append(node)
        remote_nodes.append((machine, folder, name))
        return node

    def stop_remote(machine, folder, name):
        run_host = mac2015 if machine == MAC2015 else macmini
        pid = run_host(f"cat {shlex.quote(folder + '/' + name + '.pid')} 2>/dev/null || true")
        if pid.isdecimal():
            command = run_host(f"ps -p {pid} -o command= || true")
            if f"{folder}/mesh-talk-node" in command and f"{folder}/{name}/identity.keystore" in command:
                run_host(f"kill {pid}")

    try:
        subprocess.run(["scp", "-q", str(INTEL), f"{MAC2015}:{e_dir}/mesh-talk-node"], check=True)
        subprocess.run(["scp", "-q", str(TAILNET.LOCAL_BINARY),
                        f"{MAC2015}:{e_dir}/mesh-talk-node-arm64"], check=True)
        mac2015(f"scp -q {shlex.quote(e_dir + '/mesh-talk-node-arm64')} "
                f"{MACMINI}:{shlex.quote(f_dir + '/mesh-talk-node')}")
        mac2015(f"chmod 700 {shlex.quote(e_dir + '/mesh-talk-node')}")
        macmini(f"chmod 700 {shlex.quote(f_dir + '/mesh-talk-node')}")

        with tempfile.TemporaryDirectory(prefix="mesh-talk-four-e2e-") as local_dir:
            base = int(os.environ.get("MESH_TALK_E2E_BASE_PORT", random.randint(49000, 55000)))
            if installed:
                e_udp, f_udp = 47478, 47478
                e_id = os.environ.get("MESH_TALK_E2E_MAC2015_RELAY_ID",
                                      "edbbf0daac94d930e687ba86c20d3f06")
                f_id = os.environ.get("MESH_TALK_E2E_MACMINI_RELAY_ID",
                                      "e2bc2167aab97af10cb893de4ae2fb0c")
            else:
                e_udp, f_udp, e_tcp, f_tcp = base, base + 1, base + 2, base + 3
                e = remote(MAC2015, e_dir, "E", e_udp, e_tcp, ("--post-office", "--bridge"))
                e_id = identity(e, "post-office")
                f = remote(MACMINI, f_dir, "F", f_udp, f_tcp, ("--post-office", "--bridge"))
                f_id = identity(f, "post-office")
                e.stop()
                f.stop()
                stop_remote(MAC2015, e_dir, "E")
                stop_remote(MACMINI, f_dir, "F")
                e_link = (("--federation-peer", f"{f_id}@{f_ip}:{f_udp}") if federated
                          else ("--bridge-peer", f"{f_ip}:{f_udp}", "--bridge-peer-id", f_id))
                e = remote(MAC2015, e_dir, "E", e_udp, e_tcp, ("--post-office", "--bridge", *e_link))
                identity(e, "post-office")
                f_link = (("--federation-peer", f"{e_id}@{e_ip}:{e_udp}") if federated
                          else ("--bridge-peer", f"{e_ip}:{e_udp}", "--bridge-peer-id", e_id))
                f = remote(MACMINI, f_dir, "F", f_udp, f_tcp, ("--post-office", "--bridge", *f_link))
                identity(f, "post-office")

            e_preference = ("--bridge-hub-id", e_id) if installed else ()
            f_preference = ("--bridge-hub-id", f_id) if installed else ()
            a_dir = Path(local_dir) / "A"
            a_dir.mkdir()
            a = TAILNET.Node("A", [str(TAILNET.LOCAL_BINARY), "--keystore",
                str(a_dir / "identity.keystore"), "--password", "e2e-only",
                "--name", "A", "--discovery-port", str(base + 4),
                "--port", str(base + 8), "--bridge-discovery", *e_preference])
            nodes.append(a)
            a_id = identity(a, "node")

            windows_dir = windows(
                f"$path = Join-Path 'D:\\' 'mesh-talk-four-e2e-{base}'; "
                "New-Item -ItemType Directory -Force -Path $path | Out-Null; Write-Output $path"
            ).splitlines()[-1].strip().replace("\\", "/")
            if not re.match(r"^D:/mesh-talk-four-e2e-\d+$", windows_dir):
                raise AssertionError(f"Unexpected Windows temp directory: {windows_dir}")
            subprocess.run(["scp", "-q", str(WINDOWS),
                            f"{WINDOWS_HOST}:{windows_dir}/mesh-talk-node.exe"], check=True)
            firewall_name = f"MeshTalk Four Machine E2E {base}"
            for protocol, port in (("UDP", base + 5), ("TCP", base + 9)):
                windows(
                    f"New-NetFirewallRule -DisplayName '{firewall_name}' "
                    f"-Direction Inbound -Action Allow -Protocol {protocol} "
                    f"-LocalPort {port} -RemoteAddress LocalSubnet | Out-Null"
                )
            b_args = [f"{windows_dir}/mesh-talk-node.exe", "--keystore",
                      f"{windows_dir}/identity.keystore", "--password", "e2e-only",
                      "--name", "B", "--discovery-port", str(base + 5),
                      "--port", str(base + 9), "--bridge-discovery", *e_preference]
            b_command = "& '" + b_args[0] + "' " + " ".join(
                "'" + arg.replace("'", "''") + "'" for arg in b_args[1:]
            )
            encoded = base64.b64encode(b_command.encode("utf-16le")).decode("ascii")
            b = TAILNET.Node("B-Windows", ["ssh", "-T", "-o", "BatchMode=yes",
                WINDOWS_HOST, "powershell -NoProfile -EncodedCommand " + encoded])
            nodes.append(b)
            b_id = identity(b, "node")

            c = remote(MACMINI, f_dir, "C", base + 6, base + 10,
                       ("--bridge-discovery", *f_preference))
            c_id = identity(c, "node")
            d = remote(MACMINI, f_dir, "D", base + 7, base + 11,
                       ("--bridge-discovery", *f_preference))
            d_id = identity(d, "node")

            probe_until(a, e_id, f"{e_lan_ip}:{e_udp}")
            probe_until(b, e_id, f"{e_lan_ip}:{e_udp}")
            probe_until(c, f_id, f"127.0.0.1:{f_udp}")
            probe_until(d, f_id, f"127.0.0.1:{f_udp}")
            probe_until(a, b_id, f"{b_lan_ip}:{base + 5}")
            probe_until(b, a_id, f"{a_lan_ip}:{base + 4}")
            probe_until(c, d_id, f"127.0.0.1:{base + 7}")
            probe_until(d, c_id, f"127.0.0.1:{base + 6}")
            clients = (("A", a, a_id), ("B", b, b_id), ("C", c, c_id), ("D", d, d_id))
            for name, node, own in clients:
                for _, _, other in clients:
                    if other != own:
                        peer(node, other)
                if federated:
                    peer(node, e_id if name in ("A", "B") else f_id)
                    print(f"PASS: {name} discovered all three clients and its local relay", flush=True)
                else:
                    peer(node, e_id)
                    peer(node, f_id)
                    print(f"PASS: {name} discovered all three clients and both relays", flush=True)

            for source_name, source, source_id in clients:
                for target_name, target, target_id in clients:
                    if source_id == target_id:
                        continue
                    token = f"four-machine-{source_name}-to-{target_name}-{base}"
                    source.send(f"/msg {target_id[:8]} {token}")
                    target.expect(lambda line, token=token, source_id=source_id:
                                  line.startswith(f"from {source_id} ") and token in line, 90)
                    print(f"PASS: {source_name} -> {target_name} encrypted DM", flush=True)
            print("PASS: all 12 directed encrypted DMs on four physical machines", flush=True)

            if federated:
                a.send(f"/channel-new federation-{base} {b_id[:8]} {c_id[:8]} {d_id[:8]}")
                group = a.expect(lambda line: re.match(r"^channel [0-9a-f]{64} created$", line), 90)
                group_id = group.split()[1]
                for sender_name, sender, sender_id in (("A", a, a_id), ("C", c, c_id)):
                    token = f"federated-group-{sender_name}-{base}"
                    sender.send(f"/channel-msg {group_id[:8]} {token}")
                    for target_name, target, target_id in clients:
                        if target_id == sender_id:
                            continue
                        target.expect(
                            lambda line, token=token, sender_id=sender_id: line.startswith(
                                f"channel {group_id} from {sender_id}:") and token in line,
                            90,
                        )
                    print(f"PASS: {sender_name} group event reached all three members", flush=True)

            payload = os.urandom(4096)
            fixture = Path(local_dir) / "bridge-file.bin"
            fixture.write_bytes(payload)
            bridge_file_error = None
            if not federated and os.environ.get("MESH_TALK_E2E_BRIDGE_FILE") != "1":
                print("SKIP: optional legacy gateway file check; set MESH_TALK_E2E_BRIDGE_FILE=1",
                      flush=True)
            else:
                try:
                    a.send(f"/sendfile {c_id[:8]} {fixture}")
                    offer = c.expect(lambda line: line.startswith(f"file offer from {a_id}:"), 90)
                    file_id = re.search(r" id ([0-9a-f]{64})", offer).group(1)
                    c.send(f"/receive {file_id}")
                    saved = c.expect(lambda line: line.startswith(f"file from {a_id}:")
                                     and " saved " in line, 120)
                    saved_path = saved.split(" saved ", 1)[1]
                    actual = macmini(f"shasum -a 256 {shlex.quote(saved_path)}").split()[0]
                    if actual != hashlib.sha256(payload).hexdigest():
                        raise AssertionError("A-to-C cross-relay file SHA-256 mismatch")
                    print("PASS: A -> C consented file bytes crossed both relays", flush=True)
                except (AssertionError, AttributeError, IndexError) as error:
                    bridge_file_error = error
                    print(f"FAIL: A -> C cross-relay file bytes: {error}", flush=True)

            p = remote(MAC2015, e_dir, "P", base + 14, base + 16)
            p_id = identity(p, "node")
            q = remote(MACMINI, f_dir, "Q", base + 15, base + 17)
            q_id = identity(q, "node")
            p_line = probe_until(p, q_id, f"{f_ip}:{base + 15}")
            q_line = probe_until(q, p_id, f"{e_ip}:{base + 14}")
            if f" {f_ip}:" not in p_line or f" {e_ip}:" not in q_line:
                raise AssertionError("Direct pair was discovered over a non-Tailscale path")
            print("PASS: direct signed routed discovery over mac2015/mac-mini Tailscale IPs",
                  flush=True)
            token = f"direct-tailnet-{base}"
            p.send(f"/msg {q_id[:8]} {token}")
            q.expect(lambda line: line.startswith(f"from {p_id} ") and token in line, 45)
            q.send(f"/msg {p_id[:8]} reply-{token}")
            p.expect(lambda line: line.startswith(f"from {q_id} ")
                     and f"reply-{token}" in line, 45)
            print("PASS: direct Noise-encrypted DMs in both directions", flush=True)
            direct_fixture = f"{e_dir}/P/direct-file.bin"
            subprocess.run(["scp", "-q", str(fixture), f"{MAC2015}:{direct_fixture}"],
                           check=True)
            p.send(f"/sendfile {q_id[:8]} {direct_fixture}")
            direct_offer = q.expect(
                lambda line: line.startswith(f"file offer from {p_id}:"), 60
            )
            direct_id = re.search(r" id ([0-9a-f]{64})", direct_offer).group(1)
            q.send(f"/receive {direct_id}")
            direct_saved = q.expect(
                lambda line: line.startswith(f"file from {p_id}:") and " saved " in line, 120
            )
            direct_path = direct_saved.split(" saved ", 1)[1]
            direct_hash = macmini(f"shasum -a 256 {shlex.quote(direct_path)}").split()[0]
            if direct_hash != hashlib.sha256(payload).hexdigest():
                raise AssertionError("Direct tailnet file SHA-256 mismatch")
            print("PASS: direct consented file transfer with matching SHA-256", flush=True)

            _, a_tailnet_ip = TAILNET.tailscale_status()
            b_tailnet_ip = windows(
                "& 'D:\\Tailscale\\tailscale.exe' ip -4"
            ).splitlines()[-1].strip()
            if not b_tailnet_ip.startswith("100."):
                raise AssertionError("Windows has no usable Tailscale IPv4 address")
            for protocol, port in (("UDP", base + 19), ("TCP", base + 21)):
                windows(
                    f"New-NetFirewallRule -DisplayName '{firewall_name}' "
                    f"-Direction Inbound -Action Allow -Protocol {protocol} "
                    f"-LocalPort {port} -RemoteAddress '{a_tailnet_ip}' | Out-Null"
                )
            r_dir = Path(local_dir) / "R"
            r_dir.mkdir()
            r = TAILNET.Node("R", [str(TAILNET.LOCAL_BINARY), "--keystore",
                str(r_dir / "identity.keystore"), "--password", "e2e-only",
                "--name", "R", "--discovery-port", str(base + 18),
                "--port", str(base + 20)])
            nodes.append(r)
            r_id = identity(r, "node")
            windows(f"New-Item -ItemType Directory -Force -Path "
                    f"'{windows_dir}/W' | Out-Null")
            w_args = [f"{windows_dir}/mesh-talk-node.exe", "--keystore",
                      f"{windows_dir}/W/identity.keystore", "--password", "e2e-only",
                      "--name", "W", "--discovery-port", str(base + 19),
                      "--port", str(base + 21)]
            w_command = "& '" + w_args[0] + "' " + " ".join(
                "'" + arg.replace("'", "''") + "'" for arg in w_args[1:]
            )
            encoded = base64.b64encode(w_command.encode("utf-16le")).decode("ascii")
            w = TAILNET.Node("W-Windows", ["ssh", "-T", "-o", "BatchMode=yes",
                WINDOWS_HOST, "powershell -NoProfile -EncodedCommand " + encoded])
            nodes.append(w)
            w_id = identity(w, "node")
            environment = os.environ.copy()
            environment["CARGO_INCREMENTAL"] = "0"
            environment["MESH_TALK_E2E_REMOTE_IP"] = b_tailnet_ip
            environment["MESH_TALK_E2E_DISCOVERY_PORT"] = str(base + 19)
            subprocess.run([
                "cargo", "test", "-p", "mesh-talk", "--lib",
                "live_tailnet_inventory_automatically_discovers_remote_node",
                "--features", "mesh-talk-core/fast-test-kdf", "--", "--ignored",
            ], cwd=ROOT, env=environment, check=True)
            print("PASS: desktop automatically discovered Windows through Tailscale inventory",
                  flush=True)
            r_line = probe_until(r, w_id, f"{b_tailnet_ip}:{base + 19}")
            w_line = probe_until(w, r_id, f"{a_tailnet_ip}:{base + 18}")
            if f" {b_tailnet_ip}:" not in r_line or f" {a_tailnet_ip}:" not in w_line:
                raise AssertionError("Windows direct pair used a non-Tailscale route")
            print("PASS: direct signed discovery over current-Mac/Windows Tailscale IPs",
                  flush=True)
            token = f"windows-tailnet-{base}"
            r.send(f"/msg {w_id[:8]} {token}")
            w.expect(lambda line: line.startswith(f"from {r_id} ") and token in line, 45)
            w.send(f"/msg {r_id[:8]} reply-{token}")
            r.expect(lambda line: line.startswith(f"from {w_id} ")
                     and f"reply-{token}" in line, 45)
            print("PASS: current-Mac/Windows direct Noise DMs in both directions", flush=True)
            r.send(f"/sendfile {w_id[:8]} {fixture}")
            windows_offer = w.expect(
                lambda line: line.startswith(f"file offer from {r_id}:"), 60
            )
            windows_file_id = re.search(r" id ([0-9a-f]{64})", windows_offer).group(1)
            w.send(f"/receive {windows_file_id}")
            windows_saved = w.expect(
                lambda line: line.startswith(f"file from {r_id}:") and " saved " in line, 120
            )
            windows_path = windows_saved.split(" saved ", 1)[1]
            quoted_path = windows_path.replace("'", "''")
            windows_hash = windows(
                f"(Get-FileHash -Algorithm SHA256 -LiteralPath '{quoted_path}').Hash"
            ).splitlines()[-1].strip().lower()
            if windows_hash != hashlib.sha256(payload).hexdigest():
                raise AssertionError("Windows direct tailnet file SHA-256 mismatch")
            print("PASS: current-Mac/Windows direct file SHA-256 match", flush=True)

            if bridge_file_error is not None:
                raise AssertionError("Cross-relay file delivery failed") from bridge_file_error
    finally:
        for node in reversed(nodes):
            node.stop()
        for machine, folder, name in remote_nodes:
            stop_remote(machine, folder, name)
        if firewall_name:
            windows(f"Get-NetFirewallRule -DisplayName '{firewall_name}' "
                    "-ErrorAction SilentlyContinue | Remove-NetFirewallRule")
        if windows_dir:
            windows(f"Remove-Item -LiteralPath '{windows_dir}' -Recurse -Force "
                    "-ErrorAction SilentlyContinue")
        macmini(f"rm -rf -- {shlex.quote(f_dir)}")
        mac2015(f"rm -rf -- {shlex.quote(e_dir)}")


if __name__ == "__main__":
    run()
