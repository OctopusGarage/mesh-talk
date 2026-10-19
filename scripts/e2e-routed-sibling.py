#!/usr/bin/env python3
"""Real SDK pairing and ongoing linked-device catch-up over a Tailscale route.

Build the network-acceptance example for arm64 and x86_64 macOS first with
--features fast-test-kdf. Uses disposable profiles, leaves installed services
alone, and reaches mac-mini through mac2015. It proves two physical sites, not
three independent LANs. Shares SSH/process helpers with the four-machine rig.
"""
import base64
import importlib.util
import json
import os
from pathlib import Path
import shlex
import subprocess
import tempfile
import time

ROOT = Path(__file__).resolve().parents[1]
SPEC = importlib.util.spec_from_file_location("four_machine", ROOT / "scripts/e2e-four-machine-tailnet.py")
FOUR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FOUR)


def ready(node):
    line = node.expect(lambda line: line.startswith("ready "), 60)
    _, identity, account, tcp, udp = line.split()
    return identity, account, int(tcp), int(udp)


def discover(node, identity, endpoint):
    deadline = time.monotonic() + 45
    while time.monotonic() < deadline:
        node.send(f"probe {endpoint}")
        node.send("peers")
        try:
            return node.expect(lambda line: line.startswith(f"peer {identity} "), 1)
        except AssertionError:
            pass
    raise AssertionError(f"{node.name} did not discover {identity} at {endpoint}")


def history_has(node, account, text, timeout=45):
    encoded = base64.b64encode(text.encode()).decode()
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        node.send(f"history {account}")
        try:
            return node.expect(lambda line: line.startswith("entry ") and line.endswith(" " + encoded), 1)
        except AssertionError:
            pass
    raise AssertionError(f"{node.name} missing durable history {text}")


def run():
    arm = ROOT / "target/debug/examples/network-acceptance"
    intel = ROOT / "target/x86_64-apple-darwin/debug/examples/network-acceptance"
    for binary in (arm, intel):
        if not binary.is_file():
            raise SystemExit(f"Build acceptance example first: {binary}")
    _, mac_ip = FOUR.TAILNET.tailscale_status(FOUR.MAC2015)
    # The mini need not be in the current machine's tailnet; the test follows
    # the actual mac2015-to-mini route rather than assuming a transitive VPN.
    mini_status = json.loads(FOUR.macmini(f"TAILSCALE_BE_CLI=1 {shlex.quote(FOUR.TAILNET.REMOTE_TAILSCALE)} status --json"))
    mini_ip = next(ip for ip in mini_status["TailscaleIPs"] if ip.startswith("100."))
    mac_lan = FOUR.mac2015("ipconfig getifaddr en0")
    local_ip = subprocess.run(["ipconfig", "getifaddr", "en0"], check=True, capture_output=True, text=True).stdout.strip()
    remote_dir = FOUR.mac2015("mktemp -d /tmp/mesh-talk-sibling-e2e.XXXXXX")
    mini_dir = FOUR.macmini("mktemp -d /tmp/mesh-talk-sibling-e2e.XXXXXX")
    if not all(p.startswith("/tmp/mesh-talk-sibling-e2e.") for p in (remote_dir, mini_dir)):
        raise AssertionError("Unexpected remote fixture directory")
    nodes = []
    base = int(os.environ.get("MESH_TALK_E2E_BASE_PORT", "61000"))
    try:
        subprocess.run(["scp", "-q", str(intel), f"{FOUR.MAC2015}:{remote_dir}/network-acceptance"], check=True)
        subprocess.run(["scp", "-q", str(arm), f"{FOUR.MAC2015}:{remote_dir}/network-acceptance-arm64"], check=True)
        FOUR.mac2015(f"scp -q {shlex.quote(remote_dir + '/network-acceptance-arm64')} mac-mini:{shlex.quote(mini_dir + '/network-acceptance')}")
        def remote(machine, directory, name, port):
            command = " ".join(map(shlex.quote, [directory + "/network-acceptance", "--data", directory + "/" + name, "--name", name, "--discovery-port", str(port)]))
            node = FOUR.TAILNET.Node(name, FOUR.remote_command(machine, command))
            nodes.append(node)
            return node
        a = remote(FOUR.MAC2015, remote_dir, "A", base)
        aid, account, _, _ = ready(a)
        b = remote(FOUR.MACMINI, mini_dir, "B", base + 1)
        bid, _, _, _ = ready(b)
        with tempfile.TemporaryDirectory(prefix="mesh-talk-sibling-e2e-") as local:
            c = FOUR.TAILNET.Node("C", [str(arm), "--data", local, "--name", "C", "--discovery-port", str(base + 2)])
            nodes.append(c)
            cid, sender_account, _, _ = ready(c)
            discover(a, bid, f"{mini_ip}:{base + 1}")
            discover(b, aid, f"{mac_ip}:{base}")
            discover(c, aid, f"{mac_lan}:{base}")
            discover(a, cid, f"{local_ip}:{base + 2}")
            first = f"physical-sibling-before-link-{base}"
            c.send(f"account-msg {account} {base64.b64encode(first.encode()).decode()}")
            history_has(a, sender_account, first)
            a.send("link-code")
            code = a.expect(lambda line: line.startswith("link-code ")).split()[1]
            b.send(f"link {aid} {code}")
            b.expect(lambda line: line == f"linked {account}", 60)
            b.send("restart")
            assert ready(b)[:2] == (bid, account)
            a.send("restart")
            assert ready(a)[:2] == (aid, account)
            discover(a, bid, f"{mini_ip}:{base + 1}")
            discover(b, aid, f"{mac_ip}:{base}")
            discover(c, aid, f"{mac_lan}:{base}")
            discover(a, cid, f"{local_ip}:{base + 2}")
            history_has(b, sender_account, first)
            print("PASS: authenticated Tailscale pairing and persisted account adoption", flush=True)
            b.send("history-sync off")
            b.expect(lambda line: line == "ok history-sync")
            second = f"physical-sibling-after-link-{base}"
            c.send(f"account-msg {account} {base64.b64encode(second.encode()).decode()}")
            expected = history_has(a, sender_account, second)
            b.send("history-sync on")
            b.expect(lambda line: line == "ok history-sync")
            actual = history_has(b, sender_account, second)
            assert actual == expected, "sibling catch-up changed the original durable event ID"
            print("PASS: ongoing routed sibling replay after pairing preserves message ID and bytes", flush=True)
            b.send("restart")
            assert ready(b)[:2] == (bid, account)
            assert history_has(b, sender_account, second) == expected
            print("PASS: routed sibling history survives receiver restart", flush=True)
    finally:
        for node in reversed(nodes):
            node.stop()
        FOUR.macmini(f"rm -rf -- {shlex.quote(mini_dir)}")
        FOUR.mac2015(f"rm -rf -- {shlex.quote(remote_dir)}")


if __name__ == "__main__":
    run()
