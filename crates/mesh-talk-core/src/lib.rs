//! Mesh-Talk core — the serverless, end-to-end-encrypted LAN messaging protocol.
//!
//! This crate is UI-free and embeddable: it is the foundation an SDK builds on. It
//! provides the whole protocol stack and nothing about any particular front end:
//!
//! - [`identity`] — Ed25519/X25519 device identities, accounts, and device certificates.
//! - [`eventlog`] — a content-addressed, hash-linked, encrypted event log + peer sync.
//! - [`ratchet`] / [`channel`] — Double Ratchet (DM) and sender-key (group) forward secrecy.
//! - [`dm`] — sealed direct-message envelopes.
//! - [`transport`] — a Noise (`snow`) secure channel with identity binding.
//! - [`discovery`] — LAN peer discovery.
//! - [`postoffice`] — a store-and-forward relay for offline delivery.
//! - [`file`] — chunked, encrypted file transfer.
//! - [`node`] — the runtime that wires the above into a working peer.
//!
//! The desktop app and the `mesh-talk-node` CLI are thin layers over this crate.

// Pragmatic, intentional lint allowances (the substantive clippy lints stay on):
//  - module_inception: test modules are `mod tests` inside `*/tests.rs`.
//  - too_many_arguments: a few constructors; refactor tracked separately.
//  - assertions_on_constants: placeholder "can construct" smoke tests.
//  - single_match: a couple of intentional single-arm matches.
//  - manual_flatten: a nested `if let Ok` loop kept for readability.
#![allow(
    clippy::module_inception,
    clippy::too_many_arguments,
    clippy::assertions_on_constants,
    clippy::single_match,
    clippy::manual_flatten
)]

pub mod channel;
pub mod discovery;
pub mod dm;
pub mod eventlog;
pub mod file;
pub mod identity;
pub mod node;
pub mod postoffice;
pub mod ratchet;
pub mod storage;
pub mod transport;
pub mod util;
