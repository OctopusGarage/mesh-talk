# Routed network discovery

Mesh Talk can discover another running Mesh Talk device across an existing routed network, including a user-managed Tailscale tailnet. Mesh Talk does not install, start, log into, configure, or authorize Tailscale. The same core protocol works with any reachable IP address supplied by a host adapter or the user.

## Boundaries

An address from a VPN inventory is only a **candidate**. It does not prove that Mesh Talk is running there, identify a device, or grant access. A fresh, signed Mesh Talk discovery response yields a **verified peer identity** and a possible TCP endpoint. Actual communication uses a Noise connection pinned to that identity; existing privacy, account, group, and post-office admission rules still apply. A successful query send does not mean that the remote device was found.

The desktop adapter reads `tailscale status --json` only when a locally available client already responds. It checks the reported running state and visible peer IPs, omits offline peers and this device, and limits the snapshot to 256 addresses. The adapter is optional and best effort: CLI absence, client permissions, operating system packaging, or JSON changes simply yield no candidates. Core knows only the `CandidateSource` interface. Manual IP or hostname entry in the connection dialog provides a fallback. Tailscale's inventory does not advertise Mesh Talk capability, so the desktop sends rate-limited queries to these candidate IPs; it never sweeps the Tailscale address range.

## Protocol and routes

The routed discovery query (`MTDQ`, version 1) is a fixed 64-byte UDP packet with a random 16-byte nonce and no Mesh Talk identity. A publicly visible responder rate-limits requests and signs a response (`MTDR`) that binds the nonce to its existing signed `Announce`. Responses are accepted only from an outstanding candidate IP, within five seconds, once, and after both signatures verify. Invisible mode does not answer an unknown routed query or initiate inventory probing. UDP discovery and the Mesh Talk TCP listening port must both be reachable through the user's network and firewall policy.

The roster keeps up to eight endpoints for each device identity. Paths carry their origin, last observation, and last authenticated use; a path does not replace the peer's key. The encrypted route cache keeps multiple IPs and reads the previous single-IP format. Dialing tries the caller's current endpoint, then recently successful and other verified paths with a bounded timeout, and pins every attempt to the expected Noise identity. A new signed listening port on the same IP supersedes stale cached ports. Tailscale's direct, peer-relay, or DERP choice stays below Mesh Talk's transport layer; no DERP-specific application route exists.

The desktop runtime refreshes the optional candidate snapshot periodically and on Rescan, sends bounded unicast queries over IPv4 and IPv6 sockets, and continues LAN multicast discovery. The headless CLI answers routed queries and retains LAN behavior. Existing encrypted TCP chat, files, synchronization, and post-office delivery use the selected path. Calls use a separate WebRTC ICE media path; successful TCP signaling alone does not establish that a Tailscale ICE candidate is available. Cross-network call media needs a separate platform matrix before it can be promised.

The CLI's `/probe <ip:discovery-port>` command sends the same identity-free query to an explicit candidate. It is useful for manual diagnosis and for the two-Mac tailnet test; it does not consult or configure Tailscale.

## Opt-in post-office bridge

A post office can additionally run a short-lived peer directory with `--post-office --bridge`. Each participating public node enables **Discover peers through a post office** in desktop Settings or starts the CLI with `--bridge-discovery`. A/B in LAN X discover E over ordinary LAN discovery; C discovers E over an existing routed path such as Tailscale. The clients register their own signed presence over Noise-authenticated TCP. E returns only self-signed proofs from other active participants. It does not forward their private LAN addresses, invent identities, configure the VPN, or bridge IP subnets.

For a stable relay endpoint, start E with `mesh-talk-node --post-office --bridge --port 47475` and its normal keystore, password source, and name flags. The user's LAN and tailnet policy must permit the discovery UDP port (default 47474) and E's TCP port from participating devices. A/B need no Tailscale installation when they can reach E over LAN X; only C and E need a mutually reachable overlay path.

The app stores an introduced peer as a *post-office route*, not as a direct TCP endpoint. An independently discovered LAN or routed endpoint remains preferred. When no direct path exists, the sender uploads recipient-encrypted events to E's existing opaque mailbox; the receiver drains that mailbox. Enabling bridge discovery also permits automatic mailbox recovery from peers currently introduced by that directory, similar to accepting messages from public LAN peers. The bridge setting is off by default, is ignored in invisible mode, and requires relay use to remain enabled. Directory membership expires after 45 seconds without renewal; disabling the setting stops local sharing and recovery immediately, while other clients can retain the last registration until it expires.

This makes C visible to opted-in A/B and A/B visible to opted-in C, with bidirectional encrypted DMs through E when they cannot connect directly. B and D remain ordinary LAN peers unless they each opt in and can reach E (or another bridge is explicitly deployed). E does not make WebRTC calls or arbitrary TCP traffic transit the post office. A reachable E is required for cross-LAN delivery; if E is offline, accepted messages remain queued locally for retry.

`cargo test -p mesh-talk-core --test bridge_cross_lan --features fast-test-kdf -- --ignored` runs a four-process A/B/C/E rig with isolated UDP discovery scopes and checks peer introductions plus DMs in both directions.
With two Macs already connected to Tailscale, `python3 scripts/e2e-tailnet-bridge.py mac2015` runs A/B/E on the current Mac and C on the remote Mac, checks the same introductions and DMs through E, then removes its temporary profiles. It does not configure Tailscale.

## Two-Mac tailnet test

With Tailscale already running on both Macs and SSH access to the Intel remote Mac, build the test binaries and run `python3 scripts/e2e-tailnet-macos.py <ssh-host>`. The script uses fresh temporary Mesh Talk identities and different discovery ports to keep LAN multicast from satisfying its routed-path assertions. It checks signed discovery at both Tailscale IPs, Noise-encrypted DMs in both directions, a consented file transfer with matching SHA-256, and the desktop Tailscale inventory adapter's automatic `RoutedQuery` route. It removes the temporary remote binary and profiles afterward. Build instructions are in the script header. The script does not exercise WebRTC media or force a Tailscale DERP path.

## Limits and release checks

- No tailnet account creation, membership management, ACL changes, address-range scanning, or trust based on Tailscale identity.
- A blocked discovery UDP port, invisible remote node, unavailable peer inventory, or incompatible old Mesh Talk node can prevent automatic first contact. Manual entry still requires a responding compatible node. A Tailscale IP being reachable does not imply its Mesh Talk application ports are reachable.
- Verify automatic first contact and Noise-pinned chat/file transfer on macOS, Windows, and Linux against direct Tailscale paths, DERP-backed paths, and a blocked UDP application port. Test client absent/logged out, changing Tailscale IPs, identity spoof/replay rejection, and invisible mode. The local loopback suite covers the protocol and route fallback; it cannot substitute for these real tailnet tests.

References: [Tailscale CLI](https://tailscale.com/docs/reference/tailscale-cli), [Tailscale status representation](https://github.com/tailscale/tailscale/blob/main/ipn/ipnstate/ipnstate.go), [Tailscale connection types](https://tailscale.com/docs/reference/connection-types).
