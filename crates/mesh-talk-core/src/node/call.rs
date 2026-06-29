//! Real-time call signaling (voice/video). A non-persisted control message that carries
//! opaque WebRTC signaling — an SDP offer/answer, or a "bye" — between two devices over the
//! authenticated Noise channel. UNLIKE chat, it is NEVER appended to the event log: it is
//! ephemeral and surfaced straight to the app, then forgotten.
//!
//! Security: the receiver binds a signal's sender to the cryptographically-AUTHENTICATED
//! peer identity of the Noise channel ([`SecureChannel::peer_identity`]), NOT a self-asserted
//! field in the payload. So a signal cannot be spoofed or replayed under another identity.
//! This is what makes the media leg MITM-resistant: WebRTC's DTLS-SRTP only protects bytes
//! in flight, so the DTLS fingerprint (carried inside the SDP here) is only meaningful if it
//! arrives over an authenticated channel — which this binding guarantees.
//!
//! Modeled on the device-pairing wire ([`super::pairing`]): a magic-prefixed, bincode frame
//! peeked off the first frame of a connection in `serve_connection`, distinct from a sync wire.

use super::*;
use crate::discovery::roster::UserId;
use crate::node::session::SessionError;
use crate::node::transport::dial;
use crate::node::wire::{frame, unframe};
use crate::transport::SecureChannel;
use serde::{Deserialize, Serialize};
use tokio::net::TcpStream;

const CALL_MAGIC: &[u8] = b"MTCS1";

/// The wire form: an opaque application payload (the app's JSON — call id, kind, SDP). The
/// node neither parses nor persists it; it is a dumb, authenticated pipe for the frontend's
/// WebRTC negotiation. Uses the shared magic-frame helper ([`crate::node::wire`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallSignal {
    pub payload: Vec<u8>,
}

impl CallSignal {
    pub fn encode(&self) -> Vec<u8> {
        frame(CALL_MAGIC, self)
    }

    pub fn decode(bytes: &[u8]) -> Option<Self> {
        unframe(CALL_MAGIC, bytes)
    }
}

/// A received call signal, surfaced live to the app. `from` is the AUTHENTICATED sender
/// device (taken from the Noise channel), never a self-asserted wire field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceivedCallSignal {
    pub from: UserId,
    pub payload: Vec<u8>,
}

impl Node {
    /// Send an opaque call-signaling `payload` to a specific device `target_user_id`, right
    /// now, over a fresh authenticated Noise channel. Ephemeral: nothing is appended to the
    /// event log, and there is NO post-office fallback — a live call requires both devices
    /// online, so this errors if the peer is unknown or unreachable.
    ///
    /// Always device-addressed (a call rings one device), never an account fan-out.
    pub async fn send_call_signal(
        &self,
        target_user_id: &str,
        payload: &[u8],
    ) -> Result<(), NodeError> {
        let peer = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .get(target_user_id)
            .cloned()
            .ok_or_else(|| NodeError::UnknownPeer(target_user_id.to_string()))?;
        let frame = CallSignal {
            payload: payload.to_vec(),
        }
        .encode();
        let mut channel = dial(peer.addr, &self.identity, Some(&peer.public))
            .await
            .map_err(SessionError::Transport)
            .map_err(NodeError::Session)?;
        channel
            .send(&frame)
            .await
            .map_err(SessionError::Transport)
            .map_err(NodeError::Session)?;
        Ok(())
    }

    /// Surface an inbound, already-decoded call-signal to the app, binding its sender to the
    /// channel's cryptographically-authenticated identity. Non-persisted; best-effort (a
    /// dropped receiver just means no app is listening).
    pub(in crate::node) fn serve_call_signal(
        &self,
        channel: &SecureChannel<TcpStream>,
        signal: CallSignal,
    ) {
        let from = channel.peer_identity().user_id();
        let _ = self.call_signal_incoming.send(ReceivedCallSignal {
            from,
            payload: signal.payload,
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn call_signal_round_trips() {
        let sig = CallSignal {
            payload: b"{\"kind\":\"offer\",\"sdp\":\"v=0...\"}".to_vec(),
        };
        assert_eq!(CallSignal::decode(&sig.encode()), Some(sig));
    }

    #[test]
    fn a_non_call_frame_does_not_decode() {
        // Bytes without the magic (e.g. a pairing or sync frame) must not decode as a signal.
        assert!(CallSignal::decode(b"not a call frame").is_none());
        assert!(CallSignal::decode(&[0u8, 1, 2, 3, 4, 5]).is_none());
    }

    #[test]
    fn an_oversized_frame_is_rejected_before_decode() {
        let mut huge = CALL_MAGIC.to_vec();
        huge.resize(crate::node::wire::MAX_WIRE_FRAME + 1, 0);
        assert!(CallSignal::decode(&huge).is_none());
    }
}
