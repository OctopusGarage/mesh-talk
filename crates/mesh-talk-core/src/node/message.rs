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

/// Magic for the versioned framing: `MTB2 ‖ version(u8) ‖ bincode(body)`. Emitted only
/// when a field beyond the `MTB1` layout is set (today: an animated-sticker reference), so
/// plain text/replies keep shipping as `MTB1` and old peers keep parsing those verbatim.
const MSG_MAGIC_V2: &[u8] = b"MTB2";
/// `MTB2` body version that carries the optional `sticker` field (text + reply_to + sticker).
const MTB2_V_STICKER: u8 = 1;

/// The chat-message body. `text` is the message (or, for a sticker, the fallback emoji
/// char); `reply_to` is the parent for a threaded reply; `sticker` is an animated-sticker
/// id (an emoji codepoint string like `1f602`) sent by reference — both peers bundle the
/// same set, and `text` carries the emoji char so a peer without that sticker still shows
/// something.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageBody {
    pub text: Vec<u8>,
    pub reply_to: Option<EventId>,
    pub sticker: Option<String>,
}

/// `MTB1` wire body — the original 2-field layout. Kept BYTE-IDENTICAL (its own struct, in
/// declaration order) so plain text/reply messages encode exactly as deployed v0.1.0 expects.
#[derive(Serialize, Deserialize)]
struct WireV1 {
    text: Vec<u8>,
    reply_to: Option<EventId>,
}

/// `MTB2` v1 wire body — adds `sticker`. Positional bincode: never reorder these fields.
#[derive(Serialize, Deserialize)]
struct WireV2 {
    text: Vec<u8>,
    reply_to: Option<EventId>,
    sticker: Option<String>,
}

fn opts() -> impl Options {
    bincode::DefaultOptions::new().with_fixint_encoding()
}

impl MessageBody {
    pub fn new(text: Vec<u8>, reply_to: Option<EventId>) -> Self {
        MessageBody {
            text,
            reply_to,
            sticker: None,
        }
    }

    /// A sticker message: `sticker_id` is the emoji codepoint id; `fallback` is the emoji
    /// char bytes shown if the recipient lacks that bundled sticker.
    pub fn sticker(sticker_id: String, fallback: Vec<u8>) -> Self {
        MessageBody {
            text: fallback,
            reply_to: None,
            sticker: Some(sticker_id),
        }
    }

    /// Encode the sealed plaintext. A sticker message ships as `MTB2 ‖ 1 ‖ bincode(WireV2)`;
    /// everything else ships as `MTB1 ‖ bincode(WireV1)` (unchanged on the wire).
    pub fn encode(&self) -> Vec<u8> {
        if self.sticker.is_some() {
            let body = opts()
                .serialize(&WireV2 {
                    text: self.text.clone(),
                    reply_to: self.reply_to,
                    sticker: self.sticker.clone(),
                })
                .expect("message body serializes");
            let mut out = Vec::with_capacity(MSG_MAGIC_V2.len() + 1 + body.len());
            out.extend_from_slice(MSG_MAGIC_V2);
            out.push(MTB2_V_STICKER);
            out.extend_from_slice(&body);
            out
        } else {
            let body = opts()
                .serialize(&WireV1 {
                    text: self.text.clone(),
                    reply_to: self.reply_to,
                })
                .expect("message body serializes");
            let mut out = Vec::with_capacity(MSG_MAGIC.len() + body.len());
            out.extend_from_slice(MSG_MAGIC);
            out.extend_from_slice(&body);
            out
        }
    }

    /// Recover a body from sealed-then-opened plaintext.
    ///
    /// Accepts, in order:
    /// 1. `MTB1 ‖ bincode(WireV1)` — plain text / reply (the common case).
    /// 2. `MTB2 ‖ version(u8) ‖ bincode(body)` — versioned; v1 carries `sticker`. An
    ///    unknown version or an undecodable body falls through to raw text.
    /// 3. Anything else — a legacy raw-text message.
    pub fn decode(bytes: &[u8]) -> MessageBody {
        if let Some(rest) = bytes.strip_prefix(MSG_MAGIC) {
            if let Ok(b) = opts().reject_trailing_bytes().deserialize::<WireV1>(rest) {
                return MessageBody {
                    text: b.text,
                    reply_to: b.reply_to,
                    sticker: None,
                };
            }
        } else if let Some(rest) = bytes.strip_prefix(MSG_MAGIC_V2) {
            if let Some((&version, body)) = rest.split_first() {
                if version == MTB2_V_STICKER {
                    if let Ok(b) = opts().reject_trailing_bytes().deserialize::<WireV2>(body) {
                        return MessageBody {
                            text: b.text,
                            reply_to: b.reply_to,
                            sticker: b.sticker,
                        };
                    }
                }
            }
        }
        MessageBody {
            text: bytes.to_vec(),
            reply_to: None,
            sticker: None,
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
    fn sticker_round_trips_via_mtb2() {
        // A sticker ships as MTB2 v1 with the codepoint id + an emoji-char fallback.
        let s = MessageBody::sticker("1f602".to_string(), "😂".as_bytes().to_vec());
        let decoded = MessageBody::decode(&s.encode());
        assert_eq!(decoded, s);
        assert_eq!(decoded.sticker.as_deref(), Some("1f602"));
        assert_eq!(decoded.text, "😂".as_bytes());
        // A plain text message stays MTB1 (no sticker, byte-compatible with v0.1.0).
        assert!(s.encode().starts_with(b"MTB2"));
        assert!(MessageBody::new(b"hi".to_vec(), None)
            .encode()
            .starts_with(b"MTB1"));
    }

    #[test]
    fn unknown_or_garbage_mtb2_falls_back_to_raw() {
        // An unknown MTB2 version, or a v1 frame whose body doesn't decode, must not panic
        // and must fall back to raw text rather than dropping the message.
        let mut unknown_version = b"MTB2".to_vec();
        unknown_version.push(99u8);
        unknown_version.extend_from_slice(b"whatever");
        assert_eq!(MessageBody::decode(&unknown_version).text, unknown_version);

        let mut bad_v1_body = b"MTB2".to_vec();
        bad_v1_body.push(1u8); // version 1, but not a valid WireV2 bincode
        bad_v1_body.extend_from_slice(b"\xff\xff");
        let body = MessageBody::decode(&bad_v1_body);
        assert_eq!(body.text, bad_v1_body);
        assert_eq!(body.sticker, None);
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
