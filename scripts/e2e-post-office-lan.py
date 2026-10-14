#!/usr/bin/env python3
"""Exercise an already installed LAN post office with two disposable CLI peers."""

import argparse
import queue
import re
import subprocess
import tempfile
import threading
import time
from pathlib import Path


class Peer:
    def __init__(self, binary: Path, keystore: Path, name: str, discovery_port: int, post_office=False):
        self.post_office = post_office
        self.process = subprocess.Popen(
            [str(binary), "--keystore", str(keystore), "--password", "pw", "--name", name,
             "--port", "0", "--discovery-port", str(discovery_port)]
            + (["--post-office"] if post_office else []),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
            text=True, bufsize=1,
        )
        self.lines = queue.Queue()
        self.history = []
        threading.Thread(target=self._read, daemon=True).start()

    def _read(self):
        for line in self.process.stdout:
            line = line.rstrip("\n")
            self.history.append(line)
            self.lines.put(line)

    def send(self, command):
        self.process.stdin.write(command + "\n")
        self.process.stdin.flush()

    def wait(self, pattern, timeout=60):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            try:
                line = self.lines.get(timeout=min(1, deadline - time.monotonic()))
            except queue.Empty:
                if self.process.poll() is not None:
                    break
                continue
            match = re.search(pattern, line)
            if match:
                return match
        raise AssertionError(f"expected {pattern!r}; recent output: {self.history[-15:]}")

    def discover(self, user_id, timeout=50):
        deadline = time.monotonic() + timeout
        while time.monotonic() < deadline:
            self.send("/peers")
            try:
                self.wait(r"^peer " + re.escape(user_id) + r" ", timeout=2)
                return
            except AssertionError:
                pass
        raise AssertionError(f"{user_id} was not discovered; recent output: {self.history[-15:]}")

    def stop(self):
        if self.process.poll() is None:
            if self.post_office:
                self.process.terminate()
            else:
                self.send("/quit")
            try:
                self.process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                self.process.terminate()
                self.process.wait(timeout=10)


def relay_log_size(host, remote_os="macos"):
    if remote_os == "windows":
        command = (
            'powershell -NoProfile -Command "(Get-Item -LiteralPath '
            'C:\\ProgramData\\MeshTalk\\PostOffice\\data\\relay.log).Length"'
        )
    else:
        command = 'stat -f %z "$HOME/.local/share/mesh-talk-post-office/relay.log"'
    result = subprocess.run(
        ["ssh", host, command],
        check=True, capture_output=True, text=True, timeout=20,
    )
    return int(result.stdout.strip())


def restart_relay(host, remote_os="macos"):
    if remote_os == "windows":
        pid_command = (
            'powershell -NoProfile -Command '
            '"(Get-NetTCPConnection -State Listen -LocalPort 47475 '
            '-ErrorAction SilentlyContinue | Select-Object -First 1 '
            '-ExpandProperty OwningProcess)"'
        )
        previous = subprocess.run(
            ["ssh", host, pid_command], check=True, capture_output=True,
            text=True, timeout=20,
        ).stdout.strip()
        subprocess.run(
            ["ssh", host, 'powershell -NoProfile -Command '
             '"Stop-ScheduledTask -TaskName MeshTalk-PostOffice"'],
            check=True, capture_output=True, text=True, timeout=30,
        )
        deadline = time.monotonic() + 20
        while previous and time.monotonic() < deadline:
            old = subprocess.run(
                ["ssh", host, 'powershell -NoProfile -Command '
                 f'"Get-Process -Id {previous} -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Id"'],
                capture_output=True, text=True, timeout=15,
            ).stdout.strip()
            if not old:
                break
            time.sleep(0.5)
        else:
            if previous:
                raise AssertionError(f"Windows post-office PID {previous} did not stop")
        subprocess.run(
            ["ssh", host, 'powershell -NoProfile -Command '
             '"Start-ScheduledTask -TaskName MeshTalk-PostOffice"'],
            check=True, capture_output=True, text=True, timeout=30,
        )
        deadline = time.monotonic() + 45
        while time.monotonic() < deadline:
            current = subprocess.run(
                ["ssh", host, pid_command], capture_output=True, text=True, timeout=15,
            ).stdout.strip()
            if current and current != previous:
                return
            time.sleep(1)
        raise AssertionError("Windows post-office did not resume listening with a new PID")

    subprocess.run(
        ["ssh", host,
         "launchctl kickstart -k gui/$(id -u)/com.octopusgarage.mesh-talk.post-office"],
        check=True, capture_output=True, text=True, timeout=30,
    )
    deadline = time.monotonic() + 30
    while time.monotonic() < deadline:
        result = subprocess.run(
            ["ssh", host, "lsof -nP -iTCP:47475 -sTCP:LISTEN"],
            capture_output=True, text=True, timeout=15,
        )
        if result.returncode == 0 and "mesh-talk" in result.stdout:
            return
        time.sleep(1)
    raise AssertionError("post-office did not resume listening after restart")


