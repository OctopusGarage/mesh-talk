# Protocol fuzzing

The `fuzz/` crate is outside the main Cargo workspace because its libFuzzer runner uses a
nightly toolchain. Its targets call the same public decode entry points used for untrusted
network frames and opened peer payloads. The weekly and manual [Fuzz workflow](../.github/workflows/fuzz.yml)
iterates over every target; a crash artifact is uploaded for reproduction.

The v0.2.0 targets cover LAN announcements, message and reaction payloads, DM and recall
envelopes, channel metadata and sender keys, ratchet state and headers, file manifests,
signed account profiles, device-pairing frames, and call-signal envelopes. The new framed
targets try both the raw input and the expected magic prefix so mutations reach the
payload parser as well as the framing check. Profile inputs also exercise signature
verification when decoding succeeds. Call signaling is experimental; this target covers
its Rust envelope decoder, not media or WebRTC behavior.

```bash
# Fast compile check; stable Rust is sufficient for this check.
cargo check --manifest-path fuzz/Cargo.toml --bins

# Coverage-guided run (requires rustup nightly and cargo-fuzz).
cd fuzz
cargo +nightly fuzz list
cargo +nightly fuzz run profile_payload_decode -- -max_total_time=120
```

Fuzzing checks for panics and crashes. It does not prove that accepted payloads are
semantically valid or that the end-to-end protocol is secure. Keep deterministic decoder
assertions in the core tests and run `make e2e` for real multi-process behavior.
