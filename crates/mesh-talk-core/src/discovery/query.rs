//! Compatibility exports for routed discovery query exchanges.

pub(crate) use mesh_talk_protocol::discovery::query::encode_response_signed;
pub use mesh_talk_protocol::discovery::query::{
    Nonce, QueryTracker, decode_query, decode_response, encode_query, encode_response,
};
