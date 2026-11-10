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

/// Request-only payloads keep optional session envelopes nonrecursive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum AccountSessionRequest {
    History {
        version: u16,
        account_id: String,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
        wanted: DeviceSyncPolicy,
    },
    Attachment {
        version: u16,
        account_id: String,
        file_conv: [u8; 32],
    },
    Wake {
        account_id: String,
        epoch: [u8; 16],
        revision: u64,
    },
    Documents {
        body: Vec<u8>,
    },
    AttachmentChecked {
        account_id: String,
        file_conv: [u8; 32],
    },
    FileMessageIdentities {
        account_id: String,
        requests: Vec<([u8; 32], [u8; 32])>,
    },
}

impl AccountSessionRequest {
    pub(super) fn from_frame(frame: &AccountHistoryFrame) -> Option<Self> {
        match frame {
            AccountHistoryFrame::FileMessageIdentitiesRequest {
                account_id,
                requests,
            } => Some(Self::FileMessageIdentities {
                account_id: account_id.clone(),
                requests: requests.clone(),
            }),
            AccountHistoryFrame::AttachmentRequestChecked {
                account_id,
                file_conv,
            } => Some(Self::AttachmentChecked {
                account_id: account_id.clone(),
                file_conv: *file_conv,
            }),
            AccountHistoryFrame::Request {
                version,
                account_id,
                cursor,
                known_digest,
                wanted,
            } => Some(Self::History {
                version: *version,
                account_id: account_id.clone(),
                cursor: *cursor,
                known_digest: *known_digest,
                wanted: *wanted,
            }),
            AccountHistoryFrame::AttachmentRequest {
                version,
                account_id,
                file_conv,
            } => Some(Self::Attachment {
                version: *version,
                account_id: account_id.clone(),
                file_conv: *file_conv,
            }),
            AccountHistoryFrame::Wake {
                account_id,
                epoch,
                revision,
            } => Some(Self::Wake {
                account_id: account_id.clone(),
                epoch: *epoch,
                revision: *revision,
            }),
            AccountHistoryFrame::DocumentsRequest { body } => {
                Some(Self::Documents { body: body.clone() })
            }
            _ => None,
        }
    }

