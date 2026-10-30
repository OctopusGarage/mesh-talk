//! Current sending chains are selected by channel and epoch from the leased
//! profile database. Source 8 independently authenticates and publishes legacy
//! MTCSND snapshots before its small downgrade barrier becomes authoritative.
//! Standalone compatibility retains the legacy collecting owner. Production
//! Nodes load only the current epoch, preserving sender positions across restart.

use crate::eventlog::LogError;
use crate::eventlog::event::ConversationId;
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTCSND";

/// One channel's sending-key snapshot: the channel id (hex) and the serialized
/// per-epoch sending keys (`epoch -> SenderKey::serialize()` bytes).
#[derive(Debug, Clone, Serialize, Deserialize)]
struct SenderRecord {
    channel: String,
    senders: Vec<(u64, Vec<u8>)>,
}

/// Device-local sending keys. Production uses exact indexed epoch reads;
/// standalone callers retain the explicit legacy snapshot owner.
pub struct ChannelSenderStore {
    backend: Backend,
}
enum Backend {
    Legacy {
        file: EncryptedRecordLog<SenderRecord>,
        latest: HashMap<String, Vec<(u64, Vec<u8>)>>,
    },
    Indexed(super::transcript_repository::publication::PublishedHistorySource),
}
const SOURCE_KIND: i64 = super::transcript_repository::CHANNEL_SENDERS;
const SCHEMA: &str = "CREATE TABLE channel_sender_keys(
    channel BLOB NOT NULL CHECK(length(channel)=32), epoch BLOB NOT NULL CHECK(length(epoch)=8),
    checkpoint BLOB NOT NULL, PRIMARY KEY(channel,epoch)) WITHOUT ROWID;";
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}

