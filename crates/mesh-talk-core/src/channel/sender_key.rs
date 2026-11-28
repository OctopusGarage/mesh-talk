//! Compatibility exports for the existing group sender-key API.

pub(crate) use mesh_e2ee::sender_key::MAX_SKIP;
pub use mesh_e2ee::sender_key::{
    SenderChain, SenderKey, SenderKeyDistribution, SenderKeyError, open_message, seal_message,
};
