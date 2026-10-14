//! Versioned, bounded control frames for account-history reconciliation.

use super::account_devices::AccountDevice;
use super::account_preferences::AccountPreference;
use super::account_sync_policy::DeviceSyncPolicy;
use bincode::Options;
use serde::{Deserialize, Serialize};

const MAGIC: &[u8] = b"MTAHS1";
pub(super) const VERSION: u16 = 1;
pub(super) const MAX_CHUNK: usize = 16 * 1024;
pub(super) const MAX_RECORD: usize = 256 * 1024;

/// Exclusive, content-addressed position in the history archive.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub(super) struct HistoryCursor {
    pub tag: u8,
    pub id: [u8; 32],
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum AccountHistoryFrame {
    Request {
        version: u16,
        account_id: String,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
        wanted: DeviceSyncPolicy,
    },
    Device {
        device: AccountDevice,
    },
    RecordHeader {
        conversation: [u8; 32],
        from: String,
        wall_clock: u64,
        event_id: [u8; 32],
        total_len: u32,
    },
    RecordChunk {
        bytes: Vec<u8>,
    },
    Complete {
        next_cursor: Option<HistoryCursor>,
        digest: [u8; 32],
    },
    ChannelHeader {
        channel: [u8; 32],
        from: String,
        wall_clock: u64,
        event_id: [u8; 32],
        total_len: u32,
    },
    Preference {
        record: AccountPreference,
    },
    FileCardHeader {
        host: [u8; 32],
        peer_account: Option<String>,
        from: String,
        wall_clock: u64,
        event_id: [u8; 32],
        total_len: u32,
    },
    AttachmentRequest {
        version: u16,
        account_id: String,
        file_conv: [u8; 32],
    },
    AttachmentStart {
        file_conv: [u8; 32],
        name: String,
        size: u64,
        sha256: [u8; 32],
    },
    AttachmentChunk {
        bytes: Vec<u8>,
    },
    AttachmentComplete,
}

