# Mesh-Talk — Architecture & Protocol Review (principles)

> **Mobile status, 2026-10-11:** Read [the mobile PWA feasibility and open-issues record](mobile-pwa-feasibility.md)
> before using this review as a mobile implementation plan. In particular, changing desktop IPs,
> insecure LAN HTTP, startup/role failures, and incomplete end-to-end proof block a release claim.
> The feasibility record supersedes conflicting mobile conclusions below.

A from-first-principles map of how Mesh-Talk connects nodes, moves data, and secures it — with the
real diagrams, the wire protocol, and an honest list of where the design is solid vs fragile. For
the module-by-module reference see [`ARCHITECTURE.md`](./ARCHITECTURE.md); this doc is about *why it
works and where it breaks*.

> TL;DR. The crypto + event-log foundations are sound. The **topology** (phone↔relay↔hub↔LAN) is the
> fragile part: it is **LAN-scoped** (no STUN/TURN), the **hub is a single point of failure**, and
> the **PWA identity is origin-coupled**. See §7.

---

## 0. The one-paragraph model

Every node holds an append-only, signed, content-addressed **event log**, partitioned into
**conversations**. Two nodes make their logs identical by running a small **sync protocol** over an
authenticated **Noise** channel. The only difference between participants is *how they reach each
other*: desktops find each other on the **LAN** (UDP discovery → TCP), while a **phone** (a browser,
which can't do UDP) reaches the mesh through a **WebRTC data channel** brokered by a **signaling
relay** and served by a **desktop "hub."** Presence and "who's online" is just another conversation
(`[7;32]`) that gossips the same way. Nothing in the middle ever sees plaintext.

---

## 1. Components & layers

```
┌──────────────────────────────────────────────────────────────────────────────┐
│ DEVICE (desktop app  OR  phone browser PWA)                                    │
│                                                                                │
│  UI (React / Tauri webview)                                                    │
│        │  commands (invoke / browserBackend)                                   │
│  ┌─────┴───────────────────────────────────────────────────────────────────┐ │
│  │ NODE  (mesh-talk-core, or the same logic compiled to wasm)               │ │
│  │                                                                          │ │
│  │   message crypto │  Double Ratchet (DM)   ·  Sender-Key (channel)        │ │
│  │   ───────────────┼───────────────────────────────────────────────────   │ │
│  │   EVENT LOG      │  signed, content-addressed DAG, per-conversation      │ │
│  │   ───────────────┼───────────────────────────────────────────────────   │ │
│  │   SYNC           │  id-set reconciliation (request → response → followup)│ │
│  │   ───────────────┼───────────────────────────────────────────────────   │ │
│  │   TRANSPORT      │  Noise XX SecureChannel  (mutual auth, AEAD frames)   │ │
│  └───────────────┬──────────────────────────────────┬──────────────────────┘ │
│                  │ TCP (LAN)                         │ WebRTC DataChannel      │
└──────────────────┼──────────────────────────────────┼─────────────────────────┘
                   │                                   │
            UDP discovery                       WS signaling relay
            (multicast + /24 scan)              (mesh-talk-signal: matchmaking only)
```

Key idea: **Noise + the sync protocol + the event log are transport-agnostic.** The exact same
`request_round`/`serve_one` runs over a LAN TCP socket and over a phone's WebRTC channel. The two
"networks" only differ in how the byte pipe is established.

---

## 2. Network connectivity — how the pipe is made

### 2.1 Desktop ↔ Desktop (LAN)

```
PC-A                                                  PC-B
 │  ① signed UDP announce → 224.0.0.167:47474  ─────►  (multicast)
 │      MTAN ‖ ver ‖ {user_id, ed25519, x25519, name, tcp_port, post_office?, cert?, sig}
 │  ◄───────────────── ② unicast reply (first-sight) ─┤   roster += A
 │  (fallback) ③ /24 unicast scan x.y.z.1..254 every 20s if multicast is blocked
 │
 │  ④ dial peer.addr (TCP) ──────────────────────────►  accept loop
 │  ⑤ Noise XX handshake  (-> e ; <- e,ee,s,es ; -> s,se)   mutual auth
 │  ⑥ post-handshake identity sig binds ed25519 ↔ the Noise x25519 static
 │  ⑦ sync rounds (see §4)
```

- Discovery is **signed** (Ed25519, domain-separated) → a peer can't be spoofed into your roster.
- **Reply-on-first-sight** converges in <1s without a reply storm; the **/24 scan** is the
  firewall/multicast-blocked fallback (slower, ~20s).
- Roster entries expire after **30s** of silence (TTL); a heartbeat announce every 2s keeps them.

### 2.2 Desktop ↔ Phone (WebRTC via relay)

The phone is a browser: **no UDP, no listening socket.** It joins a relay room; the **first node in
the room is the hub**; later nodes are **spokes**.

```
        ┌─────────────── WS signaling relay (mesh-talk-signal) ───────────────┐
        │  room-keyed; forwards SDP/ICE verbatim; NEVER sees app data         │
        └─────▲───────────────────────────────────────────────▲──────────────┘
              │ join(room)                                      │ join(room)
       DESKTOP HUB (joined first)                          PHONE (spoke)
        on the LAN  +  on the relay                         browser only
              │                                                  │
              │   ◄──── offer (SDP, bundled ICE) ────────────────┤  spoke offers
              ├──── answer (SDP) ───────────────────────────────►│
              │   ══════ WebRTC DataChannel (P2P, host candidates) ═══════
              │   Noise XX over the data channel  → same SecureChannel as TCP
              │   sync rounds (the phone pulls [7;32], DMs, channels …)
```

- The relay is **signaling only** — it matchmakes the handshake and forwards opaque SDP/ICE. The
  **data channel is peer-to-peer and Noise-encrypted**; the relay (and whoever hosts it) sees
  nothing but connection-setup metadata.
- **The desktop hub is the actual presence/data relay for the phone.** It's on the LAN (so it learns
  the other PCs) and on the relay (so it serves the phone). The phone sees the LAN through the hub.

