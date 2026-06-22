#![no_main]
use libfuzzer_sys::fuzz_target;

// The Double Ratchet message header travels on the wire with each ciphertext.
fuzz_target!(|data: &[u8]| {
    let _ = mesh_talk_core::ratchet::Header::decode(data);
});
