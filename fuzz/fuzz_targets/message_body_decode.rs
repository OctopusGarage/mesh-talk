#![no_main]
use libfuzzer_sys::fuzz_target;

// MessageBody::decode parses a (post-decryption) DM body frame with a raw-text fallback.
fuzz_target!(|data: &[u8]| {
    let _ = mesh_talk_core::node::MessageBody::decode(data);
});