### 2.3 Reachability reality (important)

```
no STUN / no TURN  ⇒  WebRTC only gathers HOST candidates  ⇒  works on the SAME LAN only.
```

There is **no NAT traversal**. Same-Wi-Fi works. Across subnets/the internet does **not**, unless
every party can reach a common relay address *and* the WebRTC peers are mutually routable (which,
without TURN, they generally aren't off-LAN). **This is the design's biggest scope limit.**

---

## 3. The data model — one log, many conversations

Every change is an **Event**:

```
Event {
  id           = SHA256(content)        // content-addressed → tamper-evident, dedup-by-hash
  conversation_id (32 bytes)            // which "channel" this belongs to
  author       = SHA256(ed25519_pub)    // who (device fingerprint)
  seq          (u64)                    // per-author sequence
  parents[]    = [EventId…]             // hash-links → a DAG (causal order)
  lamport      (u64)                    // logical clock → deterministic total order (lamport,id)
  wall_clock   (ms)                     // real time (only for LWW directories)
  kind         = Message|Edit|Delete|React|ReadMarker|MembershipChange|
                 KeyRotation|FileManifest|Profile
  ciphertext   = <opaque, E2E-encrypted payload>
  sig          = Ed25519(author, EVENT_DOMAIN ‖ id)
}
```

It's a **hash-linked, signed DAG** (a CRDT-flavoured append-only log), **not** a blockchain and not a
mutable store. Properties that fall out of this:

- **Tamper-evident & self-verifying:** id = hash, every event is signed → a relay can't forge or
  alter, only store/forward.
- **Idempotent merge:** same content = same id → syncing the same event twice is a no-op.
- **Deterministic order:** `(lamport, id)` gives every peer the same total order without coordination.
- **Convergence:** two logs that exchange their missing events end up identical.

### Special conversations (the "directories")

```
[7u8;32]  PRESENCE  — each node announces {ed25519 ‖ x25519 ‖ name}, heartbeat ~30s.  Last-Writer-Wins.
[8u8;32]  RELAYS    — relay ws:// URLs, so a phone can fail over to another relay.    Last-Writer-Wins.
dm_conversation_id(a,b) = SHA256(domain ‖ sorted(ed25519_a, ed25519_b))   // symmetric DM id
account_conversation_id(A,B)                                              // cross-device logical DM
<channel id>, <file conv> …
```

LWW directories are **compacted** (keep newest-per-author) and **stale-pruned** (drop authors not
seen for 10 min) so they can't grow without bound.

---

## 4. The sync protocol — making two logs equal

Per conversation, **id-set reconciliation** (not version vectors — the log allows seq gaps):

```
REQUESTER  R                                  RESPONDER  S
   │  ① Fingerprint  = SHA256(sorted ids_R)
   ├──── FpRequest(fp) ─────────────────────────►
   │                                     fp == SHA256(sorted ids_S) ?
   │  ◄──────────── FpMatch ─────────────────────┤   (common idle case → DONE, ~0 bytes)
   │  (mismatch ↓)
   │  ② have_R = all my ids   (streamed in ≤1500-id chunks, capped 200k)
   ├──── Request + ReqHave[chunks] ─────────────►
   │                                     S computes events R lacks = events_excluding(have_R)
   │  ◄──── Response(events R-lacks) + RespHave[have_S] ──┤   (events capped to one frame)
   │  ③ R applies events; computes events S lacks
   ├──── Followup(events S-lacks) ──────────────►   S applies
   │                                              both logs now hold the union
```

- **Bounded frames:** each round ships at most ~64 KB of events; `cap_events` always makes ≥1 event
  of progress; up to 10 000 rounds for a huge backlog.
- **Cost:** the metadata is **O(N) ids per round** (32 B each). The **fingerprint short-circuit**
  makes the steady "nothing changed" case ~free; real divergence costs O(diff). This is fine for
  LAN-sized logs; very large histories would want range-based (Negentropy-style) reconciliation.
- **The 192 MB runaway (fixed):** compacted presence beats were re-offered by peers and re-appended
  forever. Fix = LWW supersede by wall-clock + a resurrection guard + stale-author prune. Sound now,
  and deterministic (uses event timestamps, not a live clock).

---

## 5. Convergence topology — how an event reaches everyone

```
        author appends to its log
                 │
   ┌─────────────┼───────────────────────────────────────────────┐
   │ direct      │ post office (offline)        │ hub gossip (phone)│
   ▼             ▼                              ▼
 dial peer,   replicate to elected PO,      hub syncs [7;32]+DMs with
 sync conv    which holds ciphertext until   each LAN peer (≤30s) AND
 (LAN/TCP)    the recipient drains it        serves each phone spoke
```

- Reconciliation is **pairwise**: after one round, *those two* peers converge. Three+ nodes converge
  by **chaining** (A↔B, B↔C, …). ⚠️ Presence is therefore only transitive if the desktops actually
  form a connected sync graph — a partition leaves islands.
- A **phone's worldview = its hub's `[7;32]`.** If the hub didn't gossip a PC into its directory
  (e.g. the LAN dial failed), the phone can't see that PC. (This is the `gossiped 0/1 LAN peer`
  symptom.)

