# Security Policy

## Reporting a Vulnerability

**Do not open a public GitHub issue for security vulnerabilities.**

Use [GitHub Private Vulnerability Reporting](https://github.com/OctopusGarage/mesh-talk/security/advisories/new)
to submit a report privately. We will acknowledge the report within 7 days and
keep you informed as we work on a fix.

## Scope

Mesh-Talk is a peer-to-peer LAN chat client that runs on each user's own machine;
there is no central server. Key security properties:

- **No central trust** — peers are discovered over signed UDP multicast and connect
  directly over a Noise-encrypted TCP channel on the local network.
- **End-to-end encryption** — DMs use a Double Ratchet, group
  channels a sender-key ratchet; transport is Noise (`snow`) with the peer's device
  identity bound into the handshake. Relays only ever see ciphertext. Forward
  secrecy has the bootstrap and key-distribution limitations described below.
- **Encryption at rest** — per-device/account keystores and the message stores are
  encrypted with a key derived from the user's password (PBKDF2-600k + AES-256-GCM);
  they are unlocked only after login. Locally managed media uses authenticated
  encryption; user-exported files remain readable in their chosen destination.
- **Saved sessions and recovery** — optional saved login credentials live in the OS
  credential store (Touch ID protected Keychain on supported macOS versions). Manual
  `.mtbackup` exports use a separate recovery key that Mesh-Talk does not retain.
- **No cloud account or telemetry** — protocol and account stores live under
  `~/.mesh-talk/` (Windows: `%USERPROFILE%\.mesh-talk`). Optional packs and some
  UI preferences live in webview storage. Messages and profile photos are sent
  to intended peers; the optional post office stores only encrypted events.

## Known limitations

This is an experimental project. The following are tracked, not yet hardened:

- The initial DM sending chain uses the recipient's permanent X25519 key as
  its bootstrap ratchet key. Recorded messages from that chain can be recovered
  after later compromise of that recipient key, even if the conversation has
  since advanced. The affected chain lasts until the initiator processes the
  responder's first fresh DH ratchet header; this can include a long offline
  burst. This does not expose all subsequent DH chains.
- Group sender distributions are sealed to members' permanent X25519 keys and
  retained in the event log. Later compromise of one member's recipient key
  permits recovery of recorded distributions addressed to that member and the
  corresponding historical group messages. Sender-chain advancement alone does
  not provide forward secrecy against this attack. A new client or post office
  without a recipient's secret cannot perform either retrospective attack.
- Contact requests/responses are signed with the sender's ed25519 key and
  verified on receipt (a present-but-invalid signature is rejected). However,
  the binding between a `user_id` and its public key is trust-on-first-use —
  there is no key directory, so a first contact from an unknown key is accepted.
- LAN discovery does not provide a central identity directory. Verify a contact's
  safety number through a trusted channel before relying on that identity; the
  authenticated Noise connection protects subsequent transport.
- A backup restore preserves the account identity but creates a new device. It does
  not revoke a lost or compromised old device. Restored channels are read-only until
  a new invitation; see [backup and recovery](docs/backup-recovery.md).
- Encryption and app locking cannot hide content displayed, copied, notified, or
  exported on a device controlled by someone else. Privacy presets reduce some of
  those exposures but cannot prevent privileged screen or endpoint monitoring.

Please mention any of these in a report only if you have a concrete exploit
beyond the documented limitation.

## Security tooling (CI)

The normal CI checks run on pushes; scheduled or manual jobs cover the slower checks:

- **SAST**: CodeQL (code scanning) + `cargo clippy -D warnings`.
- **Secrets**: gitleaks (history + diff).
- **Dependencies**: `cargo-deny` (advisories/licenses/bans/sources), `cargo-audit`
  (RUSTSEC), `cargo-machete` (unused), Dependabot, and a PR **Dependency Review** gate.
- **Fuzzing**: `cargo-fuzz` targets over selected public wire and opened-payload decoders
  (weekly + on demand), with an always-on `decoder_smoke` test as the fast gate.
- **Mutation testing**: `cargo-mutants` (weekly + on PR diff) to catch weak tests.
- **Coverage**: `cargo-llvm-cov` → Codecov.
- **Posture score**: OpenSSF **Scorecard** (weekly), published to the badge above.

## Verifying a download

Every release artifact is a per-platform `.zip` published with three independent proofs.
Download the `.zip`, its `.zip.bundle` (Sigstore signature), and — for a tagged release —
the SBOM, then verify:

1. **Checksum** — each zip contains a `SHA256SUMS`; after unzipping:
   ```bash
   sha256sum -c SHA256SUMS        # macOS: shasum -a 256 -c SHA256SUMS
   ```
2. **Sigstore signature** (proves the zip came from this repo's release workflow, keyless):
   ```bash
   cosign verify-blob \
     --bundle "mesh-talk_<ver>_<os>_<arch>.zip.bundle" \
     --certificate-oidc-issuer https://token.actions.githubusercontent.com \
     --certificate-identity-regexp '^https://github.com/OctopusGarage/mesh-talk/.github/workflows/release.yml@.*$' \
     "mesh-talk_<ver>_<os>_<arch>.zip"
   ```
3. **SLSA build provenance** (GitHub-native attestation of where/how it was built):
   ```bash
   gh attestation verify "mesh-talk_<ver>_<os>_<arch>.zip" --repo OctopusGarage/mesh-talk
   ```
4. **SBOM** — a CycloneDX `mesh-talk.cdx.json` (full dependency inventory) is attached to
   each tagged release for auditing the supply chain.

A failed checksum or signature means the download was tampered with or did not come from
this project — do not run it.
