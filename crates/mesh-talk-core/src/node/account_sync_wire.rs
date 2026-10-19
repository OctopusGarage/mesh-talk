//! Versioned, bounded control frames for account-history reconciliation.

use super::account_devices::AccountDevice;
use super::account_preferences::AccountPreference;
use super::account_sync_policy::DeviceSyncPolicy;
use bincode::Options;
use serde::{Deserialize, Serialize};

const MAGIC: &[u8] = b"MTAHS1";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum AccountSyncVersion {
    V1,
    V2,
}

impl AccountSyncVersion {
    pub(super) fn number(self) -> u16 {
        match self {
            Self::V1 => 1,
            Self::V2 => 2,
        }
    }
    fn magic(self) -> &'static [u8] {
        match self {
            Self::V1 => MAGIC,
            Self::V2 => b"MTAHS2",
        }
    }
    pub(super) fn includes_file_cards(self, history: bool, attachments: bool) -> bool {
        attachments || (self == Self::V2 && history)
    }
}
#[cfg(test)]
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
    #[cfg(test)]
    pub(super) fn encode(&self) -> Option<Vec<u8>> {
        self.encode_version(AccountSyncVersion::V1)
    }

    pub(super) fn encode_version(&self, version: AccountSyncVersion) -> Option<Vec<u8>> {
        if !self.valid_for(version) {
            return None;
        }
        let bytes = super::wire::frame(version.magic(), self);
        (bytes.len() <= super::wire::MAX_WIRE_FRAME).then_some(bytes)
    }

    #[cfg(test)]
    pub(super) fn decode(bytes: &[u8]) -> Option<Self> {
        Self::decode_versioned(bytes).map(|(_, frame)| frame)
    }

    pub(super) fn decode_versioned(bytes: &[u8]) -> Option<(AccountSyncVersion, Self)> {
        if bytes.len() > super::wire::MAX_WIRE_FRAME {
            return None;
        }
        let (version, body) = if let Some(body) = bytes.strip_prefix(MAGIC) {
            (AccountSyncVersion::V1, body)
        } else {
            (AccountSyncVersion::V2, bytes.strip_prefix(b"MTAHS2")?)
        };
        let frame: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(super::wire::MAX_WIRE_FRAME as u64)
            .reject_trailing_bytes()
            .deserialize(body)
            .ok()?;
        frame.valid_for(version).then_some((version, frame))
    }

    fn valid_for(&self, codec: AccountSyncVersion) -> bool {
        match self {
            Self::Request { version, .. } | Self::AttachmentRequest { version, .. }
                if *version != codec.number() =>
            {
                return false;
            }
            _ => {}
        }
        self.valid()
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
                matches!(*version, 1 | 2)
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
            } => matches!(*version, 1 | 2) && account_id(id),
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
    fn versioned_codec_preserves_v1_and_binds_request_version_to_magic() {
        let v1 = AccountHistoryFrame::Request {
            version: 1,
            account_id: "a".repeat(32),
            cursor: None,
            known_digest: [0; 32],
            wanted: DeviceSyncPolicy::default(),
        };
        let legacy = v1.encode().unwrap();
        let mut fixture = b"MTAHS1".to_vec();
        fixture.extend_from_slice(&0u32.to_le_bytes());
        fixture.extend_from_slice(&1u16.to_le_bytes());
        fixture.extend_from_slice(&32u64.to_le_bytes());
        fixture.extend_from_slice(&[b'a'; 32]);
        fixture.push(0); // absent cursor
        fixture.extend_from_slice(&[0; 32]);
        fixture.extend_from_slice(&[1, 0, 1, 0, 0]); // frozen policy field order
        assert_eq!(legacy, fixture);
        let mut complete_fixture = b"MTAHS1".to_vec();
        complete_fixture.extend_from_slice(&4u32.to_le_bytes());
        complete_fixture.push(0);
        complete_fixture.extend_from_slice(&[7; 32]);
        assert_eq!(
            AccountHistoryFrame::Complete {
                next_cursor: None,
                digest: [7; 32]
            }
            .encode()
            .unwrap(),
            complete_fixture
        );
        assert_eq!(v1.encode_version(AccountSyncVersion::V1).unwrap(), legacy);
        assert_eq!(&legacy[..6], b"MTAHS1");
        let mut v2 = v1.clone();
        if let AccountHistoryFrame::Request { version, .. } = &mut v2 {
            *version = 2;
        }
        assert!(v2.encode_version(AccountSyncVersion::V1).is_none());
        let bytes = v2.encode_version(AccountSyncVersion::V2).unwrap();
        assert_eq!(&bytes[..6], b"MTAHS2");
        assert_eq!(
            AccountHistoryFrame::decode_versioned(&bytes),
            Some((AccountSyncVersion::V2, v2))
        );
        let mut mismatch = bytes;
        mismatch[..6].copy_from_slice(b"MTAHS1");
        assert!(AccountHistoryFrame::decode_versioned(&mismatch).is_none());
    }

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