### Worked example — "a phone sees a PC it never scanned"

```
PC2 (LAN) ──announce──► PC1 (hub)        ① PC1 learns PC2 via UDP discovery
PC1 ──gossip [7;32]──► PC2 (TCP)         ② PC1's presence dir now contains PC2
PHONE ──WebRTC──► PC1 (relay)            ③ phone connects to the hub
PHONE ──sync [7;32]──► PC1               ④ phone pulls PC1's directory ⇒ sees PC2
```

Each arrow is independently verified by tests (and the whole chain by `tests/docker-lan/`).

---

## 6. Security model — what's protected, and from whom

```
                 sees ciphertext+routing       sees plaintext
 SIGNALING RELAY        (only SDP/ICE)               never
 POST OFFICE            yes (opaque events)          never (no keys)
 LAN PEER / CHANNEL MEMBER   their own convs         only what they're a member of
 NETWORK EAVESDROPPER   nothing useful (Noise)       never
```

Layered crypto (all standard primitives, no homegrown):

```
identity   Ed25519 (sign) + X25519 (DH);  user_id = SHA256(ed25519)
  at rest  keystore = PBKDF2-600k + AES-256-GCM  (device + account secrets, separate files)
account    Ed25519 account key signs DEVICE CERTS (account_pub ‖ device_pub) → multi-device binding
           announces carry the cert ⇒ re-homing-attack defense (device commits to its account)
transport  Noise XX  (X25519 · ChaChaPoly · BLAKE2s) mutual auth, AEAD frames
           + post-handshake Ed25519 sig over the handshake hash → binds ed25519 ↔ x25519 static
DM         Double Ratchet  → forward secrecy + post-compromise security, bounded out-of-order skip
channel    Sender-Key ratchet → per-sender FS, epoch rotation on add/remove, late-join isolation
linking    128-bit code + constant-time authenticator → account secret transferred over Noise
```

