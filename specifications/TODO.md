# Implementation Status

The legacy server/relay stack has been **retired**; the serverless, end-to-end-encrypted
stack is the entire product and is feature-complete:

- **Phase 0** — identity/keystore, Noise transport, content-addressed event log + sync,
  X3DH DM crypto, persistent history, post office (store-and-forward). ✅
- **Phase 1** — LAN discovery + node wiring, group **channels**. ✅
- **Phase 2** — file sharing, reactions, replies/threads, @mentions, search. ✅
- **Phase 3** — forward secrecy (DM **Double Ratchet** + channel **sender-key** ratchet),
  **multi-device** (account identity, device linking, account-addressed fan-out +
  self-sync, history backfill, re-key). ✅

Architecture: **[`docs/ARCHITECTURE.md`](../docs/ARCHITECTURE.md)**. Design specs +
per-phase implementation plans: **`docs/superpowers/specs/`** and
**`docs/superpowers/plans/`**. Detailed task history lives in git.

## Design notes & external constraints (not open work)

No actionable correctness/feature work is open. The items below are deliberate design
choices or upstream-gated dependencies, not unfinished tasks:

- **Device-linking authentication is the one-time code, by design.** The pairing
  authenticator is `SHA-256(code ‖ linker_ed ‖ joiner_ed)` — it binds *both* device keys,
  so it already provides the key-confirmation a separate SAS would (a MITM that lacks the
  code, or substitutes its own key, fails the MAC). Backfill is plaintext history streamed
  from the code-authenticated linker, which is inside the trust boundary (it hands you the
  account secret) — there is nothing further to cryptographically re-verify.
- **A relay sees metadata, inherently.** The post office needs each event's `author` (to
  validate its signature before storing) and its conversation id (to route), so it can
  derive the participant pair; only content stays encrypted. Removing this needs
  onion-routing / mixing — a different architecture, out of scope.
- **`glib` is pinned to 0.18 by tauri's gtk stack.** A bump to 0.20 is gated on upstream
  tauri; `.github/workflows/glib-0.20-watch.yml` watches for it monthly.

Behavioural note (intentional): a member removed from a channel no longer renders their past
reactions there — reaction visibility follows current membership.