impl ChannelSenderStore {
    pub(crate) fn authenticate_legacy(
        profile: &crate::storage::profile::context::ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        super::transcript_repository::publication::PublishedHistorySource::authenticate_legacy_with::<
            SenderRecord,
        >(profile, path, password, SOURCE_KIND, MAGIC, |record| {
            let _: [u8; 32] = hex::decode(&record.channel)
                .map_err(|_| invalid("invalid channel sender identity"))?
                .try_into()
                .map_err(|_| invalid("invalid channel sender identity"))?;
            for (_, bytes) in record.senders {
                if crate::channel::sender_key::SenderKey::deserialize(&bytes).is_none() {
                    return Err(invalid("invalid channel sender checkpoint"));
                }
            }
            Ok(())
        })
    }
    /// Standalone legacy compatibility. Normal Nodes use the published SQL source.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::<SenderRecord>::open(path, password, MAGIC)?;
        let mut latest = HashMap::new();
        for record in records {
            latest.insert(record.channel, record.senders);
        }
        Ok(Self {
            backend: Backend::Legacy { file, latest },
        })
    }

    pub(crate) fn from_profile(
        profile: std::sync::Arc<crate::storage::profile::context::ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='channel_sender_keys')",
                [],
                |row| row.get(0),
            )?;
            if !present {
                c.execute_batch(SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
        })?;
        let source = super::transcript_repository::publication::PublishedHistorySource::open(
            profile,
            path,
            password,
            SOURCE_KIND,
            |c, path, password| {
                EncryptedRecordLog::<SenderRecord>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |record| {
                        let channel: [u8; 32] = hex::decode(&record.channel)
                            .map_err(|_| invalid("invalid channel sender identity"))?
                            .try_into()
                            .map_err(|_| invalid("invalid channel sender identity"))?;
                        c.execute(
                            "DELETE FROM channel_sender_keys WHERE channel=?1",
                            [channel.as_slice()],
                        )?;
                        Self::put_on(c, &ConversationId::new(channel), &record.senders)
                    },
                )?;
                Ok(())
            },
        )?;
        Ok(Self {
            backend: Backend::Indexed(source),
        })
    }

    fn put_on(
        c: &rusqlite::Connection,
        channel: &ConversationId,
        senders: &[(u64, Vec<u8>)],
    ) -> Result<(), LogError> {
        let mut statement = c.prepare("INSERT INTO channel_sender_keys(channel,epoch,checkpoint) VALUES(?1,?2,?3) ON CONFLICT(channel,epoch) DO UPDATE SET checkpoint=excluded.checkpoint")?;
        for (epoch, bytes) in senders {
            if crate::channel::sender_key::SenderKey::deserialize(bytes).is_none() {
                return Err(invalid("invalid channel sender checkpoint"));
            }
            statement.execute(rusqlite::params![
                channel.as_bytes().as_slice(),
                epoch.to_be_bytes().as_slice(),
                bytes
            ])?;
        }
        Ok(())
    }

    /// Load only the active epoch. Historical epochs remain on disk and never
    /// become a startup key snapshot or an all-channel cache.
    pub(crate) fn current(
        &self,
        channel: &ConversationId,
        epoch: u64,
    ) -> Result<Option<Vec<u8>>, LogError> {
        use rusqlite::OptionalExtension;
        match &self.backend {
            Backend::Legacy { latest, .. } => Ok(latest
                .get(&hex::encode(channel.as_bytes()))
                .and_then(|senders| {
                    senders
                        .iter()
                        .find(|(e, _)| *e == epoch)
                        .map(|(_, bytes)| bytes.clone())
                })),
            Backend::Indexed(source) => source.profile().database().read(|c| {
                c.query_row(
                    "SELECT checkpoint FROM channel_sender_keys WHERE channel=?1 AND epoch=?2",
                    rusqlite::params![
                        channel.as_bytes().as_slice(),
                        epoch.to_be_bytes().as_slice()
                    ],
                    |r| r.get(0),
                )
                .optional()
                .map_err(Into::into)
            }),
        }
    }

    pub(crate) fn put_durable(
        &mut self,
        channel: &ConversationId,
        senders: &[(u64, Vec<u8>)],
    ) -> Result<(), LogError> {
        match &mut self.backend {
            Backend::Legacy { file, latest } => {
                let channel = hex::encode(channel.as_bytes());
                file.append_durable(&SenderRecord {
                    channel: channel.clone(),
                    senders: senders.to_vec(),
                })?;
                latest.insert(channel, senders.to_vec());
                Ok(())
            }
            Backend::Indexed(source) => {
                source.check_write()?;
                source
                    .profile()
                    .database()
                    .transaction(|c| Self::put_on(c, channel, senders))
            }
        }
    }

    #[cfg(test)]
    pub fn put(
        &mut self,
        channel: &ConversationId,
        senders: &[(u64, Vec<u8>)],
    ) -> Result<(), LogError> {
        self.put_durable(channel, senders)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::new_channel_id;

    #[test]
    fn put_then_get_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.senders");
        let mut store = ChannelSenderStore::open(&path, "pw").unwrap();
        let ch = new_channel_id();
        assert!(store.current(&ch, 0).unwrap().is_none());
        let senders = vec![(0u64, vec![1, 2, 3]), (1u64, vec![4, 5, 6])];
        store.put(&ch, &senders).unwrap();
        assert_eq!(store.current(&ch, 0).unwrap(), Some(senders[0].1.clone()));
        assert_eq!(store.current(&ch, 1).unwrap(), Some(senders[1].1.clone()));
    }

    #[test]
    fn reopen_restores_latest_put_wins() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.senders");
        let ch = new_channel_id();
        {
            let mut store = ChannelSenderStore::open(&path, "pw").unwrap();
            store.put(&ch, &[(0u64, vec![1, 1, 1])]).unwrap();
            // A later put for the same channel must win on reopen.
            store.put(&ch, &[(0u64, vec![9, 9, 9])]).unwrap();
        }
        let store2 = ChannelSenderStore::open(&path, "pw").unwrap();
        assert_eq!(store2.current(&ch, 0).unwrap(), Some(vec![9, 9, 9]));
    }
    #[test]
    fn published_sender_epochs_migrate_once_and_warm_open_uses_only_barrier() {
        use crate::{
            channel::sender_key::SenderKey,
            identity::{account::Account, device::DeviceIdentity},
            storage::profile::{ProfileStorageLease, context::ProfileStorage},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.senders");
        let channel = new_channel_id();
        let first = SenderKey::generate().serialize();
        let mut latest = SenderKey::generate();
        {
            let mut legacy = ChannelSenderStore::open(&path, "pw").unwrap();
            legacy
                .put_durable(&channel, &[(0, first.clone()), (7, latest.serialize())])
                .unwrap();
        }
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let open = || {
            ProfileStorage::open_participant(
                ProfileStorageLease::acquire(dir.path()).unwrap(),
                &identity.public(),
                &account.public(),
                &dir.path().join("messages.log"),
                &dir.path().join("sent.log"),
                "pw",
            )
            .unwrap()
        };
        let profile = open();
        ChannelSenderStore::authenticate_legacy(&profile, &path, "pw").unwrap();
        profile.complete_participant("pw", None).unwrap();
        let mut store = ChannelSenderStore::from_profile(profile.clone(), &path, "pw").unwrap();
        assert!(matches!(store.backend, Backend::Indexed(_)));
        assert_eq!(store.current(&channel, 0).unwrap(), Some(first.clone()));
        latest.try_ratchet().unwrap();
        let advanced = latest.serialize();
        store
            .put_durable(&channel, &[(7, advanced.clone())])
            .unwrap();
        assert_eq!(store.current(&channel, 0).unwrap(), Some(first));
        drop(store);
        drop(profile);
        crate::storage::record_log::frame_observations::reset();
        let profile = open();
        ChannelSenderStore::authenticate_legacy(&profile, &path, "pw").unwrap();
        profile.complete_participant("pw", None).unwrap();
        let store = ChannelSenderStore::from_profile(profile, &path, "pw").unwrap();
        assert_eq!(store.current(&channel, 7).unwrap(), Some(advanced));
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0
        );
        assert!(ChannelSenderStore::open(&path, "pw").is_err());
    }
    #[test]
    fn invalid_sender_record_fails_preflight_before_profile_publication() {
        use crate::{
            identity::{account::Account, device::DeviceIdentity},
            storage::profile::{ProfileStorageLease, context::ProfileStorage},
        };
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("channel.senders");
        {
            let mut legacy = ChannelSenderStore::open(&path, "pw").unwrap();
            legacy
                .put_durable(&new_channel_id(), &[(0, vec![1, 2, 3])])
                .unwrap();
        }
        let original = std::fs::read(&path).unwrap();
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(dir.path()).unwrap(),
            &identity.public(),
            &account.public(),
            &dir.path().join("messages.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        assert!(ChannelSenderStore::authenticate_legacy(&profile, &path, "pw").is_err());
        assert!(profile.participant_database().is_none());
        assert_eq!(std::fs::read(&path).unwrap(), original);
    }
}