    pub(super) fn into_frame(self) -> AccountHistoryFrame {
        match self {
            Self::FileMessageIdentities {
                account_id,
                requests,
            } => AccountHistoryFrame::FileMessageIdentitiesRequest {
                account_id,
                requests,
            },
            Self::AttachmentChecked {
                account_id,
                file_conv,
            } => AccountHistoryFrame::AttachmentRequestChecked {
                account_id,
                file_conv,
            },
            Self::Documents { body } => AccountHistoryFrame::DocumentsRequest { body },
            Self::History {
                version,
                account_id,
                cursor,
                known_digest,
                wanted,
            } => AccountHistoryFrame::Request {
                version,
                account_id,
                cursor,
                known_digest,
                wanted,
            },
            Self::Attachment {
                version,
                account_id,
                file_conv,
            } => AccountHistoryFrame::AttachmentRequest {
                version,
                account_id,
                file_conv,
            },
            Self::Wake {
                account_id,
                epoch,
                revision,
            } => AccountHistoryFrame::Wake {
                account_id,
                epoch,
                revision,
            },
        }
    }
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
    /// Optional V2-only invalidation. Append tags preserve all existing encodings.
    Wake {
        account_id: String,
        epoch: [u8; 16],
        revision: u64,
    },
    WakeAck {
        epoch: [u8; 16],
        revision: u64,
    },
    /// Optional V2-only exchange boundary. Existing tags 0 through 13 are frozen.
    SessionRequest {
        request_id: [u8; 16],
        request: AccountSessionRequest,
    },
    SessionReady {
        request_id: [u8; 16],
    },
    // Append-only optional extension. Never emitted in a history exchange.
    FavoritesRequest {
        version: u16,
        account_id: String,
        after: Option<String>,
        known_digest: [u8; 32],
        attachments: bool,
    },
    FavoriteHeader {
        id: String,
        total_len: u32,
    },
    FavoriteChunk {
        bytes: Vec<u8>,
    },
    FavoritesComplete {
        next: Option<String>,
        digest: [u8; 32],
    },
    FavoriteAttachmentRequest {
        version: u16,
        account_id: String,
        file_conv: [u8; 32],
    },
    // Optional document protocol; existing tags and history encodings stay frozen.
    DocumentsRequest {
        body: Vec<u8>,
    },
    DocumentHeader {
        body: Vec<u8>,
    },
    DocumentChunk {
        bytes: Vec<u8>,
    },
    DocumentsComplete {
        body: Vec<u8>,
    },
    /// Optional checked attachment request. Legacy requests never receive these terminals.
    AttachmentRequestChecked {
        account_id: String,
        file_conv: [u8; 32],
    },
    AttachmentUnavailable {
        file_conv: [u8; 32],
        source_disabled: bool,
    },
    /// Optional metadata uses the existing exclusive sibling session owner.
    FileMessageIdentitiesRequest {
        account_id: String,
        requests: Vec<([u8; 32], [u8; 32])>,
    },
    FileMessageIdentitiesResponse {
        bundles: Vec<crate::eventlog::Event>,
    },
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
            Self::FileMessageIdentitiesRequest { .. }
            | Self::FileMessageIdentitiesResponse { .. }
            | Self::AttachmentRequestChecked { .. }
            | Self::AttachmentUnavailable { .. }
            | Self::DocumentsRequest { .. }
            | Self::DocumentHeader { .. }
            | Self::DocumentChunk { .. }
            | Self::DocumentsComplete { .. }
            | Self::FavoritesRequest { .. }
            | Self::FavoriteHeader { .. }
            | Self::FavoriteChunk { .. }
            | Self::FavoritesComplete { .. }
            | Self::FavoriteAttachmentRequest { .. }
            | Self::Wake { .. }
            | Self::WakeAck { .. }
            | Self::SessionRequest { .. }
            | Self::SessionReady { .. }
                if codec != AccountSyncVersion::V2 =>
            {
                return false;
            }
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
            Self::FileMessageIdentitiesRequest {
                account_id: id,
                requests,
            } => account_id(id) && requests.len() <= super::session::MAX_FILE_IDENTITIES,
            Self::FileMessageIdentitiesResponse { bundles } => {
                bundles.len() <= super::session::MAX_FILE_IDENTITIES
                    && bundles
                        .iter()
                        .all(|bundle| bundle.ciphertext.len() <= 16 * 1024)
            }
            Self::AttachmentRequestChecked { account_id: id, .. } => account_id(id),
            Self::AttachmentUnavailable { .. } => true,
            Self::DocumentsRequest { body } => {
                super::account_documents::wire::DocumentRequest::decode(body).is_some()
            }
            Self::DocumentHeader { body } => super::account_documents::wire::valid_header(body),
            Self::DocumentChunk { bytes } => !bytes.is_empty() && bytes.len() <= MAX_CHUNK,
            Self::DocumentsComplete { body } => {
                super::account_documents::wire::valid_complete(body)
            }
            Self::FavoritesRequest {
                version,
                account_id: id,
                after,
                ..
            } => {
                *version == 1
                    && account_id(id)
                    && after
                        .as_ref()
                        .is_none_or(|s| super::favorites::conversation(s).is_some())
            }
            Self::FavoriteHeader { id, total_len } => {
                super::favorites::conversation(id).is_some()
                    && *total_len > 0
                    && *total_len as usize <= super::favorites::MAX_FAVORITE_BYTES
            }
            Self::FavoriteChunk { bytes } => !bytes.is_empty() && bytes.len() <= MAX_CHUNK,
            Self::FavoritesComplete { next, .. } => next
                .as_ref()
                .is_none_or(|s| super::favorites::conversation(s).is_some()),
            Self::FavoriteAttachmentRequest {
                version,
                account_id: id,
                ..
            } => *version == 1 && account_id(id),
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
            Self::Wake {
                account_id: id,
                epoch,
                revision,
            } => account_id(id) && *epoch != [0; 16] && *revision > 0,
            Self::WakeAck { epoch, revision } => *epoch != [0; 16] && *revision > 0,
            Self::SessionRequest {
                request_id,
                request,
            } => {
                *request_id != [0; 16]
                    && request
                        .clone()
                        .into_frame()
                        .valid_for(AccountSyncVersion::V2)
            }
            Self::SessionReady { request_id } => *request_id != [0; 16],
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
    fn logical_file_identity_extension_preserves_existing_account_tags() {
        let request = AccountHistoryFrame::FileMessageIdentitiesRequest {
            account_id: "a".repeat(32),
            requests: vec![([1; 32], [2; 32])],
        };
        let response = AccountHistoryFrame::FileMessageIdentitiesResponse { bundles: vec![] };
        for (tag, frame) in [(27u32, request.clone()), (28u32, response)] {
            let bytes = frame.encode_version(AccountSyncVersion::V2).unwrap();
            assert_eq!(&bytes[6..10], &tag.to_le_bytes());
            assert_eq!(
                AccountHistoryFrame::decode_versioned(&bytes),
                Some((AccountSyncVersion::V2, frame.clone()))
            );
            assert!(frame.encode_version(AccountSyncVersion::V1).is_none());
        }
        let nested = AccountSessionRequest::from_frame(&request).unwrap();
        assert_eq!(
            &bincode::serialize(&nested).unwrap()[..4],
            &5u32.to_le_bytes()
        );
        assert_eq!(nested.into_frame(), request);
    }