def await_relay_growth(host, previous, timeout=60, remote_os="macos"):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        size = relay_log_size(host, remote_os)
        if size > previous:
            return size
        time.sleep(1)
    raise AssertionError(f"remote relay log did not grow beyond {previous} bytes")


def run_file_roundtrip(args):
    peers = []
    with tempfile.TemporaryDirectory(prefix="mesh-talk-lan-file-e2e-") as root:
        try:
            root = Path(root)
            source = root / "source/payload.bin"
            source.parent.mkdir()
            payload = bytes(range(256)) * 8
            source.write_bytes(payload)
            alice_key = root / "alice/identity.keystore"
            bob_key = root / "bob/identity.keystore"
            alice = Peer(args.binary, alice_key, "LAN-File-Alice", args.discovery_port)
            bob = Peer(args.binary, bob_key, "LAN-File-Bob", args.discovery_port)
            peers.extend([alice, bob])
            alice_id = alice.wait(r"^node ([0-9a-f]+) ", 120).group(1)
            bob_id = bob.wait(r"^node ([0-9a-f]+) ", 120).group(1)
            alice.discover(bob_id)
            alice.discover(args.relay_id)
            bob.discover(alice_id)
            bob.discover(args.relay_id)

            # The offer is held while Bob is offline. Alice stops before Bob
            # returns, so the offer cannot arrive by direct peer sync.
            before = relay_log_size(args.ssh_host, args.remote_os)
            bob.stop()
            alice.send(f"/sendfile {bob_id[:8]} {source}")
            alice.wait(r"^file sent to " + re.escape(bob_id) + r"$", 90)
            offer_size = await_relay_growth(args.ssh_host, before, remote_os=args.remote_os)
            alice.stop()
            if args.restart_relay_before_drain:
                restart_relay(args.ssh_host, args.remote_os)

            bob = Peer(args.binary, bob_key, "LAN-File-Bob", args.discovery_port)
            peers.append(bob)
            assert bob.wait(r"^node ([0-9a-f]+) ", 120).group(1) == bob_id
            offer = bob.wait(r"^file offer from " + re.escape(alice_id) + r": .* id ([0-9a-f]{64})", 90)
            file_id = offer.group(1)
            bob.send(f"/receive {file_id}")
            bob.wait(r"^file approved$", 30)

            # Consent is a local recipient decision queried by the sender. Bob
            # must be online when Alice returns; the relay cannot answer it.
            alice = Peer(args.binary, alice_key, "LAN-File-Alice", args.discovery_port)
            peers.append(alice)
            assert alice.wait(r"^node ([0-9a-f]+) ", 120).group(1) == alice_id
            alice.discover(bob_id)
            alice.discover(args.relay_id)
            saved = bob.wait(r"^file from " + re.escape(alice_id) + r": payload\.bin \(2048 bytes\) saved (.+)$", 120)
            assert Path(saved.group(1)).read_bytes() == payload, "saved attachment differs from source"
            print(f"PASS: installed relay {args.relay_id} held the offline file offer; 2048-byte attachment matched after peers reconnected")
            print(f"PASS: sender was stopped before recipient received the offer; relay log grew {before} -> {offer_size} bytes")
            if args.restart_relay_before_drain:
                print("PASS: file offer survived installed relay restart before recipient drain")
        finally:
            for peer in reversed(peers):
                peer.stop()


