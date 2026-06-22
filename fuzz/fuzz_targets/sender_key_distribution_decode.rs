#![no_main]
use libfuzzer_sys::fuzz_target;

// A sender-key distribution message is received from other channel members.
fuzz_target!(|data: &[u8]| {
    let _ = mesh_talk_core::channel::sender_key::SenderKeyDistribution::decode(data);
});
