//! Channels (group chat): the crypto primitive ([`crypto`]) and the membership +
//! per-epoch key-state model ([`model`]). Re-exported flat so callers use
//! `crate::channel::{GroupKey, ChannelState, …}`.

pub mod crypto;
pub mod model;
pub mod sender_key;

pub use crypto::{
    ChannelError, GroupKey, open_channel_message, open_group_key, seal_channel_message,
    seal_group_key,
};
pub use model::{ChannelMeta, ChannelState, new_channel_id};
