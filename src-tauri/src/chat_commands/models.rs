use serde::Serialize;

/// A peer as shown in the roster.
#[derive(Serialize)]
pub struct PeerInfo {
    pub user_id: String,
    pub name: String,
    pub addr: String,
    pub post_office: bool,
    /// The account this device belongs to (devices sharing it are one user's). The
    /// UI keys conversations by this so a multi-device contact is one conversation.
    pub account_id: Option<String>,
}

/// File metadata for a history line that represents a file/media message. Lets the UI
/// render inline media (image/video) or a file card, and read/save bytes by `file_conv`.
#[derive(Serialize)]
pub struct HistoryFileInfo {
    pub file_conv: String, // hex — pass to read_file/save_file
    pub name: String,
    pub size: u64,
    pub mime: String,
    pub media: bool, // inline media (media button) vs attachment (attach button), by intent
}

/// One merged history line (sent or received) for display.
#[derive(Serialize)]
pub struct HistoryItem {
    pub id: Option<String>, // hex EventId; null when there is no stable id (see From impl)
    pub from_me: bool,
    pub who: String,
    pub text: String,
    pub wall_clock: u64,
    pub reply_to: Option<String>, // hex EventId of the parent message, if any
    pub file: Option<HistoryFileInfo>, // present when this line is a file/media message
    pub recalled: bool,           // true when the message was recalled → render a placeholder
    pub recalled_text: Option<String>, // our own recalled text, for "re-edit" (None otherwise)
    pub sticker: Option<String>,  // animated-sticker id when this message is a sticker
}

impl From<mesh_talk_core::node::HistoryEntry> for HistoryItem {
    fn from(h: mesh_talk_core::node::HistoryEntry) -> Self {
        // A sent entry whose event isn't yet in the log gets the all-zero sentinel id; surface
        // it as null (like a pending message) so the UI never targets a react/reply at a
        // bogus id, instead of leaking the sentinel as a real hex id.
        let id = if h.id.as_bytes() == &[0u8; 32] {
            None
        } else {
            Some(hex::encode(h.id.as_bytes()))
        };
        HistoryItem {
            id,
            from_me: h.from_me,
            who: h.who,
            text: String::from_utf8_lossy(&h.text).into_owned(),
            wall_clock: h.wall_clock,
            reply_to: h.reply_to.map(|id| hex::encode(id.as_bytes())),
            file: h.file.map(|f| HistoryFileInfo {
                file_conv: hex::encode(f.file_conv.as_bytes()),
                name: f.name,
                size: f.size,
                mime: f.mime,
                media: f.media,
            }),
            recalled: h.recalled,
            recalled_text: h
                .recalled_text
                .map(|t| String::from_utf8_lossy(&t).into_owned()),
            sticker: h.sticker,
        }
    }
}

/// Aggregated reaction for display.
#[derive(Serialize)]
pub struct ReactionInfo {
    pub target: String, // hex EventId
    pub emoji: String,
    pub who: Vec<String>,
}

/// A channel member as shown in the chat UI.
#[derive(Serialize)]
pub struct ChannelMemberInfo {
    pub user_id: String,
    pub name: String,
    pub account_id: Option<String>,
}

/// A channel's membership plus its owner. The owner is the only principal allowed to
/// change membership (enforced in core); the UI uses it to show the owner badge and to
/// reveal the add/remove controls only to the owner.
#[derive(Serialize)]
pub struct ChannelMembersInfo {
    pub owner: String,
    pub members: Vec<ChannelMemberInfo>,
}
