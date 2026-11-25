# Mesh Foundation source snapshot

Managed alpha.2 SDK source copied from the adjacent Mesh Foundation repository.
Mesh Talk-specific device/account profiles, discovery, events and mailbox formats
are maintained in `crates/mesh-talk-protocol`, outside this generic SDK snapshot.
The published alpha.1 tag and crates are unchanged. See `SOURCE` for the exact SDK commit. Cargo dependencies resolve to this
tracked snapshot, so a clean checkout needs no adjacent SDK repository.

When updating, fetch Mesh Foundation `main`, review the commit delta, copy the
upstream crate files and workspace manifest without local source patches, and
record the full commit SHA in `SOURCE`. Verify the crate files byte-for-byte
against that commit and run the application compatibility and workspace tests.
The SDK package version alone does not identify unreleased alpha changes.
