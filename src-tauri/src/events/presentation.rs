//! Prepare native text on the existing bounded reception worker, never the UI thread.
use mesh_talk_core::node::{HostReception, Node, SyncCategory};

pub(super) struct Words {
    pub unknown: &'static str,
    pub group: &'static str,
    pub protected: &'static str,
    pub photo: &'static str,
    pub video: &'static str,
    pub file: &'static str,
    pub sticker: &'static str,
    pub message: &'static str,
}
pub(super) fn words(language: &str) -> Words {
    let values = match language {
        "zh-Hans" => [
            "未知联系人",
            "群聊",
            "[受保护消息]",
            "[图片]",
            "[视频]",
            "[文件]",
            "[表情]",
            "收到一条新消息",
        ],
        "zh-Hant" | "yue" => [
            "未知聯絡人",
            "群組",
            "[受保護訊息]",
            "[圖片]",
            "[影片]",
            "[檔案]",
            "[貼圖]",
            "收到一則新訊息",
        ],
        "ja" => [
            "不明な連絡先",
            "グループ",
            "[保護されたメッセージ]",
            "[写真]",
            "[動画]",
            "[ファイル]",
            "[スタンプ]",
            "新しいメッセージ",
        ],
        "ko" => [
            "알 수 없는 연락처",
            "그룹",
            "[보호된 메시지]",
            "[사진]",
            "[동영상]",
            "[파일]",
            "[스티커]",
            "새 메시지",
        ],
        "es" => [
            "Contacto desconocido",
            "Grupo",
            "[Mensaje protegido]",
            "[Foto]",
            "[Vídeo]",
            "[Archivo]",
            "[Sticker]",
            "Nuevo mensaje",
        ],
        "pt-BR" => [
            "Contato desconhecido",
            "Grupo",
            "[Mensagem protegida]",
            "[Foto]",
            "[Vídeo]",
            "[Arquivo]",
            "[Figurinha]",
            "Nova mensagem",
        ],
        _ => [
            "Unknown contact",
            "Group chat",
            "[Protected message]",
            "[Photo]",
            "[Video]",
            "[File]",
            "[Sticker]",
            "New message",
        ],
    };
    let [
        unknown,
        group,
        protected,
        photo,
        video,
        file,
        sticker,
        message,
    ] = values;
    Words {
        unknown,
        group,
        protected,
        photo,
        video,
        file,
        sticker,
        message,
    }
}

pub(crate) struct NotificationPresentation {
    pub title: String,
    pub body: String,
    pub summary_label: String,
}
impl NotificationPresentation {
    pub fn summary(&self, count: usize) -> String {
        format!("{} ({count})", self.summary_label)
    }
}

pub(super) fn display_name(value: &str, fallback: &str) -> String {
    let value = clean(value);
    if value.is_empty()
        || (matches!(value.len(), 32 | 64) && value.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        fallback.into()
    } else {
        value.chars().take(80).collect()
    }
}

