//! Content encryption state machines for host-managed key lifecycles.
//!
//! Callers supply authenticated key material, enforce epoch and membership
//! authority, and persist each accepted event atomically with its new state.
//! Checkpoints contain secrets and require protected local storage.
//! These state machines do not implement key distribution or account recovery.

pub mod ratchet;
pub mod sender_key;
