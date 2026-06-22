//! The chat-message envelope. A `kind = Message` event's plaintext is a framed
//! `MessageBody` (text + optional `reply_to`) rather than raw bytes, so a reply can
//! reference its parent. Backward-compatible: anything not starting with the magic
//! (or not decoding) is treated as a legacy raw-text message (`reply_to = None`).

use crate::eventlog::event::EventId;
use bincode::Options;
use serde::{Deserialize, Serialize};

/// Frames a structured body so it's distinguishable from legacy raw-text plaintext.
/// `MTB1` is the v1 framing (magic only, no version byte): `MTB1 ‖ bincode(body)`.
const MSG_MAGIC: &[u8] = b"MTB1";

/// Reserved magic for the FUTURE versioned framing: `MTB2 ‖ version(u8) ‖ body`.
/// Not emitted yet (so v0.1.0 peers keep parsing our `MTB1` output verbatim); the
/// decoder already recognizes it so the next body change is purely additive — see
/// the module/`decode` docs for the migration path.
const MSG_MAGIC_V2: &[u8] = b"MTB2";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageBody {
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
}

impl MessageBody {
    pub fn new(text: Vec<u8>, reply_to: Option<EventId>) -> Self {
        MessageBody { text, reply_to }
    }

    /// `MSG_MAGIC ‖ bincode(self)` — the plaintext that gets sealed.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(MSG_MAGIC.len() + 64);
        out.extend_from_slice(MSG_MAGIC);
        out.extend_from_slice(
            &bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(self)
                .expect("message body serializes"),
        );
        out
    }

    /// Recover a body from sealed-then-opened plaintext.
    ///
    /// Accepts three framings, in order:
    /// 1. `MTB1 ‖ bincode(body)` — the CURRENT framing this build emits.
    /// 2. `MTB2 ‖ version(u8) ‖ <version-specific body>` — the FUTURE versioned
    ///    framing. Recognized now (forward-readiness) but never emitted yet, so
    ///    today's known versions are empty and any `MTB2` payload falls through.
    /// 3. Anything else — a legacy raw-text message (`reply_to = None`).
    ///
    /// ## Migration path (why no version byte is added in place today)
    /// `MessageBody` is bincode (positional), and `MTB1` has no version field, so a
    /// version byte cannot be inserted without shifting every following byte and
    /// breaking already-deployed v0.1.0 peers. We therefore keep EMITTING `MTB1`
    /// verbatim and only add decoder-side readiness for an `MTB2` frame. When a body
    /// change is needed, emit `MTB2 ‖ 1 ‖ bincode(new_body)` and add a `decode_v2(1, …)`
    /// arm here; `MTB1` decoding stays for backward compatibility with old peers.
    pub fn decode(bytes: &[u8]) -> MessageBody {
        if let Some(rest) = bytes.strip_prefix(MSG_MAGIC) {
            if let Ok(body) = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .reject_trailing_bytes()
                .deserialize::<MessageBody>(rest)
            {
                return body;
            }
        } else if let Some(rest) = bytes.strip_prefix(MSG_MAGIC_V2) {
            // `MTB2 ‖ version(u8) ‖ body`. No versions are defined yet, so this never
            // matches a real frame today; it exists so the next change is additive.
            if let Some((&_version, _body)) = rest.split_first() {
                // Future: `match version { 1 => decode_v2_body(body), _ => {} }`.
            }
        }
        MessageBody {
            text: bytes.to_vec(),
            reply_to: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_with_and_without_reply() {
        let plain = MessageBody::new(b"hello".to_vec(), None);
        assert_eq!(MessageBody::decode(&plain.encode()), plain);
        let reply = MessageBody::new(b"yes!".to_vec(), Some(EventId::new([7u8; 32])));
        assert_eq!(MessageBody::decode(&reply.encode()), reply);
    }

    #[test]
    fn legacy_raw_text_decodes_as_textonly() {
        // A pre-envelope message (raw UTF-8, no magic) still opens.
        let body = MessageBody::decode(b"just raw text");
        assert_eq!(body.text, b"just raw text");
        assert_eq!(body.reply_to, None);
    }

    #[test]
    fn current_encoding_matches_golden_bytes() {
        // Pins the MTB1 wire layout. Reordering/adding a field, or changing the
        // bincode config, breaks these — protecting already-deployed v0.1.0 peers.
        let with_reply = MessageBody::new(b"hi".to_vec(), Some(EventId::new([7u8; 32])));
        assert_eq!(
            hex::encode(with_reply.encode()),
            "4d54423102000000000000006869\
01\
0707070707070707070707070707070707070707070707070707070707070707"
        );
        let no_reply = MessageBody::new(b"hi".to_vec(), None);
        assert_eq!(
            hex::encode(no_reply.encode()),
            "4d5442310200000000000000686900"
        );
        // Decode round-trip on the golden inputs.
        assert_eq!(MessageBody::decode(&with_reply.encode()), with_reply);
        assert_eq!(MessageBody::decode(&no_reply.encode()), no_reply);
    }

    #[test]
    fn future_mtb2_frame_is_tolerated_as_raw_for_now() {
        // No MTB2 version is defined yet, so an MTB2-framed payload must not panic
        // and must fall back to raw text — proving the readiness path is inert today.
        let mut bytes = b"MTB2".to_vec();
        bytes.push(1u8); // a hypothetical future version
        bytes.extend_from_slice(b"future body");
        let body = MessageBody::decode(&bytes);
        assert_eq!(body.text, bytes);
        assert_eq!(body.reply_to, None);
    }

    #[test]
    fn text_that_starts_like_magic_but_isnt_a_body_falls_back() {
        // Bytes beginning with the magic but not a valid body are treated as raw.
        let mut bytes = MSG_MAGIC.to_vec();
        bytes.extend_from_slice(&[0xFF, 0xFF]); // not a valid MessageBody encoding
        let body = MessageBody::decode(&bytes);
        assert_eq!(body.text, bytes);
        assert_eq!(body.reply_to, None);
    }
}
