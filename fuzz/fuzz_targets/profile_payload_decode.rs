#![no_main]
use libfuzzer_sys::fuzz_target;
use mesh_talk_core::node::ProfilePayload;

// Peer profiles carry signed avatar bytes. Exercise framing and signature checks.
fuzz_target!(|data: &[u8]| {
    if let Some(profile) = ProfilePayload::decode(data) {
        let _ = profile.verify();
    }
    let mut framed = b"MTPF1".to_vec();
    framed.extend_from_slice(data);
    if let Some(profile) = ProfilePayload::decode(&framed) {
        let _ = profile.verify();
    }
});
