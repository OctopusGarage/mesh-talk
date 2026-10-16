#!/usr/bin/env python3
"""Physical equal-relay failover, partition recovery and consented file completion.

Four disposable relays share one public manifest: P/currentMac, R/Windows,
Q/mac2015 and S/mac-mini. Only Q has a Tailscale route to S. A/currentMac and
B/mac-mini are clients. Stop preferred P, then require A→R→Q→S→B delivery and
recipient receipt. Stop Q to partition the graph, queue work, restore Q and
require convergence. This is a four-host/two-physical-site test, not three LANs.

Build mesh-talk-node and network-acceptance for all three targets with
--features fast-test-kdf first. Windows files, logs and profiles stay on D:.
Installed post offices and network services are not changed.
"""
import base64
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import queue
import shlex
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("sibling", ROOT / "scripts/e2e-routed-sibling.py")
SIBLING = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(SIBLING)
FOUR = SIBLING.FOUR


def b64(value):
    return base64.b64encode(str(value).encode()).decode()


def wait(node, command, predicate, timeout=90):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        node.send(command)
        try:
            return node.expect(predicate, 1)
        except AssertionError:
            pass
    raise AssertionError(f"{node.name}: {command} did not converge")


def setting(node, command):
    node.send(command)
    node.expect(lambda line: line == "ok " + command.split()[0])


def history_lacks(node, account, text):
    node.send(f"history {account}")
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        try:
            line = node.lines.get(timeout=1)
        except queue.Empty:
            continue
        if line.startswith("entry ") and line.rsplit(" ", 1)[-1] == b64(text):
            raise AssertionError("Partitioned recipient already received queued message")
        if line == "ok history":
            return
    raise AssertionError("Recipient history query did not finish during partition")


