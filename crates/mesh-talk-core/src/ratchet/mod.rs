//! Double Ratchet for DMs (Phase 3): forward secrecy + post-compromise security over
//! the existing X25519 / HKDF-SHA256 / AES-256-GCM primitives. `kdf` is the key
//! schedule; this module is the state machine. Pure — durable state + the Node DM
//! integration are later plans.

mod kdf;
pub mod state;

pub(crate) use kdf::{kdf_ck, message_keys};

pub use state::{init_alice, init_bob, Header, RatchetError, RatchetState};
