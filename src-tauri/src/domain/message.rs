use serde::{Deserialize, Serialize};

// Protocol constants
pub const PROTOCOL_MAGIC: u32 = 0x4D455348; // "MESH" in hex
pub const PROTOCOL_VERSION: u8 = 1;

// Protocol header structure
#[derive(Debug, Serialize, Deserialize)]
pub struct ProtocolHeader {
    pub magic: u32,
    pub version: u8,
    pub message_type: u8,
}

// Message types
pub const MESSAGE_TYPE_DISCOVERY: u8 = 1;
pub const MESSAGE_TYPE_CHAT: u8 = 2;
pub const MESSAGE_TYPE_HEARTBEAT: u8 = 3;
pub const MESSAGE_TYPE_CONTACT_REQUEST: u8 = 4;
pub const MESSAGE_TYPE_CONTACT_RESPONSE: u8 = 5;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Message {
    Discovery {
        name: String,
        port: u16,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        user_id: Option<u64>,  // Add user ID field
    },
    Chat {
        from: String,
        content: String,
    },
    Heartbeat {
        name: String,
        port: u16,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        user_id: Option<u64>,  // Add user ID field
    },
    ContactRequest {
        requester_public_key: String,
        requester_alias: String,
        timestamp: u64,
        signature: Vec<u8>,
        #[serde(default)]
        node_name: Option<String>,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        user_id: Option<u64>,  // Add user ID field
        #[serde(default)]
        ip: Option<String>,
        #[serde(default)]
        port: Option<u16>,
    },
    ContactResponse {
        responder_public_key: String,
        approved: bool,
        responder_alias: String,
        timestamp: u64,
        signature: Vec<u8>,
        #[serde(default)]
        user_id: Option<u64>,  // Add user ID field
    },
}
