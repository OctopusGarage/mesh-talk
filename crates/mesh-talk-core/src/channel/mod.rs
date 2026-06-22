//! Channels (group chat): the crypto primitive ([`crypto`]) and the membership +
//! per-epoch key-state model ([`model`]). Re-exported flat so callers use
//! `crate::channel::{GroupKey, ChannelState, …}`.

pub mod crypto;
pub mod model;
pub mod sender_key;

pub use crypto::{
    open_channel_message, open_group_key, seal_channel_message, seal_group_key, ChannelError,
    GroupKey,
};
pub use model::{new_channel_id, ChannelMeta, ChannelState};
