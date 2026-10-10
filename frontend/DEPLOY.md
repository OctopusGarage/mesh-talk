# Experimental Mesh-Talk PWA deployment

> **Prototype only.** Read the [mobile PWA feasibility and open-issues record](../docs/mobile-pwa-feasibility.md)
> before deployment or further implementation. The earlier `http://LAN-IP` QR recipe is not a
> reliable installable PWA design and does not recover automatically when the desktop IP changes.

The browser build contains a WebAssembly node and uses WebRTC DataChannels plus Noise to sync
encrypted events with a desktop gateway. The signaling relay exchanges connection setup; the
**desktop gateway is in the encrypted data path** to other LAN peers. The app currently syncs
while its page is running; its service worker only caches the app shell.

## Requirements for a PWA deployment

1. Serve the app from one stable, trusted `https://` origin. A phone opening
   `http://<desktop-LAN-IP>` does not gain a secure context simply because the desktop is on the
   same LAN. Without a secure context, the intended service worker and QR camera flow fail, and
   browser-supported PWA installation is not established.
2. Provide a reachable `wss://` signaling endpoint for that HTTPS page. WebRTC creates its own
   DTLS certificate/fingerprint for the data channel; the **website** and WSS endpoint still need
   a trusted TLS deployment. A DNS-01 certificate for a stable domain can cover a private LAN
   service, but requires domain/DNS control and a renewal plan.
3. Resolve the desktop's current network endpoint after IP changes. A QR containing an IP address
   is only a one-time hint. Previously gossiped relay URLs and a `.local` fallback are insufficient
   as the only recovery mechanism, especially on Android. The current branch has no proven
   no-rescan recovery path.
4. Keep the desktop gateway running after login and relay startup, verify pairing and the expected
   hub identity, and test both phone-first and desktop-first joins. These are open implementation
   issues, not deployment settings.
5. Test on real devices. Host-only ICE candidates require a mutually reachable network; isolated
   Wi-Fi, different subnets, or remote connections need additional transport support such as TURN.

For local development, the branch's `mesh-talk-signal --serve-dir` command can still serve the
static shell and signaling over HTTP/WS. That setup is useful for inspecting the prototype, but
`http://LAN-IP` is a different browser origin after every address change and is **not** the
recommended mobile installation path.

Do not promise that entering the same password recovers a lost browser identity: a missing
IndexedDB keystore currently causes a fresh random identity. Provide encrypted backup or account
linking before relying on browser storage for a mobile release.

References: [PWA installability](https://developer.mozilla.org/en-US/docs/Web/Progressive_web_apps/Guides/Making_PWAs_installable),
[service-worker secure contexts](https://developer.mozilla.org/en-US/docs/Web/API/Service_Worker_API),
[WebSocket security](https://developer.mozilla.org/en-US/docs/Web/API/WebSockets_API/Writing_WebSocket_client_applications),
[DNS-01 certificates](https://letsencrypt.org/docs/challenge-types/).
