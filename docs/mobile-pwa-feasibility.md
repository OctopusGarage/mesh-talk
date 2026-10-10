# Mobile PWA feasibility and open issues

Status: **prototype; do not present the pure-LAN PWA flow as a working mobile release**.
Reviewed 2026-10-10 and updated 2026-10-11 after discussion of changing desktop IPs.
This is the decision record to read before resuming mobile work. It records observed behavior,
code-derived failure modes, unverified claims, and the next architecture choice. It is not a
claim that every issue has been reproduced on a physical phone.

## Intended path and its actual dependencies

```text
phone browser / installed PWA
  ├─ downloads its shell from the app host
  ├─ reaches a signaling relay over WebSocket to exchange SDP/ICE
  └─ opens WebRTC DataChannel + Noise to a desktop gateway hub
       └─ hub synchronizes encrypted event-log data with other LAN nodes over Noise/TCP
```

The signaling relay forwards connection setup; the **desktop gateway hub is in the data path**
between a phone and other nodes. It handles encrypted events and needs to be online and reachable.
The phone does not independently join the existing UDP multicast/TCP LAN mesh. The current
WebRTC setup gathers host candidates only, with no STUN/TURN fallback
(`crates/mesh-talk-core/src/gateway/mod.rs`, `frontend/src/lib/webrtcClient.ts`). Thus common
signaling alone does not guarantee a data path across isolated Wi-Fi clients, subnets, or the
internet.

## Release-blocking problems

| Area | What the branch does / why it can fail | Required resolution |
| --- | --- | --- |
| Changing desktop IP | `src-tauri/src/lan_host.rs::join_url` puts the current LAN IP and relay port in the QR URL. `frontend/src/lib/browserBackend.ts` saves the relay URL in localStorage and retries it. DHCP, switching Wi-Fi, or moving between Ethernet and Wi-Fi can make both URLs stale. | Use a discovery or rendezvous mechanism keyed by stable **node identity**, resolve its current endpoint on every reconnect, and test an IP change without rescanning. |
| Incomplete IP fallback | The desktop also advertises a `.local` mDNS name, but the phone learns that relay URL only after successful mesh gossip. A first connection cannot bootstrap from learned relays. Browser JavaScript cannot browse arbitrary LAN mDNS services; Android `.local` hostname resolution and multicast availability vary. Even a previously learned URL is only a fallback, not a guarantee. | Do not describe gossip or `.local` as zero-configuration recovery. Test on target Android devices/networks, or use native Android service discovery or a stable rendezvous service. |
| Origin-coupled identity | Opening `http://new-IP:port` gives the browser a different origin, with a different IndexedDB keystore and event log. `pwaNode.ts::loadNode` creates a **random new identity** if the keystore is absent. Entering the same password does not recover the old identity. | Keep one stable app origin and provide explicit encrypted backup/device linking before relying on browser storage. |
| HTTP is not an installable PWA origin | `frontend/DEPLOY.md` recommends `http://LAN-IP` and claims service-worker offline use and Add to Home Screen. A LAN IP over HTTP is not a secure context. On 2026-10-10, Chromium exposed service workers and camera APIs at `http://localhost`, but neither at `http://192.168.3.8`. It may still load as a web page, but it does not meet the intended secure PWA behavior. | Use a trusted HTTPS origin for the app and compatible `wss://` signaling. Verify installation, service worker, storage, and QR scanning on real phones. |
| Certificate confusion | WebRTC data channels use DTLS certificates/fingerprints created for the peer connection; they do not require a CA-issued website certificate. **Serving a PWA page** and its WebSocket signaling have separate HTTPS/WSS requirements. `ws://` from an HTTPS page is blocked as mixed content in the standard deployment. | Document and implement the browser-origin certificate and WSS plan. Do not ship an HTTP LAN page as a certificate-free PWA workaround. |
| Gateway start order | `set_relay_running` starts the native gateway hub only if `NodeRuntime` is already present. `spawn_node_runtime` later installs the runtime without starting a previously enabled hub. | Make gateway startup converge when both relay and signed-in node are ready; add a relay-before-login test and accurate health state. |
| Phone-first room | `browserBackend.ts::runMeshSync` treats the first browser participant as a hub/answerer; `gateway::run_mesh_hub` is always an answerer. If a phone enters first and the desktop joins later, both can wait for an offer. | Give the desktop a fixed authenticated gateway role or define deterministic role negotiation independent of join order; test both orders. |
| QR can point to a dead relay | `start_app_host` falls back to `PREFERRED_RELAY_PORT` in the URL when `relay_port` is absent. | Generate join links only after the relay and gateway are healthy and include the actual listening endpoint. |
| Misleading success | `browserBackend.ts::send_to_account` returns success when the recipient is unknown. `mesh-talk-wasm/src/lib.rs::sync_all_over_dc` discards individual `request_round` errors. | Surface send, per-conversation sync, and delivery states; do not show success for missing recipients or partial sync. |
| First-contact and session security | Signaling room join has no admission check (`mesh-talk-signal/src/lib.rs`). The browser initiates Noise with `expected_peer = None`; the QR carries a room/URL, not a trusted hub identity. `pwaNode.ts::saveSession` stores the password as plain JSON bytes in IndexedDB. | Authenticate pairing, bind the expected hub identity to it, pin that identity in the Noise handshake, and remove plaintext password persistence. |
| Foreground-only behavior | The sync loop runs in the page. The service worker caches the app shell; it does not maintain the WebRTC/Noise session or deliver messages while the page is suspended. | Specify foreground-only semantics, or choose a native mobile transport/background design if timely receipt is required. |

