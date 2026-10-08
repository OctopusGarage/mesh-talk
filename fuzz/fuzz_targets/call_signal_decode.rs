#![no_main]
use libfuzzer_sys::fuzz_target;
use mesh_talk_core::node::CallSignal;

// A live call control frame carries an opaque payload; the node decodes its envelope.
fuzz_target!(|data: &[u8]| {
    let _ = CallSignal::decode(data);
    let mut framed = b"MTCS1".to_vec();
    framed.extend_from_slice(data);
    let _ = CallSignal::decode(&framed);
});
