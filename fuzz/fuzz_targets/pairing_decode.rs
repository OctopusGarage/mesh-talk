#![no_main]
use libfuzzer_sys::fuzz_target;
use mesh_talk_core::node::{BackfillRecord, PairingCode, PairingRequest, PairingResponse};

// Linking controls arrive on the authenticated Noise connection as separate frames.
fuzz_target!(|data: &[u8]| {
    let _ = PairingRequest::decode(data);
    let _ = PairingResponse::decode(data);
    let _ = BackfillRecord::decode(data);
    if let Ok(code) = std::str::from_utf8(data) {
        let _ = PairingCode::from_hex(code);
    }
    for magic in [b"MTPQ1", b"MTPS1", b"MTBF1"] {
        let mut framed = magic.to_vec();
        framed.extend_from_slice(data);
        let _ = PairingRequest::decode(&framed);
        let _ = PairingResponse::decode(&framed);
        let _ = BackfillRecord::decode(&framed);
    }
});
