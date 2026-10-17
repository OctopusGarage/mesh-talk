# EasyTier two-Mac acceptance — 2026-10-17

## Observed result

The current Apple Silicon Mac and Intel `mac2015` successfully exchanged Mesh
Talk signed discovery queries, Noise-encrypted DMs, encrypted group messages,
and consented files through EasyTier virtual IPs. Both file directions carried
130,000 random bytes and passed SHA-256 comparison. The two-Mac harness exited 0.

This follows the [source feasibility research](../easytier-compatibility-research.md).
It verifies the current working tree based on
`b59c2952cc293b64f6c3ad57c5b048caf52c5c95`, including pre-existing local changes;
it is not a published-release certification.

| Host                       | OS / architecture     | EasyTier       | Virtual IP     | Observed interface |
| -------------------------- | --------------------- | -------------- | -------------- | ------------------ |
| Current Mac (`kingson-pc`) | macOS 15.4.1 / arm64  | 2.6.4-8428a89d | 10.253.77.1/24 | utun9              |
| `mac2015`                  | macOS 12.7.6 / x86_64 | 2.6.4-8428a89d | 10.253.77.2/24 | utun4              |

Both route inventories reported the other node as a direct EasyTier peer.
Three pings in each direction succeeded with zero packet loss. Interface
numbers are observations from this run and can change after restart.

## Installation and running services

Downloaded the official v2.6.4 macOS archives and checked their SHA-256 against
GitHub release asset digests before installation:

| Archive                             | SHA-256                                                            |
| ----------------------------------- | ------------------------------------------------------------------ |
| `easytier-macos-aarch64-v2.6.4.zip` | `4be1882d1aa36d31c1d6ba0596f2cf8a097e371f8da124212324b2e0f8df7e4b` |
| `easytier-macos-x86_64-v2.6.4.zip`  | `89fc28a6e6995259d76ce3f11775220e8a21c760e94df91a6a9db30a69b6982e` |

