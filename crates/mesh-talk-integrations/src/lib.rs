//! Revocable local messaging capabilities shared by desktop and headless hosts.
pub mod client;
pub mod host;
pub mod ipc;
pub mod model;
pub mod policy;
pub mod private_files;
pub mod service;

pub use model::*;
pub use service::{CommitAuthorization, IntegrationHost, IntegrationService};
