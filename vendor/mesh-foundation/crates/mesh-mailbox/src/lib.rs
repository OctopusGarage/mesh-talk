//! Opaque store-and-forward mechanisms with host-owned authentication and policy.
//! Models and typed operations are independent from the optional network/storage profiles.
//! Content hashes establish integrity, never permission or authorship.
pub mod model;
pub use model::{MailboxCursor, MailboxDeltaPage, Record, RecordId, ScopeId};
pub mod capacity;
pub mod client;
pub mod delta;
pub mod ports;
#[cfg(feature = "wire")]
pub mod rpc;
#[cfg(feature = "wire")]
pub mod service;
#[cfg(feature = "spool")]
pub mod spool;
#[cfg(feature = "wire")]
pub mod wire;
