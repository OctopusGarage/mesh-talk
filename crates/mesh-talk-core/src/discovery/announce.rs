//! Compatibility exports for signed discovery announcements.

#[cfg(test)]
pub(crate) use mesh_talk_protocol::discovery::announce::verification_observations;
pub use mesh_talk_protocol::discovery::announce::{Announce, decode, encode};
