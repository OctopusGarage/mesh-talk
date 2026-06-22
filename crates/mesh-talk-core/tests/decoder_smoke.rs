//! Smoke test: every untrusted-input wire decoder must survive ARBITRARY bytes without
//! panicking — it must return `Err`/`None`/a fallback, never crash. This is the cheap,
//! always-run (stable toolchain) complement to the coverage-guided fuzz targets in `fuzz/`
//! (which need nightly + cargo-fuzz and run on a schedule). The exact same public decoder
//! entry points are fuzzed there; keep the two lists in sync.
//!
//! A panic here = a remotely-triggerable crash (these all parse bytes that arrive from a
//! peer, post-decryption or off the wire), so this test failing is a real bug, not flake.

/// Run every untrusted-input decoder on `data`. A panic in any of them fails the test.
fn run_all_decoders(data: &[u8]) {
    let _ = mesh_talk_core::discovery::announce::decode(data);
    let _ = mesh_talk_core::node::MessageBody::decode(data);
    let _ = mesh_talk_core::node::DmEnvelope::decode(data);
    let _ = mesh_talk_core::node::ReactionEnvelope::decode(data);
    let _ = mesh_talk_core::node::ReactionPayload::decode(data);
    let _ = mesh_talk_core::ratchet::Header::decode(data);
    let _ = mesh_talk_core::ratchet::RatchetState::deserialize(data);
    let _ = mesh_talk_core::channel::sender_key::SenderKey::deserialize(data);
    let _ = mesh_talk_core::channel::sender_key::SenderKeyDistribution::decode(data);
    let _ = mesh_talk_core::channel::model::ChannelMeta::decode(data);
    let _ = mesh_talk_core::file::manifest::FileManifest::decode(data);
}

#[test]
fn decoders_survive_arbitrary_bytes() {
    // Empty + every single byte (catches length-prefix / first-byte-tag handling).
    run_all_decoders(&[]);
    for b in 0u8..=255 {
        run_all_decoders(&[b]);
    }

    // Structured edge cases: zeros, all-ones, the wire magic prefixes (so the decoders
    // get past the magic check and into the bincode body), and oversized buffers.
    let zeros = [0u8; 256];
    let ones = [0xFFu8; 256];
    let big = vec![0xABu8; 4096];
    let mut patterns: Vec<Vec<u8>> = vec![
        zeros.to_vec(),
        ones.to_vec(),
        big,
        b"MTB1".to_vec(),
        b"MTDE1".to_vec(),
        b"MTRX1".to_vec(),
        b"MTAN".to_vec(),
    ];
    // Each magic followed by a huge little-endian length prefix + a few bytes — the classic
    // "claim a giant Vec, deliver nothing" allocation-DoS probe.
    for magic in [&b"MTB1"[..], &b"MTDE1"[..], &b"MTRX1"[..]] {
        let mut v = magic.to_vec();
        v.extend_from_slice(&u64::MAX.to_le_bytes());
        v.extend_from_slice(&[1, 2, 3, 4]);
        patterns.push(v);
    }
    for p in &patterns {
        run_all_decoders(p);
    }

    // Deterministic pseudo-random sweep (a small LCG — no rng dependency, reproducible) over
    // a spread of lengths, hitting bincode length/enum-tag boundaries.
    let mut x: u64 = 0x1234_5678_9abc_def0;
    let mut next = || {
        x = x
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        (x >> 33) as u8
    };
    for &len in &[
        0usize, 1, 2, 4, 7, 8, 16, 31, 32, 33, 48, 64, 100, 128, 255, 256, 512, 1000,
    ] {
        for _ in 0..150 {
            let buf: Vec<u8> = (0..len).map(|_| next()).collect();
            run_all_decoders(&buf);
        }
    }
}