    #[test]
    fn checked_attachment_extension_appends_tags_and_cannot_encode_legacy_terminals() {
        let request = AccountHistoryFrame::AttachmentRequestChecked {
            account_id: "a".repeat(32),
            file_conv: [8; 32],
        };
        let terminal = AccountHistoryFrame::AttachmentUnavailable {
            file_conv: [8; 32],
            source_disabled: false,
        };
        for (tag, frame) in [(25u32, request.clone()), (26u32, terminal)] {
            let encoded = frame.encode_version(AccountSyncVersion::V2).unwrap();
            assert_eq!(&encoded[6..10], &tag.to_le_bytes());
            assert_eq!(
                AccountHistoryFrame::decode_versioned(&encoded),
                Some((AccountSyncVersion::V2, frame.clone()))
            );
            assert!(frame.encode_version(AccountSyncVersion::V1).is_none());
            let mut legacy = encoded;
            legacy[..6].copy_from_slice(MAGIC);
            assert!(AccountHistoryFrame::decode_versioned(&legacy).is_none());
        }
        let nested = AccountSessionRequest::from_frame(&request).unwrap();
        assert_eq!(
            &bincode::serialize(&nested).unwrap()[..4],
            &4u32.to_le_bytes()
        );
        assert_eq!(nested.into_frame(), request);
    }

    #[test]
    fn session_extension_accepts_appended_v2_tags_and_rejects_v1_envelopes() {
        // Frozen nested tag 0 is History, carrying the existing Request payload.
        let mut request = b"MTAHS2".to_vec();
        request.extend_from_slice(&14u32.to_le_bytes());
        request.extend_from_slice(&[9; 16]);
        request.extend_from_slice(&0u32.to_le_bytes());
        request.extend_from_slice(&2u16.to_le_bytes());
        request.extend_from_slice(&32u64.to_le_bytes());
        request.extend_from_slice(&[b'a'; 32]);
        request.push(0);
        request.extend_from_slice(&[0; 32]);
        request.extend_from_slice(&[1, 0, 1, 0, 0]);
        let mut ready = b"MTAHS2".to_vec();
        ready.extend_from_slice(&15u32.to_le_bytes());
        ready.extend_from_slice(&[9; 16]);
        for bytes in [request, ready] {
            let (version, frame) = AccountHistoryFrame::decode_versioned(&bytes)
                .expect("optional session frames must decode in V2");
            assert_eq!(version, AccountSyncVersion::V2);
            assert_eq!(frame.encode_version(version), Some(bytes.clone()));
            assert!(frame.encode_version(AccountSyncVersion::V1).is_none());
            let mut legacy = bytes;
            legacy[..6].copy_from_slice(MAGIC);
            assert!(AccountHistoryFrame::decode_versioned(&legacy).is_none());
        }
    }