def run():
    arm = ROOT / "target/debug/examples/network-acceptance"
    node_binary = FOUR.TAILNET.LOCAL_BINARY
    base = int(os.environ.get("MESH_TALK_E2E_BASE_PORT", "62000"))
    local_ip = subprocess.run(["ipconfig", "getifaddr", "en0"], check=True, capture_output=True, text=True).stdout.strip()
    q_ip = FOUR.mac2015("ipconfig getifaddr en0")
    _, q_ts = FOUR.TAILNET.tailscale_status(FOUR.MAC2015)
    mini_status = json.loads(FOUR.macmini(f"TAILSCALE_BE_CLI=1 {shlex.quote(FOUR.TAILNET.REMOTE_TAILSCALE)} status --json"))
    s_ts = next(ip for ip in mini_status["TailscaleIPs"] if ip.startswith("100."))
    q_dir = FOUR.mac2015("mktemp -d /tmp/mesh-talk-recovery-e2e.XXXXXX")
    s_dir = FOUR.macmini("mktemp -d /tmp/mesh-talk-recovery-e2e.XXXXXX")
    if not all(p.startswith("/tmp/mesh-talk-recovery-e2e.") for p in (q_dir, s_dir)):
        raise AssertionError("Unexpected remote fixture directory")
    win_dir = f"D:/Temp/mesh-talk-recovery-e2e-{base}-{os.getpid()}"
    firewall = f"MeshTalk-Recovery-E2E-{base}-{os.getpid()}"
    processes = []
    remote_pids = []
    def remote(machine, folder, name, args):
        command = f"echo $$ > {shlex.quote(folder + '/' + name + '.pid')}; exec " + " ".join(map(shlex.quote, args))
        process = FOUR.TAILNET.Node(name, FOUR.remote_command(machine, command))
        processes.append(process)
        remote_pids.append((machine, folder, name))
        return process
    def stop_remote(process, machine, folder, name):
        process.stop()
        host = FOUR.mac2015 if machine == FOUR.MAC2015 else FOUR.macmini
        pid = host(f"cat {shlex.quote(folder + '/' + name + '.pid')} 2>/dev/null || true")
        if pid.isdecimal() and folder in host(f"ps -p {pid} -o command= || true"):
            host(f"kill {pid}")
    def windows(command):
        # Every disposable Windows process uses D: for its working directory,
        # profile and temporary environment; never inherit SSH's C: directory.
        return FOUR.windows(f"$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; $env:TEMP='{win_dir}'; $env:TMP='{win_dir}'; Set-Location '{win_dir}'; " + command)
    def windows_node(name, args):
        command = f"$ErrorActionPreference='Stop'; $ProgressPreference='SilentlyContinue'; $env:TEMP='{win_dir}'; $env:TMP='{win_dir}'; Set-Location '{win_dir}'; & " + " ".join("'" + str(arg).replace("'", "''") + "'" for arg in args)
        encoded = base64.b64encode(command.encode("utf-16le")).decode()
        process = FOUR.TAILNET.Node(name, ["ssh", "-T", "-o", "BatchMode=yes", FOUR.WINDOWS_HOST, "powershell -NoProfile -EncodedCommand " + encoded])
        processes.append(process)
        return process
    try:
        FOUR.windows(f"Set-Location D:/; New-Item -ItemType Directory -Force -Path '{win_dir}/R' | Out-Null")
        r_ip = windows("(Get-NetIPAddress -AddressFamily IPv4 -InterfaceAlias WLAN).IPAddress").splitlines()[-1].strip()
        subprocess.run(["scp", "-q", str(FOUR.WINDOWS), f"{FOUR.WINDOWS_HOST}:{win_dir}/mesh-talk-node.exe"], check=True)
        subprocess.run(["scp", "-q", str(FOUR.INTEL), f"{FOUR.MAC2015}:{q_dir}/mesh-talk-node"], check=True)
        subprocess.run(["scp", "-q", str(node_binary), f"{FOUR.MAC2015}:{q_dir}/node-arm"], check=True)
        subprocess.run(["scp", "-q", str(arm), f"{FOUR.MAC2015}:{q_dir}/accept-arm"], check=True)
        FOUR.mac2015(f"scp -q {shlex.quote(q_dir + '/node-arm')} mac-mini:{shlex.quote(s_dir + '/mesh-talk-node')}")
        FOUR.mac2015(f"scp -q {shlex.quote(q_dir + '/accept-arm')} mac-mini:{shlex.quote(s_dir + '/network-acceptance')}")
        for protocol, port in [("UDP", base + 1), ("TCP", base + 11)]:
            windows(f"New-NetFirewallRule -DisplayName '{firewall}' -Direction Inbound -Action Allow -Protocol {protocol} -LocalPort {port} -RemoteAddress '{local_ip}','{q_ip}' | Out-Null")
        with tempfile.TemporaryDirectory(prefix="mesh-talk-recovery-e2e-") as local:
            data = Path(local)
            for name in ["P", "A"]:
                (data / name).mkdir()
            for host, folder in [(FOUR.mac2015, q_dir), (FOUR.macmini, s_dir)]:
                host(f"mkdir -p {shlex.quote(folder + '/relay')}")
            def relay_args(binary, folder, name, offset, manifest=None):
                return [str(binary), "--keystore", str(folder) + "/identity.keystore", "--password", "e2e-only", "--name", name, "--post-office", "--bridge", "--discovery-port", str(base + offset), "--port", str(base + 10 + offset), *(["--federation-config", str(manifest)] if manifest else [])]
            def p_start(manifest=None):
                p = FOUR.TAILNET.Node("P", relay_args(node_binary, data / "P", "P", 0, manifest))
                processes.append(p)
                return p
            def r_start(manifest=None):
                return windows_node("R", relay_args(win_dir + "/mesh-talk-node.exe", win_dir + "/R", "R", 1, manifest))
            def q_start(manifest=None):
                return remote(FOUR.MAC2015, q_dir, "Q", relay_args(q_dir + "/mesh-talk-node", q_dir + "/relay", "Q", 2, manifest))
            def s_start(manifest=None):
                return remote(FOUR.MACMINI, s_dir, "S", relay_args(s_dir + "/mesh-talk-node", s_dir + "/relay", "S", 3, manifest))
            p, r, q, s = p_start(), r_start(), q_start(), s_start()
            ids = [FOUR.identity(process, "post-office") for process in [p, r, q, s]]
            p.stop()
            r.stop()
            windows(f"Get-CimInstance Win32_Process -Filter \"name='mesh-talk-node.exe'\" | Where-Object {{ $_.CommandLine -like '*{win_dir}*' }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}")
            stop_remote(q, FOUR.MAC2015, q_dir, "Q")
            stop_remote(s, FOUR.MACMINI, s_dir, "S")
            manifest = data / "relays.conf"
            manifest.write_text("mesh-talk-federation-v1\n" + "\n".join(f"{identity}@{ip}:{base + offset}" for identity, ip, offset in [(ids[0], local_ip, 0), (ids[1], r_ip, 1), (ids[2], q_ip, 2), (ids[2], q_ts, 2), (ids[3], s_ts, 3)]) + "\n")
            subprocess.run(["scp", "-q", str(manifest), f"{FOUR.WINDOWS_HOST}:{win_dir}/relays.conf"], check=True)
            subprocess.run(["scp", "-q", str(manifest), f"{FOUR.MAC2015}:{q_dir}/relays.conf"], check=True)
            FOUR.mac2015(f"scp -q {shlex.quote(q_dir + '/relays.conf')} mac-mini:{shlex.quote(s_dir + '/relays.conf')}")
            p, r, q, s = p_start(manifest), r_start(win_dir + "/relays.conf"), q_start(q_dir + "/relays.conf"), s_start(s_dir + "/relays.conf")
            for process, identity in zip([p, r, q, s], ids):
                assert FOUR.identity(process, "post-office") == identity
            # The alternate route is meaningful only if Windows cannot bypass
            # Q and dial S directly. Bound the check instead of waiting for the
            # platform's TCP timeout; S is already listening on this exact port.
            direct = windows(
                "$socket=[System.Net.Sockets.TcpClient]::new(); try { "
                f"$pending=$socket.ConnectAsync('{s_ts}', {base + 13}); "
                "try { if ($pending.Wait(3000)) { 'DIRECT_ROUTE_AVAILABLE' } "
                "else { 'NO_DIRECT_ROUTE timeout' } } "
                "catch { 'NO_DIRECT_ROUTE rejected' } "
                "} finally { $socket.Dispose() }"
            )
            if "DIRECT_ROUTE_AVAILABLE" in direct:
                raise AssertionError("Topology unsuitable: Windows R can dial S directly, so R→Q→S is not isolated")
            assert "NO_DIRECT_ROUTE " in direct, f"Unexpected R→S TCP check: {direct}"
            print(f"PASS: bounded Windows R→S TCP precheck confirms no direct path ({direct})", flush=True)
            a = FOUR.TAILNET.Node("A", [str(arm), "--data", str(data / "A"), "--name", "A", "--discovery-port", str(base + 20)])
            processes.append(a)
            aid, aa, _, _ = SIBLING.ready(a)
            b = remote(FOUR.MACMINI, s_dir, "B", [s_dir + "/network-acceptance", "--data", s_dir + "/B", "--name", "B", "--discovery-port", str(base + 21)])
            bid, ba, _, _ = SIBLING.ready(b)
            def configure_a():
                for command in ["relay on", "bridge on", "prefer " + ids[0]]:
                    setting(a, command)
                SIBLING.discover(a, ids[0], f"{local_ip}:{base}") if p.process.poll() is None else None
                SIBLING.discover(a, ids[1], f"{r_ip}:{base + 1}")
            configure_a()
            setting(b, "relay on")
            setting(b, "bridge on")
            SIBLING.discover(b, ids[3], f"127.0.0.1:{base + 3}")
            wait(a, "peers", lambda line: line.startswith(f"peer {bid} "))
            wait(b, "peers", lambda line: line.startswith(f"peer {aid} "))
            p.stop()
            first = f"alternate-three-relay-delivery-{base}"
            a.send(f"enqueue {ba} {b64(first)}")
            message = a.expect(lambda line: line.startswith("queued ")).split()[1]
            SIBLING.history_has(b, aa, first)
            wait(a, f"delivery-status {message}", lambda line: line == "delivery-status Some(Delivered)")
            print("PASS: stopped preferred relay; encrypted DM and recipient receipt crossed R→Q→S", flush=True)
            payload = os.urandom(130_000)
            fixture = data / "source.bin"
            fixture.write_bytes(payload)
            a.send(f"enqueue-file {ba} {b64(fixture)}")
            _, card, file = a.expect(lambda line: line.startswith("queued-file ")).split()
            b.expect(lambda line: line.startswith(f"file-offer {file} "))
            a.send("restart")
            assert SIBLING.ready(a)[:2] == (aid, aa)
            configure_a()
            setting(b, "approve " + file)
            wait(b, "file-ready " + file, lambda line: line == "file-ready true", 180)
            destination = s_dir + "/saved.bin"
            setting(b, f"save-file {file} {b64(destination)}")
            actual = FOUR.macmini(f"shasum -a 256 {shlex.quote(destination)}").split()[0]
            assert actual == hashlib.sha256(payload).hexdigest(), "failover file SHA-256 mismatch"
            wait(a, "file-sent " + card, lambda line: line == "file-sent true", 180)
            print("PASS: failover file consent, sender restart, full SHA-256 and recipient completion", flush=True)
            stop_remote(q, FOUR.MAC2015, q_dir, "Q")
            second = f"queued-during-federation-partition-{base}"
            a.send(f"enqueue {ba} {b64(second)}")
            queued = a.expect(lambda line: line.startswith("queued ")).split()[1]
            wait(a, f"delivery-status {queued}", lambda line: line == "delivery-status Some(Awaiting)")
            # A first Awaiting state alone proves nothing: even a healthy graph
            # queues asynchronously. Hold the actual bridge down, repeatedly
            # query the recipient, and prove the new event remains absent.
            partition_until = time.monotonic() + 3
            while time.monotonic() < partition_until:
                history_lacks(b, aa, second)
                wait(a, f"delivery-status {queued}", lambda line: line == "delivery-status Some(Awaiting)", 5)
                time.sleep(0.25)
            print("PASS: bridge stayed offline ≥3s; recipient history lacked queued event and sender remained Awaiting", flush=True)
            q = q_start(q_dir + "/relays.conf")
            assert FOUR.identity(q, "post-office") == ids[2]
            SIBLING.history_has(b, aa, second, 120)
            wait(a, f"delivery-status {queued}", lambda line: line == "delivery-status Some(Delivered)", 120)
            print("PASS: queued ciphertext converged with recipient confirmation after bridge restart", flush=True)
    finally:
        for process in reversed(processes):
            process.stop()
        for machine, folder, name in remote_pids:
            host = FOUR.mac2015 if machine == FOUR.MAC2015 else FOUR.macmini
            pid = host(f"cat {shlex.quote(folder + '/' + name + '.pid')} 2>/dev/null || true")
            if pid.isdecimal() and folder in host(f"ps -p {pid} -o command= || true"):
                host(f"kill {pid}")
        windows(f"Get-CimInstance Win32_Process -Filter \"name='mesh-talk-node.exe'\" | Where-Object {{ $_.CommandLine -like '*{win_dir}*' }} | ForEach-Object {{ Stop-Process -Id $_.ProcessId -Force }}; Get-NetFirewallRule -DisplayName '{firewall}' -ErrorAction SilentlyContinue | Remove-NetFirewallRule; Set-Location D:/; Remove-Item -LiteralPath '{win_dir}' -Recurse -Force -ErrorAction SilentlyContinue; if (Test-Path '{win_dir}') {{ throw 'Windows fixture cleanup failed' }}; if (Get-NetFirewallRule -DisplayName '{firewall}' -ErrorAction SilentlyContinue) {{ throw 'Firewall cleanup failed' }}")
        FOUR.macmini(f"rm -rf -- {shlex.quote(s_dir)}; test ! -e {shlex.quote(s_dir)}")
        FOUR.mac2015(f"rm -rf -- {shlex.quote(q_dir)}; test ! -e {shlex.quote(q_dir)}")


if __name__ == "__main__":
    run()
