//! Versioned, bounded control frames for account-history reconciliation.

use super::account_devices::AccountDevice;
use bincode::Options;
use serde::{Deserialize, Serialize};

const MAGIC: &[u8] = b"MTAHS1";
pub(super) const VERSION: u16 = 1;
pub(super) const MAX_CHUNK: usize = 16 * 1024;
pub(super) const MAX_RECORD: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(super) enum AccountHistoryFrame {
    Request {
        version: u16,
        account_id: String,
        cursor: u32,
        known_digest: [u8; 32],
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
        next_cursor: Option<u32>,
        digest: [u8; 32],
    },
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
                ..
            } => *version == VERSION && account_id(id),
            Self::Device { device } => {
                device.certificate.verify()
                    && device.certificate.device_ed25519_pub == device.public.ed25519_pub
            }
            Self::RecordHeader {
                from, total_len, ..
            } => account_id(from) && *total_len > 0 && (*total_len as usize) <= MAX_RECORD,
            Self::RecordChunk { bytes } => !bytes.is_empty() && bytes.len() <= MAX_CHUNK,
            Self::Complete { .. } => true,
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
                cursor: 7,
                known_digest: [0; 32],
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
                next_cursor: Some(8),
                digest: [9; 32],
            },
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
                cursor: 0,
                known_digest: [0; 32],
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
}
