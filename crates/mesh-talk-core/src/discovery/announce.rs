//! Compatibility exports for signed discovery announcements.

#[cfg(test)]
pub(crate) use mesh_connect::discovery::announce::verification_observations;
pub use mesh_connect::discovery::announce::{Announce, decode, encode};
