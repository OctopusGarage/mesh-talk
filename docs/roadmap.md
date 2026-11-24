# Product roadmap

## REQ-026 — Personal feed, profile and sharing

**Status:** Deferred. Revisit audience and delivery rules before implementation.

The personal feed helps people share updates with selected contacts. The forum
exploration in REQ-019 centers on topics and community discussion.

- **Per-post audience:** The publisher selects the contacts who can see each
  post. Posts are private by default.
- **Profile visibility:** The publisher configures their profile's audience
  independently. Profile access does not grant access to individual posts.
- **Open decisions:** Define how posts reach offline contacts, how audience
  changes and withdrawal work, and whether replies or forwarding are supported.
  Define retention for personal posts independently of forum retention.
- **Acceptance to define:** Verify that an omitted contact cannot read a post
  through the profile or synchronization paths, and that delivery and withdrawal
  states reflect what recipients can still access.

## REQ-027 — Mobile PWA and paired communication nodes

**Status:** Deferred. Resume with a feasibility check on physical phones.

Let mobile browser users scan a QR code in Mesh Talk to pair with their online
desktop or an eligible post-office node. Support multiple authorized nodes and
selection of an available node. Evaluate same-LAN and remotely reachable setups.

- **Identity and authorization:** Decide whether the browser joins an existing
  account as a linked device. Define pairing, gateway identity verification,
  revocation, protected key storage and recovery after browser data loss.
- **Hosting and discovery:** Evaluate stable app hosting and ways to rediscover
  an authorized node after its address changes. Verify browser support for local
  discovery and secure transport on the target phones.
- **Delivery and performance:** Define what works offline, while the browser is
  suspended and after it resumes. Distinguish local queueing, gateway custody
  and recipient delivery. Measure startup, memory and network use on phones.
- **Existing prototype:** `origin/feature/mobile-pwa` at `1cd7e78e` contains a
  browser node and WebRTC gateway. Its feasibility notes record IP-dependent
  bootstrap, gateway identity verification and plaintext session-password
  persistence as issues to resolve before release.
- **First acceptance scenario:** Pair once, exchange messages in both directions
  with a second desktop, change the gateway IP, and repeat without rescanning
  or losing the browser identity. Then verify failover and explicit revocation.

## REQ-028 — Contribution incentives and resource priority

**Status:** Deferred. Revisit when there is measured contention for optional
shared resources.

Explore whether useful contributions should earn limited scheduling preference
for optional forum content, files or cache forwarding. Basic chat retains its
normal service regardless of contribution. Existing authorization and content
integrity checks still apply.

- **Open decisions:** Define eligible work, how to verify it, who evaluates
  contributions, and whether any reputation can travel between nodes. Points,
  signed receipts and broader reputation aggregation are research options.
- **First experiment:** Under measured contention for one optional resource,
  compare ordinary scheduling with a limited preference based on useful work
  observed by the serving node. Distinguish enabling sharing from completing
  useful forwarding.
- **Acceptance:** Verify that new and low-contribution peers continue to get
  service, basic chat is unaffected, and duplicate or replayed claims do not
  earn extra credit. Measure the CPU, network and storage cost of accounting.
