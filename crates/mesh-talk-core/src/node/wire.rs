//! Shared magic-prefixed bincode framing for the small, NON-PERSISTED control messages that
//! ride the Noise channel out-of-band from the sync wire — device pairing ([`super::pairing`])
//! and call signaling ([`super::call`]). Each message is `magic ‖ bincode(value)`; the magic
//! lets `serve_connection` tell these apart from a sync frame on the first frame of a
//! connection. Centralized here so the two callers can't drift apart (bincode options, the
//! size guard, trailing-byte rejection).

use bincode::Options;
use serde::{Deserialize, Serialize};

/// Upper bound on a single control frame before we attempt to deserialize. Every such message
/// is small (keys, a cert, one SDP blob); a frame larger than the Noise transport's own
/// `MAX_FRAME` (65535) cannot be legitimate, so reject it before feeding bincode.
/// Defense-in-depth: the transport already caps wire frames; this guards the decode path too.
pub(in crate::node) const MAX_WIRE_FRAME: usize = 65_535;

/// `magic ‖ bincode(value)` with fixint encoding.
pub(in crate::node) fn frame<T: Serialize>(magic: &[u8], v: &T) -> Vec<u8> {
    let mut out = Vec::with_capacity(magic.len() + 64);
    out.extend_from_slice(magic);
    out.extend_from_slice(
        &bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(v)
            .expect("control message serializes"),
    );
    out
}

/// Inverse of [`frame`]: strip `magic`, bincode-decode the rest, rejecting oversized or
/// trailing-garbage frames. `None` when the magic doesn't match or the payload is malformed.
pub(in crate::node) fn unframe<T: for<'de> Deserialize<'de>>(
    magic: &[u8],
    bytes: &[u8],
) -> Option<T> {
    if bytes.len() > MAX_WIRE_FRAME {
        return None;
    }
    let rest = bytes.strip_prefix(magic)?;
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
        .deserialize::<T>(rest)
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    const MAGIC: &[u8] = b"MTWX1";

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct Sample {
        a: u32,
        b: String,
    }

    fn sample() -> Sample {
        Sample {
            a: 7,
            b: "hi".to_string(),
        }
    }

    #[test]
    fn frame_then_unframe_round_trips() {
        let bytes = frame(MAGIC, &sample());
        assert!(bytes.starts_with(MAGIC));
        assert_eq!(unframe::<Sample>(MAGIC, &bytes), Some(sample()));
    }

    #[test]
    fn a_wrong_or_missing_magic_does_not_decode() {
        let bytes = frame(MAGIC, &sample());
        assert_eq!(unframe::<Sample>(b"OTHER", &bytes), None);
        assert_eq!(unframe::<Sample>(MAGIC, b"no magic here"), None);
    }

    #[test]
    fn an_oversized_frame_is_rejected_before_decode() {
        let mut huge = MAGIC.to_vec();
        huge.resize(MAX_WIRE_FRAME + 1, 0);
        assert_eq!(unframe::<Sample>(MAGIC, &huge), None);
    }

    #[test]
    fn trailing_garbage_after_the_payload_is_rejected() {
        let mut bytes = frame(MAGIC, &sample());
        bytes.push(0xff);
        assert_eq!(unframe::<Sample>(MAGIC, &bytes), None);
    }
}
