#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = mesh_talk_core::channel::model::ChannelMeta::decode(data);
});
