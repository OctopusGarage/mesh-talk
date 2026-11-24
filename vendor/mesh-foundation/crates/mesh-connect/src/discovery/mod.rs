//! Mesh Talk-compatible signed discovery and host route inputs.

#[cfg(feature = "signed-discovery")]
pub mod announce;
pub mod candidates;
#[cfg(feature = "signed-discovery")]
pub mod query;
