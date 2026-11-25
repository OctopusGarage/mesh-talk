//! Pairwise Double Ratchet using X25519, HKDF-SHA256 and AES-256-GCM.
//! Hosts establish authenticated shared secrets, persist serialized state securely,
//! and define admission and recovery policies. See the crate README for the
//! historical cryptographic profile and compatibility constraints.

mod kdf;
pub mod state;

pub(crate) use kdf::{kdf_ck, message_keys};

pub use state::{Header, RatchetError, RatchetState, init_alice, init_bob};
