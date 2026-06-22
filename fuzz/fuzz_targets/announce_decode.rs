#![no_main]
use libfuzzer_sys::fuzz_target;

// A signed LAN announce arrives as an untrusted UDP datagram — decode must never panic.
fuzz_target!(|data: &[u8]| {
    let _ = mesh_talk_core::discovery::announce::decode(data);
});
