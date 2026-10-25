use serde::Serialize;

/// A peer as shown in the roster.
#[derive(Serialize)]
pub struct PeerInfo {
    pub user_id: String,
    pub name: String,
    pub addr: String,
    pub post_office: bool,
    /// True when this device has a directly discovered LAN route.
    pub discovered_on_lan: bool,
    /// A known direct application route, including routed/Tailscale discovery.
    /// Actual connectivity and pinned identity are rechecked during transport.
    pub directly_reachable: bool,
    /// The account this device belongs to (devices sharing it are one user's). The
    /// UI keys conversations by this so a multi-device contact is one conversation.
    pub account_id: Option<String>,
}

impl From<mesh_talk_core::discovery::PeerRecord> for PeerInfo {
    fn from(peer: mesh_talk_core::discovery::PeerRecord) -> Self {
        Self {
            discovered_on_lan: peer.has_lan_route(),
            directly_reachable: peer.has_direct_route(),
            user_id: peer.public.user_id(),
            name: peer.name,
            addr: peer.addr.to_string(),
            post_office: peer.post_office,
            account_id: peer.account_id,
        }
    }
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
    pub protection: Option<mesh_talk_core::node::MessageProtection>,
    pub expires_at_ms: Option<u64>,
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
            protection: h.protection,
            expires_at_ms: h.expires_at_ms,
            id,
            from_me: h.from_me,
            who: h.who,
            text: if h.protection.is_some() {
                String::new()
            } else {
                String::from_utf8_lossy(&h.text).into_owned()
            },
            wall_clock: h.wall_clock,
            reply_to: h.reply_to.map(|id| hex::encode(id.as_bytes())),
            file: h.file.map(|f| HistoryFileInfo {
                file_conv: hex::encode(f.file_conv.as_bytes()),
                name: if h.protection.is_some() {
                    "Protected image".into()
                } else {
                    f.name
                },
                size: f.size,
                mime: f.mime,
                media: f.media,
            }),
            recalled: h.recalled,
            recalled_text: if h.protection.is_some() {
                None
            } else {
                h.recalled_text
                    .map(|t| String::from_utf8_lossy(&t).into_owned())
            },
            sticker: h.sticker,
        }
    }
}

/// Aggregated reaction for display.
#[derive(Serialize)]
pub struct ReactionInfo {
    /// Certified current-account ownership for channels; legacy and account DMs omit it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from_me: Option<bool>,
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
    pub pending: bool,
    pub replica_enabled: bool,
    /// This exact local device only; remote members' network cost is private.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_sharing: Option<mesh_talk_core::node::GroupFileSharingStatus>,
}

/// Membership and management rights projected by the core authority owner.
/// Device provenance and account ownership remain distinct; UI presence cannot
/// establish a management permission.
#[derive(Serialize)]
pub struct ChannelMembersInfo {
    pub history_policy: mesh_talk_core::channel::HistoryPolicy,
    /// False when a descriptive reference has no source-confirmed policy.
    pub history_policy_known: bool,
    pub directory_reconciling: bool,
    pub owner: String,
    pub owner_account_id: Option<String>,
    pub can_manage: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account_access: Option<super::channels::AccountGroupAccessInfo>,
    pub members: Vec<ChannelMemberInfo>,
}

#[cfg(test)]
mod peer_route_tests {
    use super::*;
    use mesh_talk_core::{
        discovery::{Announce, Roster},
        identity::{account::Account, device::DeviceIdentity},
    };

    #[test]
    fn peer_info_preserves_direct_routed_and_gateway_provenance() {
        let device = DeviceIdentity::generate();
        let proof =
            Announce::new_post_office_with_account(&device, &Account::generate(), "Relay", 47475);
        let mut roster = Roster::default();
        roster.update_gateway(&proof, "192.168.1.2".parse().unwrap(), 47476, "self");
        roster.mark_authenticated(&device.public(), "192.168.1.2:47476".parse().unwrap());
        let gateway = PeerInfo::from(roster.get(&device.user_id()).unwrap().clone());
        assert!(!gateway.directly_reachable);
        assert!(!gateway.discovered_on_lan);
        roster.update_routed(&proof, "100.101.1.2".parse().unwrap(), "self");
        let routed = PeerInfo::from(roster.get(&device.user_id()).unwrap().clone());
        assert!(routed.directly_reachable);
        assert!(!routed.discovered_on_lan);
        let json = serde_json::to_value(&routed).unwrap();
        assert_eq!(json["directly_reachable"], true);
        assert_eq!(json["discovered_on_lan"], false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::channel::HistoryPolicy;
    #[test]
    fn membership_metadata_serializes_each_history_policy() {
        for (policy, expected) in [
            (HistoryPolicy::Off, "off"),
            (HistoryPolicy::LastTen, "last_ten"),
            (HistoryPolicy::LastThreeDays, "last_three_days"),
            (
                HistoryPolicy::LastTenWithinThreeDays,
                "last_ten_within_three_days",
            ),
        ] {
            let value = serde_json::to_value(ChannelMembersInfo {
                owner: "owner".into(),
                owner_account_id: None,
                can_manage: false,
                account_access: None,
                members: vec![],
                history_policy: policy,
                history_policy_known: true,
                directory_reconciling: false,
            })
            .unwrap();
            assert_eq!(value["history_policy"], expected);
        }
    }
}

#[cfg(test)]
mod protected_projection_tests {
    use super::*;

    #[test]
    fn protected_history_redacts_live_and_recalled_text_defensively() {
        let item = HistoryItem::from(mesh_talk_core::node::HistoryEntry {
            expires_at_ms: None,
            id: mesh_talk_core::eventlog::EventId::new([1; 32]),
            from_me: true,
            who: "me".into(),
            text: b"protected secret".to_vec(),
            protection: Some(mesh_talk_core::node::MessageProtection::Authenticate),
            wall_clock: 1,
            reply_to: None,
            file: Some(mesh_talk_core::node::FileHistoryInfo {
                file_conv: mesh_talk_core::eventlog::ConversationId::new([2; 32]),
                name: "sensitive-private-image.png".into(),
                size: 12,
                mime: "image/png".into(),
                media: true,
            }),
            recalled: true,
            recalled_text: Some(b"protected secret".to_vec()),
            sticker: None,
        });
        let json = serde_json::to_value(item).unwrap();
        assert_eq!(json["text"], "");
        assert!(json["recalled_text"].is_null());
        assert_eq!(json["protection"], "authenticate");
        assert!(!json.to_string().contains("protected secret"));
        assert_eq!(json["file"]["name"], "Protected image");
        assert!(!json.to_string().contains("sensitive-private-image.png"));
    }
}