    #[test]
    fn session_extension_rejects_zero_request_ids_and_invalid_nested_requests() {
        let mut ready = b"MTAHS2".to_vec();
        ready.extend_from_slice(&15u32.to_le_bytes());
        ready.extend_from_slice(&[0; 16]);
        assert!(AccountHistoryFrame::decode_versioned(&ready).is_none());
        for (version, id, cursor, policy) in [
            (1u16, [b'a'; 32], None, [1, 0, 1, 0, 0]),
            (2, [b'A'; 32], None, [1, 0, 1, 0, 0]),
            (2, [b'a'; 32], Some(3), [1, 0, 1, 0, 0]),
            (2, [b'a'; 32], Some(0), [0, 0, 1, 0, 0]),
        ] {
            let mut request = b"MTAHS2".to_vec();
            request.extend_from_slice(&14u32.to_le_bytes());
            request.extend_from_slice(&[9; 16]);
            request.extend_from_slice(&0u32.to_le_bytes());
            request.extend_from_slice(&version.to_le_bytes());
            request.extend_from_slice(&32u64.to_le_bytes());
            request.extend_from_slice(&id);
            if let Some(tag) = cursor {
                request.extend_from_slice(&[1, tag]);
                request.extend_from_slice(&[4; 32]);
            } else {
                request.push(0);
            }
            request.extend_from_slice(&[0; 32]);
            request.extend_from_slice(&policy);
            assert!(AccountHistoryFrame::decode_versioned(&request).is_none());
        }
    }

