#!/usr/bin/env python3
"""Exercise one pinned bridge hub and two gateways on four physical machines.

Build the arm64, x86_64 macOS, and Windows GNU CLI binaries with
``--features fast-test-kdf`` first. This checkout uses the 1.99 toolchain for
cross targets (set RUSTC to that toolchain's rustc when invoking its cargo).
Run ``python3 scripts/e2e-four-machine-hub.py`` from the checkout. SSH aliases:
mac2015, windows10; mac-mini is reached through mac2015. All identities and
processes are temporary.

The hub and a competing post office run on mac2015. The gateway on this Mac
reaches the hub by LAN IP; the gateway on mac-mini uses Tailscale. Separate UDP
ports isolate the three discovery scopes despite the shared physical LAN.
"""

import base64
import importlib.util
import json
from pathlib import Path
import random
import re
import shlex
import subprocess
import tempfile


ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location(
    "four_machine", ROOT / "scripts/e2e-four-machine-tailnet.py"
)
FOUR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FOUR)
TAILNET = FOUR.TAILNET


def run():
    for binary in (TAILNET.LOCAL_BINARY, FOUR.INTEL, FOUR.WINDOWS):
        if not binary.is_file():
            raise SystemExit(f"Build the test binary first: {binary}")

    hub_lan_ip = FOUR.mac2015("ipconfig getifaddr en0")
    gateway_lan_ip = subprocess.run(
        ["ipconfig", "getifaddr", "en0"], check=True, capture_output=True, text=True
    ).stdout.strip()
    hub_status, hub_tailnet_ip = TAILNET.tailscale_status(FOUR.MAC2015)
    mini_status = json.loads(FOUR.macmini(
        f"TAILSCALE_BE_CLI=1 {shlex.quote(TAILNET.REMOTE_TAILSCALE)} status --json"
    ))
    mini_ip = next(ip for ip in mini_status["TailscaleIPs"] if ip.startswith("100."))
    if not any(mini_ip in peer.get("TailscaleIPs", []) and peer.get("Online")
               for peer in hub_status.get("Peer", {}).values()):
        raise AssertionError("mac-mini is not online in mac2015's Tailscale account")
    print(f"Routes: hub {hub_lan_ip}/{hub_tailnet_ip}, G {gateway_lan_ip}, H {mini_ip}",
          flush=True)

    hub_dir = FOUR.mac2015("mktemp -d /tmp/mesh-talk-hub-e2e.XXXXXX")
    mini_dir = FOUR.macmini("mktemp -d /tmp/mesh-talk-hub-e2e.XXXXXX")
    if not all(path.startswith("/tmp/mesh-talk-hub-e2e.") for path in (hub_dir, mini_dir)):
        raise AssertionError("Unexpected remote temporary directory")
    nodes = []
    remote_nodes = []
    windows_dir = None
    firewall_name = None

    def remote(machine, folder, name, udp, tcp, extra=()):
        directory = f"{folder}/{name}"
        args = [f"{folder}/mesh-talk-node", "--keystore",
                f"{directory}/identity.keystore", "--password", "e2e-only",
                "--name", name, "--discovery-port", str(udp), "--port", str(tcp), *extra]
        command = (f"mkdir -p {shlex.quote(directory)}; "
                   f"echo $$ > {shlex.quote(folder + '/' + name + '.pid')}; "
                   "exec " + " ".join(map(shlex.quote, args)))
        node = TAILNET.Node(name, FOUR.remote_command(machine, command))
        nodes.append(node)
        remote_nodes.append((machine, folder, name))
        return node

    def stop_remote(machine, folder, name):
        run_host = FOUR.mac2015 if machine == FOUR.MAC2015 else FOUR.macmini
        pid = run_host(f"cat {shlex.quote(folder + '/' + name + '.pid')} 2>/dev/null || true")
        if pid.isdecimal():
            command = run_host(f"ps -p {pid} -o command= || true")
            if f"{folder}/mesh-talk-node" in command and \
                    f"{folder}/{name}/identity.keystore" in command:
                run_host(f"kill {pid}")

    try:
        subprocess.run(["scp", "-q", str(FOUR.INTEL),
                        f"{FOUR.MAC2015}:{hub_dir}/mesh-talk-node"], check=True)
        subprocess.run(["scp", "-q", str(TAILNET.LOCAL_BINARY),
                        f"{FOUR.MAC2015}:{hub_dir}/mesh-talk-node-arm64"], check=True)
        FOUR.mac2015(f"scp -q {shlex.quote(hub_dir + '/mesh-talk-node-arm64')} "
                     f"{FOUR.MACMINI}:{shlex.quote(mini_dir + '/mesh-talk-node')}")
        FOUR.mac2015(f"chmod 700 {shlex.quote(hub_dir + '/mesh-talk-node')}")
        FOUR.macmini(f"chmod 700 {shlex.quote(mini_dir + '/mesh-talk-node')}")

        with tempfile.TemporaryDirectory(prefix="mesh-talk-hub-e2e-") as local_dir:
            base = random.randint(49000, 55000)
            relay_one = remote(FOUR.MAC2015, hub_dir, "relay-one", base,
                               base + 10, ("--post-office", "--bridge"))
            one_id = FOUR.identity(relay_one, "post-office")
            relay_two = remote(FOUR.MAC2015, hub_dir, "relay-two", base + 1,
                               base + 11, ("--post-office", "--bridge"))
            two_id = FOUR.identity(relay_two, "post-office")
            # Choose the higher fingerprint as hub so the lower local relay
            # would win the ordinary election if client pinning were broken.
            c, c_id, d, d_id, d_udp, d_tcp, d_name = (
                (relay_one, one_id, relay_two, two_id, base + 1, base + 11,
                 "relay-two")
                if one_id < two_id else
                (relay_two, two_id, relay_one, one_id, base, base + 10,
                 "relay-one")
            )
            print(f"C competing post office {c_id}; D pinned hub {d_id}", flush=True)

            g_dir = Path(local_dir) / "G"
            g_dir.mkdir()
            g = TAILNET.Node("G", [str(TAILNET.LOCAL_BINARY), "--keystore",
                str(g_dir / "identity.keystore"), "--password", "e2e-only",
                "--name", "G", "--discovery-port", str(base + 2),
                "--port", str(base + 12), "--post-office", "--bridge",
                "--bridge-peer", f"{hub_lan_ip}:{d_udp}",
                "--bridge-peer-id", d_id, "--bridge-proxy-port", "0"])
            nodes.append(g)
            g_id = FOUR.identity(g, "post-office")
            h = remote(FOUR.MACMINI, mini_dir, "H", base + 3, base + 13,
                       ("--post-office", "--bridge", "--bridge-peer",
                        f"{hub_tailnet_ip}:{d_udp}", "--bridge-peer-id", d_id,
                        "--bridge-proxy-port", "0"))
            h_id = FOUR.identity(h, "post-office")

            x = remote(FOUR.MAC2015, hub_dir, "X", base + 4, base + 14,
                       ("--bridge-discovery", "--bridge-hub-id", d_id))
            x_id = FOUR.identity(x, "node")
            z = remote(FOUR.MACMINI, mini_dir, "Z", base + 6, base + 16,
                       ("--bridge-discovery", "--bridge-hub-id", d_id))
            z_id = FOUR.identity(z, "node")

            windows_dir = FOUR.windows(
                f"$path = Join-Path $env:TEMP 'mesh-talk-hub-e2e-{base}'; "
                "New-Item -ItemType Directory -Force -Path $path | Out-Null; Write-Output $path"
            ).splitlines()[-1].strip().replace("\\", "/")
            if not re.match(r"^[A-Za-z]:/.*mesh-talk-hub-e2e-\d+$", windows_dir):
                raise AssertionError(f"Unexpected Windows temp directory: {windows_dir}")
            subprocess.run(["scp", "-q", str(FOUR.WINDOWS),
                            f"{FOUR.WINDOWS_HOST}:{windows_dir}/mesh-talk-node.exe"],
                           check=True)
            firewall_name = f"MeshTalk Hub E2E {base}"
            FOUR.windows(
                f"New-NetFirewallRule -DisplayName '{firewall_name}' "
                f"-Direction Inbound -Action Allow -Protocol UDP -LocalPort {base + 5} "
                "-RemoteAddress LocalSubnet | Out-Null"
            )
            y_args = [f"{windows_dir}/mesh-talk-node.exe", "--keystore",
                      f"{windows_dir}/identity.keystore", "--password", "e2e-only",
                      "--name", "Y", "--discovery-port", str(base + 5),
                      "--port", str(base + 15), "--bridge-discovery",
                      "--bridge-hub-id", d_id]
            y_command = "& '" + y_args[0] + "' " + " ".join(
                "'" + arg.replace("'", "''") + "'" for arg in y_args[1:]
            )
            encoded = base64.b64encode(y_command.encode("utf-16le")).decode("ascii")
            y = TAILNET.Node("Y-Windows", ["ssh", "-T", "-o", "BatchMode=yes",
                FOUR.WINDOWS_HOST, "powershell -NoProfile -EncodedCommand " + encoded])
            nodes.append(y)
            y_id = FOUR.identity(y, "node")

            FOUR.probe_until(x, c_id, f"127.0.0.1:{base if c is relay_one else base + 1}")
            FOUR.probe_until(x, d_id, f"127.0.0.1:{d_udp}")
            FOUR.probe_until(y, g_id, f"{gateway_lan_ip}:{base + 2}")
            FOUR.probe_until(z, h_id, f"127.0.0.1:{base + 3}")
            clients = (("X", x, x_id), ("Y", y, y_id), ("Z", z, z_id))
            for name, node, own in clients:
                for _, _, other in clients:
                    if other != own:
                        FOUR.peer(node, other, 120)
                hub_line = FOUR.peer(node, d_id, 120)
                if name == "Y" and f" {gateway_lan_ip}:" not in hub_line:
                    raise AssertionError(f"Y did not route D through G: {hub_line}")
                if name == "Z" and " 127.0.0.1:" not in hub_line:
                    raise AssertionError(f"Z did not route D through H: {hub_line}")
                print(f"PASS: {name} discovered all sites and pinned hub D", flush=True)

            for source_name, source, source_id in clients:
                for target_name, target, target_id in clients:
                    if source_id == target_id:
                        continue
                    token = f"hub-{source_name}-to-{target_name}-{base}"
                    source.send(f"/msg {target_id[:8]} {token}")
                    target.expect(lambda line, token=token, source_id=source_id:
                                  line.startswith(f"from {source_id} ") and token in line, 90)
                    print(f"PASS: {source_name} -> {target_name} encrypted DM", flush=True)
            print("PASS: all six directed encrypted DMs across three scopes", flush=True)

            d.stop()
            stop_remote(FOUR.MAC2015, hub_dir, d_name)
            token = f"hub-restart-Y-to-Z-{base}"
            y.send(f"/msg {z_id[:8]} {token}")
            try:
                z.expect(lambda line: line.startswith(f"from {y_id} ")
                         and token in line, 5)
            except AssertionError as error:
                if not str(error).startswith("Timed out waiting"):
                    raise
            else:
                raise AssertionError("Message bypassed the stopped pinned hub")
            d = remote(FOUR.MAC2015, hub_dir, d_name, d_udp, d_tcp,
                       ("--post-office", "--bridge"))
            if FOUR.identity(d, "post-office") != d_id:
                raise AssertionError("Hub identity changed across restart")
            z.expect(lambda line: line.startswith(f"from {y_id} ")
                     and token in line, 120)
            print("PASS: queued Y -> Z DM drained after same-identity hub restart",
                  flush=True)
    finally:
        for node in reversed(nodes):
            node.stop()
        for machine, folder, name in remote_nodes:
            stop_remote(machine, folder, name)
        if firewall_name:
            FOUR.windows(f"Get-NetFirewallRule -DisplayName '{firewall_name}' "
                         "-ErrorAction SilentlyContinue | Remove-NetFirewallRule")
        if windows_dir:
            FOUR.windows(f"Remove-Item -LiteralPath '{windows_dir}' -Recurse -Force "
                         "-ErrorAction SilentlyContinue")
        FOUR.macmini(f"rm -rf -- {shlex.quote(mini_dir)}")
        FOUR.mac2015(f"rm -rf -- {shlex.quote(hub_dir)}")


if __name__ == "__main__":
    run()