def run_multi_relay(args):
    peers = []
    with tempfile.TemporaryDirectory(prefix="mesh-talk-lan-two-relays-") as root:
        try:
            root = Path(root)
            local_path = root / "local-relay"
            local = Peer(args.binary, local_path / "identity.keystore", "LAN-Second-Relay",
                         args.discovery_port, post_office=True)
            peers.append(local)
            local_id = local.wait(r"^post-office ([0-9a-f]+) listening ", 120).group(1)
            assert local_id != args.relay_id
            alice_key = root / "alice/identity.keystore"
            bob_key = root / "bob/identity.keystore"
            alice = Peer(args.binary, alice_key, "LAN-Two-Relay-Alice", args.discovery_port)
            bob = Peer(args.binary, bob_key, "LAN-Two-Relay-Bob", args.discovery_port)
            peers.extend([alice, bob])
            alice_id = alice.wait(r"^node ([0-9a-f]+) ", 120).group(1)
            bob_id = bob.wait(r"^node ([0-9a-f]+) ", 120).group(1)
            for peer, other in [(alice, bob_id), (bob, alice_id)]:
                peer.discover(other)
                peer.discover(local_id)
                peer.discover(args.relay_id)
            elected = min(local_id, args.relay_id)
            local_log = local_path / "relay.log"
            before = (local_log.stat().st_size if elected == local_id
                      else relay_log_size(args.ssh_host, args.remote_os))
            bob.stop()
            marker = f"two-relay-{time.time_ns()}"
            alice.send(f"/msg {bob_id[:8]} {marker}")
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline:
                size = (local_log.stat().st_size if elected == local_id
                        else relay_log_size(args.ssh_host, args.remote_os))
                if size > before:
                    break
                time.sleep(1)
            else:
                raise AssertionError(f"elected relay {elected} did not store the offline DM")
            alice.stop()
            bob = Peer(args.binary, bob_key, "LAN-Two-Relay-Bob", args.discovery_port)
            peers.append(bob)
            assert bob.wait(r"^node ([0-9a-f]+) ", 120).group(1) == bob_id
            bob.discover(local_id)
            bob.discover(args.relay_id)
            message = re.compile(r"^from " + re.escape(alice_id) + r" .*: "
                                 + re.escape(marker) + r"$")
            if not any(message.search(line) for line in bob.history):
                bob.wait(message.pattern, 90)
            print(f"PASS: both LAN relays discovered; lowest fingerprint {elected} stored the offline DM")
            print("PASS: sender stopped before recipient drained the elected relay")
        finally:
            for peer in reversed(peers):
                peer.stop()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--ssh-host", default="mac2015")
    parser.add_argument("--remote-os", choices=["macos", "windows"], default="macos")
    parser.add_argument("--relay-id", required=True, help="device ID from relay startup log")
    parser.add_argument("--binary", type=Path, default=Path("target/debug/mesh-talk-node"))
    parser.add_argument("--discovery-port", type=int, default=47474)
    parser.add_argument("--restart-relay-before-drain", action="store_true")
    parser.add_argument("--file-roundtrip", action="store_true", help="isolate and verify offline attachment offer, consent, and content")
    parser.add_argument("--multi-relay", action="store_true", help="add a disposable second LAN relay and verify elected custody")
    args = parser.parse_args()
    if args.multi_relay:
        run_multi_relay(args)
        return
    if args.file_roundtrip:
        run_file_roundtrip(args)
        return
    peers = []
    with tempfile.TemporaryDirectory(prefix="mesh-talk-lan-e2e-") as root:
        try:
            alice = Peer(args.binary, Path(root) / "alice/identity.keystore", "LAN-Alice", args.discovery_port)
            bob = Peer(args.binary, Path(root) / "bob/identity.keystore", "LAN-Bob", args.discovery_port)
            peers.extend([alice, bob])
            alice_id = alice.wait(r"^node ([0-9a-f]+) ", 120).group(1)
            bob_id = bob.wait(r"^node ([0-9a-f]+) ", 120).group(1)
            alice.discover(bob_id)
            alice.discover(args.relay_id)
            bob.discover(alice_id)
            bob.discover(args.relay_id)
            before = relay_log_size(args.ssh_host, args.remote_os)
            bob.stop()
            marker = str(int(time.time()))
            dm = "lan-held-dm-" + marker
            channel_text = "lan-held-channel-" + marker
            alice.send(f"/msg {bob_id[:8]} {dm}")
            alice.send(f"/channel-new lan-private-{marker} {bob_id[:8]}")
            group = alice.wait(r"^channel ([0-9a-f]{64}) created$", 60).group(1)
            alice.send(f"/channel-msg {group[:8]} {channel_text}")
            deadline = time.monotonic() + 60
            while time.monotonic() < deadline and relay_log_size(args.ssh_host, args.remote_os) <= before:
                time.sleep(1)
            assert relay_log_size(args.ssh_host, args.remote_os) > before, "remote relay stored no events"
            # Give the asynchronous channel send time to reach the relay before the
            # sender exits. The recipient will only start after the sender is gone.
            time.sleep(4)
            alice.stop()
            if args.restart_relay_before_drain:
                restart_relay(args.ssh_host, args.remote_os)
            bob = Peer(args.binary, Path(root) / "bob/identity.keystore", "LAN-Bob", args.discovery_port)
            peers.append(bob)
            assert bob.wait(r"^node ([0-9a-f]+) ", 120).group(1) == bob_id
            seen = set()
            deadline = time.monotonic() + 90
            while len(seen) < 2 and time.monotonic() < deadline:
                try:
                    line = bob.lines.get(timeout=1)
                except queue.Empty:
                    continue
                if line.startswith(f"from {alice_id}") and dm in line:
                    seen.add("dm")
                if line.startswith(f"channel {group} from {alice_id}:") and channel_text in line:
                    seen.add("channel")
            assert seen == {"dm", "channel"}, f"missing delivery: {seen}; recent output: {bob.history[-20:]}"
            print(f"PASS: remote relay {args.relay_id} stored and delivered offline DM and private channel event")
            print(f"PASS: sender was stopped before recipient restarted; relay log grew from {before} to {relay_log_size(args.ssh_host, args.remote_os)} bytes")
            if args.restart_relay_before_drain:
                print("PASS: installed relay restarted while holding events before recipient drain")
        finally:
            for peer in reversed(peers):
                peer.stop()


if __name__ == "__main__":
    main()
