use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::time::{SystemTime, UNIX_EPOCH};

/// User entity representing a Mesh-Talk user
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct User {
    /// Unique identifier for the user
    pub user_id: String,
    /// User's display name
    pub name: String,
    /// User's unique identifier/address
    pub address: String,
    /// Timestamp when the user was created
    pub created_at: u64,
    /// Timestamp when the user was last seen online
    pub last_seen: u64,
    /// Whether the user is currently online
    pub is_online: bool,
}

impl User {
    /// Creates a new User instance with the provided id, name, and address
    pub fn new(user_id: String, name: String, address: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();

        Self {
            user_id,
            name,
            address,
            created_at: now,
            last_seen: now,
            is_online: false,
        }
    }

    /// Updates the last seen timestamp to the current time and marks user as online
    pub fn update_last_seen(&mut self) {
        self.last_seen = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        self.is_online = true;
    }
}

use uuid::Uuid;
/// Contact entity representing a user's contact
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Contact {
    /// Unique identifier for the contact
    pub id: String,
    /// ID of the user who owns this contact
    pub user_id: String,
    /// Contact's name (may be different from their display name)
    pub name: String,
    /// Contact's username (as reported by the remote peer or backend)
    pub username: String,
    /// Contact's address
    pub address: String,
    /// Whether this contact is currently online
    pub is_online: bool,
    /// Timestamp when the contact was added
    pub added_at: u64,
    /// Notes about this contact
    pub notes: Option<String>,
}

impl Contact {
    /// Creates a new Contact instance with the provided parameters
    pub fn new(user_id: String, name: String, username: String, address: String) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let id = Uuid::new_v4().to_string();
        Self::from_storage(id, user_id, name, username, address, false, now, None)
    }

    /// Creates a Contact instance from stored data
    pub fn from_storage(
        id: String,
        user_id: String,
        name: String,
        username: String,
        address: String,
        is_online: bool,
        added_at: u64,
        notes: Option<String>,
    ) -> Self {
        Self {
            id,
            user_id,
            name,
            username,
            address,
            is_online,
            added_at,
            notes,
        }
    }
}

/// Message status enum
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum MessageStatus {
    /// Message was sent but not yet delivered
    Sent,
    /// Message was delivered to the recipient
    Delivered,
    /// Message was read by the recipient
    Read,
    /// Message failed to send
    Failed,
}

impl MessageStatus {
    /// Convert MessageStatus to database integer representation
    pub fn to_db_status(&self) -> i32 {
        match self {
            MessageStatus::Sent => 0,
            MessageStatus::Delivered => 1,
            MessageStatus::Read => 2,
            MessageStatus::Failed => 3,
        }
    }

    /// Convert database integer representation to MessageStatus
    pub fn from_db_status(status: i32) -> MessageStatus {
        match status {
            0 => MessageStatus::Sent,
            1 => MessageStatus::Delivered,
            2 => MessageStatus::Read,
            3 => MessageStatus::Failed,
            _ => MessageStatus::Failed, // Default to Failed for unknown status
        }
    }
}

/// Message entity representing a chat message
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    /// Unique identifier for the message
    pub id: String,
    /// ID of the user who sent the message
    pub from_user_id: String,
    /// Address of the user who sent the message
    pub from_address: String,
    /// ID of the user who received the message (for direct messages)
    pub to_user_id: Option<String>,
    /// Address of the user who received the message (for direct messages)
    pub to_address: Option<String>,
    /// Content of the message
    pub content: String,
    /// Timestamp when the message was sent
    pub sent_at: u64,
    /// Timestamp when the message was delivered (if applicable)
    pub delivered_at: Option<u64>,
    /// Timestamp when the message was read (if applicable)
    pub read_at: Option<u64>,
    /// Status of the message
    pub status: MessageStatus,
    /// Username of the account that owns/persisted this message on disk
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_username: Option<String>,
}

impl ChatMessage {
    /// Creates a new ChatMessage instance with the provided parameters
    pub fn new(
        from_user_id: String,
        from_address: String,
        to_user_id: Option<String>,
        to_address: Option<String>,
        content: String,
    ) -> Self {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let id = Uuid::new_v4().to_string();
        Self {
            id,
            from_user_id,
            from_address,
            to_user_id,
            to_address,
            content,
            sent_at: now,
            delivered_at: None,
            read_at: None,
            status: MessageStatus::Sent,
            owner_username: None,
        }
    }

    /// Marks the message as delivered and updates the delivery timestamp
    pub fn mark_delivered(&mut self) {
        self.delivered_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        self.status = MessageStatus::Delivered;
    }

    /// Marks the message as read and updates the read timestamp
    pub fn mark_read(&mut self) {
        self.read_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
        self.status = MessageStatus::Read;
    }

    /// Marks the message as failed
    pub fn mark_failed(&mut self) {
        self.status = MessageStatus::Failed;
    }
}

/// Peer information for network connections
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerInfo {
    /// Peer's address
    pub addr: SocketAddr,
    /// Peer's advertised node name
    pub node_name: String,
    /// Peer's advertised username (if provided)
    pub username: Option<String>,
    /// Peer's advertised TCP listening port
    pub listen_port: Option<u16>,
    /// Whether this peer is currently connected
    pub is_connected: bool,
    /// Timestamp of last connection
    pub last_connected: Option<u64>,
    /// Timestamp of last heartbeat received
    pub last_heartbeat: Option<u64>,
    /// User ID of the peer (if provided in discovery)
    pub user_id: Option<String>,
    /// IP address of the peer (provided in discovery/heartbeat messages)
    pub ip: Option<String>,
}

impl PeerInfo {
    /// Creates a new PeerInfo instance with the provided parameters
    pub fn new(
        addr: SocketAddr,
        node_name: String,
        username: Option<String>,
        listen_port: Option<u16>,
    ) -> Self {
        Self {
            addr,
            node_name,
            username,
            listen_port,
            is_connected: false,
            last_connected: None,
            last_heartbeat: None,
            user_id: None,
            ip: None,
        }
    }

    /// Marks the peer as connected and updates the connection timestamp
    pub fn mark_connected(&mut self) {
        self.is_connected = true;
        self.last_connected = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
    }

    /// Marks the peer as disconnected
    pub fn mark_disconnected(&mut self) {
        self.is_connected = false;
    }

    /// Updates the heartbeat timestamp to the current time
    pub fn update_heartbeat(&mut self) {
        self.last_heartbeat = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
        );
    }

    /// Updates the peer's metadata (name, username, listen port)
    pub fn update_metadata(
        &mut self,
        node_name: String,
        username: Option<String>,
        listen_port: Option<u16>,
        user_id: Option<String>,
    ) {
        self.node_name = node_name;
        self.username = username;
        self.listen_port = listen_port;
        if user_id.is_some() {
            self.user_id = user_id;
        }
    }

    /// Returns a formatted display label for the peer
    pub fn display_label(&self) -> String {
        let username = self
            .username
            .clone()
            .unwrap_or_else(|| "Unknown".to_string());
        let port = self.listen_port.unwrap_or(self.addr.port());
        format!(
            "{} • {} • {}:{}",
            self.node_name,
            username,
            self.addr.ip(),
            port
        )
    }
}
