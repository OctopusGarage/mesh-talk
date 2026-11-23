//! Authenticated, end-to-end-encrypted peer transport.
//!
//! A Noise XX handshake (`Noise_XX_25519_ChaChaPoly_BLAKE2s`) keyed by the
//! device's X25519 identity key establishes a [`session::Session`]; a short
//! identity-auth exchange ([`auth`]) then binds the Noise static key to the
//! device's Ed25519 identity. [`channel::SecureChannel`] drives the whole
//! thing over any async stream with length-prefixed framing.

pub mod auth;
pub mod ble;
pub mod channel;
pub mod diagnostics;
pub mod handshake;
/// Low-level socket plumbing (UDP discovery socket, multicast joins, dual-stack
/// TCP listener) shared by discovery and the node runtime. Dependency-free of the
/// higher layers, so it sits at the bottom of the stack.
pub mod net;
mod presence_auth;
pub mod session;

pub use channel::SecureChannel;
pub use presence_auth::VerifiedPeer;
pub use session::Session;

pub use mesh_connect::{MAX_FRAME, MAX_PLAINTEXT, NOISE_PARAMS, TransportError};

#[cfg(test)]
mod tests;
