#![no_main]
use libfuzzer_sys::fuzz_target;

// DmEnvelope::decode parses the account-routing envelope from an opened DM.
fuzz_target!(|data: &[u8]| {
    let _ = mesh_talk_core::node::DmEnvelope::decode(data);
});
