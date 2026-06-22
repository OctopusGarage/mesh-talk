#![no_main]
use libfuzzer_sys::fuzz_target;

// A file-transfer manifest describes chunks/keys; it arrives from a peer.
fuzz_target!(|data: &[u8]| {
    let _ = mesh_talk_core::file::manifest::FileManifest::decode(data);
});