**Accepted/known gaps (by design):** metadata (who-talks-to-whom, sizes) is visible to a relay/PO;
no deniability (signatures are non-repudiable); device-linking backfill rides plaintext *inside* the
Noise channel; no out-of-band SAS verification UX. See §8 in `ARCHITECTURE.md`.

---

## 7. Where it's fragile — the review verdict

Ranked by how much it actually bites:

| # | Issue | Why it matters | Fundamental? |
|---|-------|----------------|--------------|
| 1 | **LAN-only (no STUN/TURN)** | WebRTC gathers host candidates only → no cross-subnet / internet. "Phone from anywhere" isn't supported. | Yes — needs TURN or a data-relay to change |
| 2 | **Hub = single point of failure** | Phone depends on one desktop hub; "first-in-room", no re-election/standby → hub leaves = phones stranded. | No — add hub election/failover |
| 3 | **Pairwise gossip not transitive** | Multi-desktop presence converges only if the sync graph is connected; partitions island. | No — gossip-tree / forwarded directory |
| 4 | **PWA identity is origin-coupled** | Each PC's IP = different browser origin = different IndexedDB = new identity (ghosts). Fixed origin (GitHub Pages) needs `wss://` (mixed-content). | Partly — credential-derived identity OR fixed origin + TLS relay |
| 5 | **O(N) id-set sync + uncompacted DM/file history** | Fine at LAN scale; a ceiling for very large logs / many nodes. | No — range-based reconciliation, history compaction |
| 6 | **Wall-clock LWW for presence** | Clock skew (NTP/DST) can misorder; bounded to 10-min windows. | Low impact |
| 7 | **PWA secrets less protected than desktop** | No OS keychain; "stay signed in" stores the password in IndexedDB (device-trust boundary). | Accepted for v1 |

**Feasibility verdict.** For the **stated scope — serverless, single-LAN, small group, E2E** — the
architecture is **sound and feasible**, and the crypto/data-model foundations are genuinely strong.
The ambitions that strain it are "phone as a first-class node *anywhere*" (blocked by #1/#2) and
"same identity across PCs" (blocked by #4). Those are well-understood **extensions** (TURN + hub
failover + a fixed origin / credential identity), not a redesign.

---

## 8. If we extend beyond one LAN (sketch)

```
phone (anywhere) ─wss─► public relay (signaling) ──► desktop hub
       └───────── WebRTC + TURN (relayed media) ─────────┘     ← #1 fix
hub failover: elect a hub by lowest-fingerprint among relay members,            ← #2 fix
              spokes re-offer to the new hub on PeerLeft (mirror the PO election)
identity:     KDF(username,password) → deterministic device id (origin-free)    ← #4 fix
              OR GitHub Pages (fixed origin) + a TLS (wss) relay
```

None of these touch the event log, the sync protocol, or the crypto — they're all at the
**topology/transport** edge, which is exactly where the current limits live.
