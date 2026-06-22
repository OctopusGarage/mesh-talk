//! Small, dependency-free helpers shared across the crate: filename sanitization +
//! collision-avoiding de-dup for received-file saves, a safety number derived from
//! a fingerprint, and a bounded-concurrency fan-out wrapper.

pub mod fanout;
pub mod safety_number;
pub mod savename;