The branch also treats the PWA node as its own identity/account. It does not yet prove that a
phone can join the desktop user's existing multi-device account with the expected history and
device-link semantics. Decide this product behavior explicitly before calling it a mobile client.

## Evidence and gaps as of this review

- An isolated build of `origin/feature/mobile-pwa` passed frontend build, TypeScript checking,
  lint (two nonfatal React refresh warnings), and the IndexedDB persistence test on 2026-10-10.
- `pwa-gateway-sync.spec.ts` failed because the WebRTC data channel did not connect. A separate,
  minimal two-peer WebRTC test failed in the same Chromium environment and advertised a
  `198.18.0.1` virtual ICE candidate. This **does not prove a branch defect**; repeat on two
  physical devices on a normal LAN.
- `pwa-lan-bridge.spec.ts` is normally skipped unless release binaries and the E2E flag/CI are
  present. Its assertion is that the phone sees a second desktop in the roster; it does not
  assert phone → second desktop DM → reply. No physical Android end-to-end run was completed.
- The HTTP secure-context comparison above is a direct Chromium observation and agrees with
  current browser documentation.

## Architecture decision for the next discussion

**For a zero-setup, LAN-first Android experience, prefer a native Android APK using the shared
Rust core; Dioxus is one possible UI layer.** Use Android Network Service Discovery (DNS-SD/mDNS)
or the project's existing discovery through an Android-specific bridge to find the desktop's
current address, then use authenticated Mesh-Talk transport. Validate Android networking,
multicast permissions, Rust/Tokio integration, lifecycle, and packaging on devices; this is a
proposed direction, not an already proven implementation. The minimum proof is: install once,
discover desktop, exchange a DM with another desktop, change the gateway desktop's IP, then
rediscover and exchange another DM **without rescanning or re-pairing**.

If a PWA is still required, choose and prove one of these **stable bootstrap** designs first:

1. A stable HTTPS app origin and WSS rendezvous service. The desktop registers its current
   endpoint under a stable authenticated identity and re-registers after network changes; the
   phone reconnects and negotiates fresh ICE. Add TURN if isolated networks or remote access are
   in scope. This introduces service and internet dependencies.
2. A stable LAN DNS name with a trusted HTTPS certificate and WSS proxy, with a documented router
   or local-DNS setup and renewal procedure. A DNS-01 certificate can serve a private LAN name
   under a controlled public domain without exposing the LAN web port. This is workable for
   managed networks, not zero-setup on arbitrary Wi-Fi.

Do not use a DHCP reservation, current IP QR code, gossip-only fallback, or `.local` alone as a
general solution to dynamic addresses. Whatever approach is chosen must also show reconnect
state to the user and distinguish **queued**, **synced to gateway**, and **delivered to recipient**.

## Acceptance gates before a mobile release claim

1. Two real desktops plus Android phone: phone ↔ non-gateway desktop DM, both directions; verify
   message contents and delivery state, not only roster presence.
2. Repeat after gateway IP changes, relay restart, desktop login after relay startup, phone-first
   join, app restart, and stale endpoint cache; no rescan or silent identity reset.
3. Test ordinary Wi-Fi, guest/client-isolated Wi-Fi, different subnets, and screen-off/resume;
   state clearly which topologies and background behavior are supported.
4. Verify trusted origin, installability, service worker, QR camera, WSS, ICE, Noise peer pinning,
   unauthorized-room rejection, and storage recovery on target Android browsers if PWA remains.
5. Add automated tests for each code-derived failure above, then run the project health gate and
   a real-device acceptance run. Port only the proven seams into the current development line;
   this branch diverges significantly from the main product code.

## External references

- [MDN: PWA installability and HTTPS](https://developer.mozilla.org/en-US/docs/Web/Progressive_web_apps/Guides/Making_PWAs_installable)
- [MDN: service workers require secure contexts](https://developer.mozilla.org/en-US/docs/Web/API/Service_Worker_API)
- [MDN: WebRTC certificate fingerprints](https://developer.mozilla.org/en-US/docs/Web/API/RTCCertificate/getFingerprints)
- [MDN: WebSocket client security](https://developer.mozilla.org/en-US/docs/Web/API/WebSockets_API/Writing_WebSocket_client_applications)
- [Android: Network Service Discovery](https://developer.android.com/reference/android/net/nsd/NsdManager)
- [Dioxus: mobile/Android support](https://dioxuslabs.com/learn/0.7/guides/platforms/mobile/)
- [Let's Encrypt: DNS-01 challenge](https://letsencrypt.org/docs/challenge-types/)