fn clean(value: &str) -> String {
    value
        .chars()
        .filter(|c| {
            (!c.is_control() || c.is_whitespace())
                && !matches!(c, '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}')
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

pub(crate) fn prepare_notification<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    node: &Node,
    receipt: &HostReception,
    aliases: &std::collections::HashMap<String, String>,
) -> NotificationPresentation {
    let language = node
        .account_preferences(SyncCategory::Appearance)
        .into_iter()
        .find(|p| p.key == "language")
        .and_then(|p| p.value)
        .unwrap_or_default();
    use tauri::Manager;
    let language = app
        .try_state::<super::NotificationContextState>()
        .and_then(|state| {
            state
                .0
                .lock()
                .ok()
                .and_then(|context| context.language.clone())
        })
        .unwrap_or(language);
    let labels = words(&language);
    let (unknown, group, protected) = (labels.unknown, labels.group, labels.protected);
    let (device, account, supplied, channel) = match receipt {
        HostReception::DirectMessage(dm) => (
            dm.from.as_str(),
            dm.from_account.as_deref(),
            dm.from_name.as_str(),
            None,
        ),
        HostReception::ChannelMessage(msg) => (
            msg.from.as_str(),
            None,
            "",
            Some((msg.channel_id, msg.channel_name.as_str())),
        ),
        HostReception::File(file) => (
            file.from.as_str(),
            None,
            "",
            file.is_channel.then_some((file.conv, "")),
        ),
    };
    let (known_account, known_name) = node.peer_display_identity(device);
    let account = account.or(known_account.as_deref());
    let alias = account.and_then(|account| aliases.get(account));
    let profile = account.and_then(|account| node.account_display_name(account).ok().flatten());
    let sender = display_name(
        alias
            .map(String::as_str)
            .or(profile.as_deref())
            .or(known_name.as_deref())
            .unwrap_or(supplied),
        unknown,
    );
    let body = match receipt {
        HostReception::DirectMessage(dm) => {
            if dm.protection.is_some() {
                protected.into()
            } else {
                clean(&String::from_utf8_lossy(&dm.text))
            }
        }
        HostReception::ChannelMessage(msg) => {
            if msg.protection.is_some() {
                protected.into()
            } else {
                clean(&String::from_utf8_lossy(&msg.text))
            }
        }
        HostReception::File(file) => attachment(
            &file.name,
            &file.mime,
            file.size,
            file.media,
            file.protection.is_some(),
            &labels,
        ),
    };
    let body = if body.is_empty() {
        labels.message.into()
    } else {
        body
    };
    if let Some((id, supplied)) = channel {
        let name = node.channel_display_name(id);
        NotificationPresentation {
            title: display_name(
                aliases
                    .get(&hex::encode(id.as_bytes()))
                    .map(String::as_str)
                    .or(name.as_deref())
                    .unwrap_or(supplied),
                group,
            ),
            body: super::preview(&format!("{sender}: {body}")),
            summary_label: labels.message.into(),
        }
    } else {
        NotificationPresentation {
            title: sender,
            body: super::preview(&body),
            summary_label: labels.message.into(),
        }
    }
}

pub(super) fn attachment(
    name: &str,
    mime: &str,
    size: u64,
    media: bool,
    protected: bool,
    labels: &Words,
) -> String {
    use mesh_talk_core::file::sticker;
    if protected {
        return labels.protected.into();
    }
    if sticker::valid_manifest(name, mime, size, media) {
        return labels.sticker.into();
    }
    if media && mime.starts_with("image/") {
        return labels.photo.into();
    }
    if media && mime.starts_with("video/") {
        return labels.video.into();
    }
    let label = labels.file;
    // Only show the basename; sender-controlled paths and control characters aren't UI.
    let name = clean(name.rsplit(['/', '\\']).next().unwrap_or_default());
    if name.is_empty() {
        label.into()
    } else {
        format!("{label} {name}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn notification_preparation_uses_aliases_group_names_and_safe_unknown_fallback() {
        use mesh_talk_core::{
            discovery::Roster,
            identity::{account::Account, keystore},
            node::ReceivedDm,
        };
        use std::sync::{Arc, Mutex};
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        let node = Node::open_with_account(
            keystore::load_or_create(&dir.path().join("identity.keystore"), "pw").unwrap(),
            Account::from_secret_bytes([19; 32]),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &dir.path().join("messages.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let app = tauri::test::mock_app();
        let aliases = std::collections::HashMap::from([("account".into(), "My colleague".into())]);
        let dm = HostReception::DirectMessage(ReceivedDm {
            received_via: None,
            message_id: None,
            from_account: Some("account".into()),
            from: "ab".repeat(16),
            from_name: "ab".repeat(16),
            text: b"Hello".to_vec(),
            protection: None,
            reply_to: None,
        });
        let preview = prepare_notification(app.handle(), &node, &dm, &aliases);
        assert_eq!(preview.title, "My colleague");
        assert_eq!(preview.body, "Hello");
        assert_eq!(
            prepare_notification(app.handle(), &node, &dm, &Default::default()).title,
            "Unknown contact"
        );
        let id = mesh_talk_core::eventlog::ConversationId::new([2; 32]);
        let receipt = HostReception::File(mesh_talk_core::node::ReceivedFile {
            received_via: None,
            protection: None,
            expires_at_ms: None,
            conv: id,
            is_channel: true,
            from: "ab".repeat(16),
            name: "photo.png".into(),
            size: 100,
            mime: "image/png".into(),
            file_conv: id,
            media: true,
            event_id: mesh_talk_core::eventlog::EventId::new([3; 32]),
            authored_at: 1,
            received_at: 2,
            arrival_kind: "live",
            own_device: false,
            batch_id: None,
            batch_complete: true,
            batch_size: 1,
        });
        let groups =
            std::collections::HashMap::from([(hex::encode(id.as_bytes()), "Design team".into())]);
        let preview = prepare_notification(app.handle(), &node, &receipt, &groups);
        assert_eq!(preview.title, "Design team");
        assert_eq!(preview.body, "Unknown contact: [Photo]");
        assert_eq!(
            prepare_notification(app.handle(), &node, &receipt, &Default::default()).title,
            "Group chat"
        );
    }

    #[test]
    fn notification_names_never_expose_raw_identifiers_or_control_characters() {
        assert_eq!(
            display_name(&"ab".repeat(16), "Unknown contact"),
            "Unknown contact"
        );
        assert_eq!(display_name(&"ab".repeat(32), "Group chat"), "Group chat");
        assert_eq!(display_name(" \nAlice\u{202e} ", "Unknown"), "Alice");
        assert_eq!(display_name("王小明", "Unknown"), "王小明");
        assert_eq!(clean("hello\nworld"), "hello world");
    }
    #[test]
    fn notification_attachments_preserve_type_intent_and_protection() {
        assert_eq!(
            attachment(
                "secret.png",
                "image/png",
                100,
                true,
                true,
                &words("zh-Hans")
            ),
            "[受保护消息]"
        );
        assert_eq!(
            attachment(
                "photo.png",
                "image/png",
                100,
                true,
                false,
                &words("zh-Hans")
            ),
            "[图片]"
        );
        assert_eq!(
            attachment("photo.png", "image/png", 100, false, false, &words("en")),
            "[File] photo.png"
        );
        assert_eq!(
            attachment(
                "../../report.pdf",
                "application/pdf",
                100,
                false,
                false,
                &words("zh-Hans")
            ),
            "[文件] report.pdf"
        );
        assert_eq!(
            attachment("movie.mp4", "video/mp4", 100, true, false, &words("en")),
            "[Video]"
        );
        assert_eq!(
            attachment(
                "mt-sticker-v1-YnVpbHRpbjp3YXZlCvCfkYs.png",
                "image/png",
                100,
                true,
                false,
                &words("zh-Hans")
            ),
            "[表情]"
        );
    }
}
