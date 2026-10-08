#![no_main]
use libfuzzer_sys::fuzz_target;
use mesh_talk_core::node::RecallEnvelope;

// Opened account-addressed recall events have a distinct magic frame.
fuzz_target!(|data: &[u8]| {
    let _ = RecallEnvelope::decode(data);
    let mut framed = b"MTRC1".to_vec();
    framed.extend_from_slice(data);
    let _ = RecallEnvelope::decode(&framed);
});