    #[test]
    fn session_request_conversions_preserve_payloads_and_validate_nested_frames() {
        for frame in [
            AccountHistoryFrame::Request {
                version: 2,
                account_id: "ab".repeat(16),
                cursor: Some(HistoryCursor {
                    tag: 2,
                    id: [6; 32],
                }),
                known_digest: [4; 32],
                wanted: DeviceSyncPolicy::default(),
            },
            AccountHistoryFrame::AttachmentRequest {
                version: 2,
                account_id: "ab".repeat(16),
                file_conv: [3; 32],
            },
            AccountHistoryFrame::Wake {
                account_id: "ab".repeat(16),
                epoch: [7; 16],
                revision: 5,
            },
        ] {
            let request = AccountSessionRequest::from_frame(&frame).unwrap();
            assert_eq!(request.clone().into_frame(), frame);
            let envelope = AccountHistoryFrame::SessionRequest {
                request_id: [8; 16],
                request: request.clone(),
            };
            let encoded = envelope.encode_version(AccountSyncVersion::V2).unwrap();
            assert_eq!(AccountHistoryFrame::decode(&encoded), Some(envelope));
            let zero_id = AccountHistoryFrame::SessionRequest {
                request_id: [0; 16],
                request,
            };
            assert!(zero_id.encode_version(AccountSyncVersion::V2).is_none());
            let unchecked = super::super::wire::frame(b"MTAHS2", &zero_id);
            assert!(AccountHistoryFrame::decode(&unchecked).is_none());
        }
        let ready = AccountHistoryFrame::SessionReady {
            request_id: [8; 16],
        };
        assert!(AccountSessionRequest::from_frame(&ready).is_none());
        assert!(
            AccountSessionRequest::from_frame(&AccountHistoryFrame::AttachmentComplete).is_none()
        );
        for request in [
            AccountSessionRequest::Attachment {
                version: 1,
                account_id: "ab".repeat(16),
                file_conv: [3; 32],
            },
            AccountSessionRequest::Attachment {
                version: 2,
                account_id: "bad".into(),
                file_conv: [3; 32],
            },
            AccountSessionRequest::Wake {
                account_id: "ab".repeat(16),
                epoch: [0; 16],
                revision: 5,
            },
            AccountSessionRequest::Wake {
                account_id: "ab".repeat(16),
                epoch: [7; 16],
                revision: 0,
            },
        ] {
            let envelope = AccountHistoryFrame::SessionRequest {
                request_id: [8; 16],
                request,
            };
            assert!(envelope.encode_version(AccountSyncVersion::V2).is_none());
            let unchecked = super::super::wire::frame(b"MTAHS2", &envelope);
            assert!(AccountHistoryFrame::decode(&unchecked).is_none());
        }
    }

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
        for (tag, frame) in [
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
        ]
        .into_iter()
        .enumerate()
        {
            let encoded = frame.encode().expect("valid frame encodes");
            assert!(encoded.starts_with(MAGIC));
            assert_eq!(&encoded[6..10], &(tag as u32).to_le_bytes());
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
    fn optional_wake_appends_v2_tags_without_changing_frozen_legacy_frames() {
        let wake = AccountHistoryFrame::Wake {
            account_id: "ab".repeat(16),
            epoch: [7; 16],
            revision: 3,
        };
        assert!(wake.encode_version(AccountSyncVersion::V1).is_none());
        let encoded = wake.encode_version(AccountSyncVersion::V2).unwrap();
        let mut wake_fixture = b"MTAHS2".to_vec();
        wake_fixture.extend_from_slice(&12u32.to_le_bytes());
        wake_fixture.extend_from_slice(&32u64.to_le_bytes());
        wake_fixture.extend_from_slice("ab".repeat(16).as_bytes());
        wake_fixture.extend_from_slice(&[7; 16]);
        wake_fixture.extend_from_slice(&3u64.to_le_bytes());
        assert_eq!(encoded, wake_fixture);
        let ack = AccountHistoryFrame::WakeAck {
            epoch: [7; 16],
            revision: 3,
        };
        let mut ack_fixture = b"MTAHS2".to_vec();
        ack_fixture.extend_from_slice(&13u32.to_le_bytes());
        ack_fixture.extend_from_slice(&[7; 16]);
        ack_fixture.extend_from_slice(&3u64.to_le_bytes());
        assert_eq!(
            ack.encode_version(AccountSyncVersion::V2),
            Some(ack_fixture)
        );
        assert_eq!(
            &encoded[6..10],
            &12u32.to_le_bytes(),
            "wake appends after frozen tags0..11"
        );
        assert_eq!(
            AccountHistoryFrame::decode_versioned(&encoded),
            Some((AccountSyncVersion::V2, wake.clone()))
        );
        // A V1 magic cannot smuggle an optional V2 request through the legacy codec.
        let mut legacy = encoded.clone();
        legacy[..6].copy_from_slice(MAGIC);
        assert!(AccountHistoryFrame::decode_versioned(&legacy).is_none());
        let complete = AccountHistoryFrame::Complete {
            next_cursor: None,
            digest: [9; 32],
        };
        assert_eq!(&complete.encode().unwrap()[6..10], &4u32.to_le_bytes());
        for invalid in [
            AccountHistoryFrame::Wake {
                account_id: "ab".repeat(16),
                epoch: [0; 16],
                revision: 3,
            },
            AccountHistoryFrame::Wake {
                account_id: "ab".repeat(16),
                epoch: [7; 16],
                revision: 0,
            },
        ] {
            assert!(invalid.encode_version(AccountSyncVersion::V2).is_none());
        }
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

#[cfg(test)]
mod favorites_wire_tests {
    use super::*;
    #[test]
    fn favorites_extension_is_explicitly_versioned_and_v1_incompatible() {
        let request = AccountHistoryFrame::FavoritesRequest {
            version: 1,
            account_id: "ab".repeat(16),
            after: None,
            known_digest: [0; 32],
            attachments: false,
        };
        assert!(request.encode_version(AccountSyncVersion::V1).is_none());
        let bytes = request.encode_version(AccountSyncVersion::V2).unwrap();
        assert_eq!(
            AccountHistoryFrame::decode_versioned(&bytes),
            Some((AccountSyncVersion::V2, request))
        );
    }
}
