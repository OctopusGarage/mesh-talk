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
pub const MESSAGE_TYPE_FILE: u8 = 6;
pub const MESSAGE_TYPE_FILE_ACK: u8 = 7;
pub const MESSAGE_TYPE_FILE_COMPLETE: u8 = 8;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum Message {
    Discovery {
        name: String,
        port: u16,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        user_id: Option<String>, // Add user ID field
    },
    Chat {
        from: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_user_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        from_address: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        to_user_id: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        to_address: Option<String>,
    },
    Heartbeat {
        name: String,
        port: u16,
        #[serde(default)]
        username: Option<String>,
        #[serde(default)]
        user_id: Option<String>, // Add user ID field
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
        user_id: Option<String>, // Add user ID field
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
        user_id: Option<String>, // Add user ID field
    },
    FileOffer(FileOfferPayload),
    FileChunk(FileChunkPayload),
    FileAck(FileAckPayload),
    FileComplete(FileCompletePayload),
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileOfferPayload {
    pub transfer_id: String,
    pub file_name: String,
    pub file_size: u64,
    #[serde(default)]
    pub mime_type: Option<String>,
    pub chunk_size: u64,
    #[serde(default)]
    pub checksum: Option<String>,
    #[serde(default)]
    pub resume_offset: Option<u64>,
    #[serde(default)]
    pub sender_user_id: Option<String>,
    #[serde(default)]
    pub sender_address: Option<String>,
    #[serde(default)]
    pub sender_name: Option<String>,
    #[serde(default)]
    pub request_save_path: Option<bool>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileChunkPayload {
    pub transfer_id: String,
    pub offset: u64,
    pub data: String,
    #[serde(default)]
    pub final_chunk: bool,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileAckPayload {
    pub transfer_id: String,
    pub received_offset: u64,
    #[serde(default)]
    pub request_offset: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct FileCompletePayload {
    pub transfer_id: String,
    #[serde(default)]
    pub checksum_valid: Option<bool>,
}