impl AccountHistoryFrame {
    pub(super) fn encode(&self) -> Option<Vec<u8>> {
        if !self.valid() {
            return None;
        }
        let bytes = super::wire::frame(MAGIC, self);
        (bytes.len() <= super::wire::MAX_WIRE_FRAME).then_some(bytes)
    }

    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > super::wire::MAX_WIRE_FRAME {
            return None;
        }
        let body = bytes.strip_prefix(MAGIC)?;
        let frame: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(super::wire::MAX_WIRE_FRAME as u64)
            .reject_trailing_bytes()
            .deserialize(body)
            .ok()?;
        frame.valid().then_some(frame)
    }

    fn valid(&self) -> bool {
        fn account_id(id: &str) -> bool {
            id.len() == 32
                && id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        }
        match self {
            Self::Request {
                version,
                account_id: id,
                cursor,
                wanted,
                ..
            } => {
                *version == VERSION
                    && account_id(id)
                    && cursor.is_none_or(|position| position.tag <= 2)
                    && (wanted.history || wanted.attachments || cursor.is_none())
            }
            Self::Device { device } => {
                device.certificate.verify()
                    && device.certificate.device_ed25519_pub == device.public.ed25519_pub
            }
            Self::RecordHeader {
                from, total_len, ..
            } => account_id(from) && *total_len > 0 && (*total_len as usize) <= MAX_RECORD,
            Self::ChannelHeader {
                from, total_len, ..
            } => account_id(from) && *total_len > 0 && (*total_len as usize) <= MAX_RECORD,
            Self::Preference { record } => record.valid(),
            Self::FileCardHeader {
                peer_account,
                from,
                total_len,
                ..
            } => {
                account_id(from)
                    && peer_account.as_ref().is_none_or(|id| account_id(id))
                    && *total_len > 0
                    && (*total_len as usize) <= MAX_RECORD
            }
            Self::AttachmentRequest {
                version,
                account_id: id,
                ..
            } => *version == VERSION && account_id(id),
            Self::AttachmentStart { name, size, .. } => {
                !name.is_empty() && name.len() <= 255 && *size <= super::node::MAX_FILE_SIZE
            }
            Self::AttachmentChunk { bytes } => !bytes.is_empty() && bytes.len() <= MAX_CHUNK,
            Self::AttachmentComplete => true,
            Self::RecordChunk { bytes } => !bytes.is_empty() && bytes.len() <= MAX_CHUNK,
            Self::Complete { next_cursor, .. } => {
                next_cursor.is_none_or(|position| position.tag <= 2)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_frames_round_trip() {
        let account = crate::identity::account::Account::generate();
        let public = crate::identity::device::DeviceIdentity::generate().public();
        for frame in [
            AccountHistoryFrame::Request {
                version: VERSION,
                account_id: "ab".repeat(16),
                cursor: Some(HistoryCursor {
                    tag: 0,
                    id: [7; 32],
                }),
                known_digest: [0; 32],
                wanted: DeviceSyncPolicy::default(),
            },
            AccountHistoryFrame::Device {
                device: AccountDevice {
                    certificate: account.certify(&public.ed25519_pub),
                    public,
                },
            },
            AccountHistoryFrame::RecordHeader {
                conversation: [1; 32],
                from: "cd".repeat(16),
                wall_clock: 42,
                event_id: [2; 32],
                total_len: 512,
            },
            AccountHistoryFrame::RecordChunk {
                bytes: vec![3; MAX_CHUNK],
            },
            AccountHistoryFrame::Complete {
                next_cursor: Some(HistoryCursor {
                    tag: 1,
                    id: [8; 32],
                }),
                digest: [9; 32],
            },
            AccountHistoryFrame::ChannelHeader {
                channel: [1; 32],
                from: "ef".repeat(16),
                wall_clock: 42,
                event_id: [4; 32],
                total_len: 1024,
            },
            AccountHistoryFrame::Preference {
                record: AccountPreference {
                    category: super::super::account_sync_policy::SyncCategory::Appearance,
                    key: "theme".into(),
                    value: Some("dark".into()),
                    revision: 1,
                    device_id: "ab".repeat(16),
                },
            },
            AccountHistoryFrame::FileCardHeader {
                host: [1; 32],
                peer_account: Some("ab".repeat(16)),
                from: "cd".repeat(16),
                wall_clock: 42,
                event_id: [5; 32],
                total_len: 1024,
            },
            AccountHistoryFrame::AttachmentRequest {
                version: VERSION,
                account_id: "ab".repeat(16),
                file_conv: [6; 32],
            },
            AccountHistoryFrame::AttachmentStart {
                file_conv: [6; 32],
                name: "photo.png".into(),
                size: 5,
                sha256: [7; 32],
            },
            AccountHistoryFrame::AttachmentChunk {
                bytes: vec![8; MAX_CHUNK],
            },
            AccountHistoryFrame::AttachmentComplete,
        ] {
            let encoded = frame.encode().expect("valid frame encodes");
            assert!(encoded.starts_with(MAGIC));
            assert_eq!(AccountHistoryFrame::decode(&encoded), Some(frame));
        }
    }

    #[test]
    fn rejects_bad_version_and_oversized_payloads() {
        assert!(
            AccountHistoryFrame::Request {
                version: VERSION + 1,
                account_id: "ab".repeat(16),
                cursor: None,
                known_digest: [0; 32],
                wanted: DeviceSyncPolicy::default(),
            }
            .encode()
            .is_none()
        );
        assert!(
            AccountHistoryFrame::RecordHeader {
                conversation: [1; 32],
                from: "cd".repeat(16),
                wall_clock: 0,
                event_id: [2; 32],
                total_len: (MAX_RECORD + 1) as u32,
            }
            .encode()
            .is_none()
        );
        assert!(
            AccountHistoryFrame::RecordChunk {
                bytes: vec![0; MAX_CHUNK + 1],
            }
            .encode()
            .is_none()
        );
        assert!(
            AccountHistoryFrame::ChannelHeader {
                channel: [1; 32],
                from: "ef".repeat(16),
                wall_clock: 0,
                event_id: [3; 32],
                total_len: (MAX_RECORD + 1) as u32,
            }
            .encode()
            .is_none()
        );
    }

    #[test]
    fn rejects_trailing_bytes_and_wrong_magic() {
        let encoded = AccountHistoryFrame::Complete {
            next_cursor: None,
            digest: [0; 32],
        }
        .encode()
        .unwrap();
        let mut trailing = encoded.clone();
        trailing.push(0);
        assert!(AccountHistoryFrame::decode(&trailing).is_none());
        assert!(AccountHistoryFrame::decode(b"MTDE1").is_none());
    }

    #[test]
    fn decode_rejects_untrusted_oversized_record_headers_and_chunks() {
        let oversized_header = super::super::wire::frame(
            MAGIC,
            &AccountHistoryFrame::RecordHeader {
                conversation: [1; 32],
                from: "cd".repeat(16),
                wall_clock: 0,
                event_id: [2; 32],
                total_len: (MAX_RECORD + 1) as u32,
            },
        );
        assert!(AccountHistoryFrame::decode(&oversized_header).is_none());

        let oversized_chunk = super::super::wire::frame(
            MAGIC,
            &AccountHistoryFrame::RecordChunk {
                bytes: vec![0; MAX_CHUNK + 1],
            },
        );
        assert!(AccountHistoryFrame::decode(&oversized_chunk).is_none());
    }
}
