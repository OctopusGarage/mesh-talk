//! Compatibility exports for the original signed event codec.

#[cfg(test)]
pub(crate) use mesh_replication::legacy_event::signature_observations;
pub use mesh_replication::legacy_event::{Author, ConversationId, Event, EventId, EventKind};