Source: [official release](https://github.com/EasyTier/EasyTier/releases/tag/v2.6.4).
The current Mac's previous 2.4.3 binaries are backed up in
`~/.local/state/easytier/backups/2.4.3-445e68dd/`.

Both hosts now run the root LaunchDaemon
`system/com.octopusgarage.easytier`, configured to start at boot and restart on
exit. Files on each host:

- Plist: `/Library/LaunchDaemons/com.octopusgarage.easytier.plist`.
- Executables: `/Library/Application Support/MeshTalk-EasyTier/bin/`.
- Configuration: `/Library/Application Support/MeshTalk-EasyTier/network.toml`.
- Log: `/var/log/mesh-talk-easytier.log`.
- Management RPC: `127.0.0.1:15888`.

The network uses a generated shared secret stored in root-owned mode-600
configuration files. Staging copies of the secret were deleted after activation.
No secret is included in this report, command arguments, or the repository.
User-facing binaries are at `~/.cargo/bin/` locally and `~/.local/bin/` on
`mac2015`.

The local bootstrap connects to `tcp://192.168.3.72:11010` on `mac2015`; both
hosts listen on outer TCP/UDP 11010. This is a LAN underlay carrying an EasyTier
overlay. It does not establish internet NAT-traversal or public-relay behavior.
The bootstrap uses the observed LAN address; if that address changes, update the
local root-owned configuration. Existing Tailscale and Mesh Talk post-office
services were left running.

## Executed two-host test

```sh
cargo build --locked -p mesh-talk-core --bin mesh-talk-node --features fast-test-kdf
RUSTC="$(rustup which --toolchain stable rustc)" \
  "$(rustup which --toolchain stable cargo)" build --locked \
  -p mesh-talk-core --bin mesh-talk-node --features fast-test-kdf \
  --target x86_64-apple-darwin
python3 scripts/e2e-easytier-macos.py mac2015 10.253.77.1 10.253.77.2
```

The Intel build required the rustup Cargo/rustc pair: the default Homebrew rustc
could not find the cross target installed under rustup. The corrected build
completed successfully.

The [harness](../../scripts/e2e-easytier-macos.py) first checks both EasyTier
inventories and OS TUN routes. It creates disposable node identities, copies
the Intel binary to a temporary remote directory, and starts separate real
processes with different discovery ports. This run used UDP 58136 locally and
58137 remotely. The asserted peer routes were `10.253.77.2:52837` and
`10.253.77.1:57399`. LAN multicast and scans target each node's own discovery
port and therefore cannot discover the other test process.

Observed assertions:

1. Signed routed discovery on EasyTier virtual IPs.
2. Noise-encrypted DMs in both directions.
3. Encrypted group messages in both directions.
4. Explicitly consented 130,000-byte local-to-remote file and matching SHA-256.
5. Explicitly consented 130,000-byte remote-to-local file and matching SHA-256.

SSH controls the remote process and retrieves the remote file hash; the payload
is sent by Mesh Talk over its discovered virtual-IP routes. The remote SSH
command does not forward application ports. Temporary processes, identities,
and files were removed by the harness. Installed EasyTier services stay running.
Raw console evidence is at `/tmp/mesh-talk-easytier-e2e.log` on the current Mac;
this temporary log is not a durable repository artifact.

## Additional backend gate

`make e2e` also completed with exit 0: **13 tests passed, zero failed**.
This includes four bridge scenarios, two channel/file scenarios, persistent
history, offline post-office delivery, four CLI lifecycle/discovery/DM scenarios,
and the 110 MB compact-directory-manifest check. These are the repository's
backend acceptance rigs; their success is separate from the two-host EasyTier
evidence above and does not imply every rig used EasyTier.

Full console output is at `/tmp/mesh-talk-easytier-backend-e2e.log` on the
current Mac. Python syntax compilation, Markdown formatting, repository
whitespace checks, and relative links in the two EasyTier documents also passed.

## Automatic discovery follow-up

The later implementation on working-tree base
`88feae6e49c56ced3c5ef43adc5319587f4de69a` adds a desktop EasyTier adapter alongside
Tailscale. Both are wired through the production `desktop_candidates` factory.
They refresh automatically and on rescan, with independent deadlines and fair
deduplication under the core candidate limit. The
[current setup guide](../routed-network-discovery.md) owns configuration details.

The two-Mac harness was extended and rerun successfully: **six checks passed**,
including the five original discovery/DM/group/file assertions. Before either
CLI sends a manual probe, an ignored host integration test starts a fresh
`NodeRuntime` with a disposable profile and the production inventory factory.
Its test wrapper retains only the expected EasyTier address actually returned
by that factory, so a simultaneous Tailscale route cannot satisfy the transfer.
The wrapper does not synthesize candidates. The test requires the exact remote
identity and `RoutedQuery` provenance on its EasyTier route; separate discovery
ports prevent incidental LAN discovery. The runtime then sends an encrypted DM
and the harness observes its unique payload at the remote CLI.

This proves inventory-driven discovery and messaging through the EasyTier
virtual IP without a manual probe. It does not require disabling Tailscale or
changing firewall/routes to isolate the test. Full raw inventory is never logged
or retained: verbose EasyTier output can include private configuration.

Executed validation for this implementation:

```sh
cargo test -p mesh-talk --lib discovery::
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=runtime_automatically_queries
make test-focused TEST_PACKAGE=mesh-talk-core TEST_FILTER=physical_scope
cargo clippy -p mesh-talk --all-targets -- -D warnings
cargo build -p mesh-talk
python3 scripts/e2e-easytier-macos.py mac2015 10.253.77.1 10.253.77.2
cd frontend
npm test -- src/lib/localeParity.test.ts src/lib/settingsTranslations.test.ts
npm run build
```

The host selection passed 16 tests, including existing Tailscale regressions;
the two physical acceptance tests are ignored by default and the EasyTier one
was explicitly run by the harness. The core selections passed two candidate
tests and seven physical-scope tests. The two frontend translation files passed
13 tests. All-target Clippy, the desktop debug build, frontend production build,
Rust/Markdown/locale formatting and whitespace checks passed. The frontend build
retains its existing advisory about a bundle chunk exceeding 700 kB. The desktop
binary was built but the installed application was not replaced or launched.
The EasyTier fixture is a sanitized subset of actual 2.6.4 CLI route
output; the old 2.4.3 CLI did not interoperate with the installed 2.6.4 RPC, so no
2.4.3 compatibility claim is made.

Follow-up console evidence: `/tmp/mesh-talk-easytier-auto-e2e.log` on the current
Mac. The earlier `make e2e` result above predates the automatic adapter change;
the focused checks and live harness were used for this implementation.

## Evidence limits

The original test exercises manual routed discovery and real backend processes;
the follow-up also verifies automatic desktop inventory discovery. Neither
verifies multicast automatic discovery, native GUI behavior, WebRTC media,
Windows/Linux, actual cross-/24 or multihop routing,
public relay fallback, offline mailbox delivery, or survival across an actual
machine reboot. LaunchDaemon configuration enables boot startup, but no reboot
was performed. The follow-up changes host inventory collection and bootstrap;
the core discovery/transport protocol is unchanged. Multihop and IPv6 candidate
parsing have unit coverage, not live EasyTier topology evidence.

To inspect either running service, use `launchctl print
system/com.octopusgarage.easytier` and its local `easytier-cli -p
127.0.0.1:15888 -o json route`. Administrator operations use
`sudo launchctl kickstart -k system/com.octopusgarage.easytier` to restart or
`sudo launchctl bootout system/com.octopusgarage.easytier` to stop. Bootout alone
does not remove the installed boot plist.
