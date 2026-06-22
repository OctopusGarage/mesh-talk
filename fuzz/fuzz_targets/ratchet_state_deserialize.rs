#![no_main]
use libfuzzer_sys::fuzz_target;

// Restoring ratchet state from the (encrypted-at-rest, but treat as untrusted) store.
fuzz_target!(|data: &[u8]| {
    let _ = mesh_talk_core::ratchet::RatchetState::deserialize(data);
});
