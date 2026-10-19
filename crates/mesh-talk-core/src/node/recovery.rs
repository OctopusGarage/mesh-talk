//! Offline export adapter. The desktop host must retire the runtime before
//! calling this method so all stores belong to one consistent snapshot.

use super::Node;
use crate::discovery::roster::Roster;
use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, EventId};
use crate::identity::{account::Account, account_keystore, device::DeviceIdentity, keystore};
use crate::recovery::snapshot::{
    SNAPSHOT_SCHEMA_VERSION, SnapshotError, SnapshotHeaderV1, SnapshotRecordV1, SnapshotWriter,
    read_snapshot,
};
use crate::recovery::{BackupError, BackupPreview, archive, key::RecoveryKey};
use crate::storage::record_log::EncryptedRecordLog;
use sha2::{Digest, Sha256};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

const MEDIA_CHUNK_BYTES: usize = 64 * 1024;
const RECOVERY_ORIGIN_MAGIC: &[u8; 6] = b"MTORIG";
const RECOVERY_CONTACT_MAGIC: &[u8; 6] = b"MTRACC";

struct MediaDigest(Sha256);

impl Write for MediaDigest {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.update(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct MediaRecordSink<'a, W: Write> {
    writer: &'a mut SnapshotWriter<W>,
    file_conv: ConversationId,
    buffer: Vec<u8>,
    index: u32,
}

impl<W: Write> MediaRecordSink<'_, W> {
    fn push_buffer(&mut self) -> Result<(), SnapshotError> {
        self.writer.push(&SnapshotRecordV1::MediaChunk {
            file_conv: *self.file_conv.as_bytes(),
            index: self.index,
            bytes: std::mem::take(&mut self.buffer),
        })?;
        self.index = self.index.checked_add(1).ok_or(SnapshotError::SizeLimit)?;
        Ok(())
    }

    fn finish(mut self) -> Result<(), SnapshotError> {
        if !self.buffer.is_empty() {
            self.push_buffer()?;
        }
        Ok(())
    }
}

impl<W: Write> Write for MediaRecordSink<'_, W> {
    fn write(&mut self, mut bytes: &[u8]) -> std::io::Result<usize> {
        let total = bytes.len();
        while !bytes.is_empty() {
            let available = MEDIA_CHUNK_BYTES - self.buffer.len();
            let take = available.min(bytes.len());
            self.buffer.extend_from_slice(&bytes[..take]);
            bytes = &bytes[take..];
            if self.buffer.len() == MEDIA_CHUNK_BYTES {
                self.push_buffer().map_err(std::io::Error::other)?;
            }
        }
        Ok(total)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

pub(in crate::node) fn load_recovery_contacts(
    path: &Path,
    password: &str,
) -> Result<Vec<String>, LogError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let (_, contacts) = EncryptedRecordLog::<String>::open(path, password, RECOVERY_CONTACT_MAGIC)?;
    if contacts.len() > 10_000
        || contacts.iter().collect::<HashSet<_>>().len() != contacts.len()
        || contacts
            .iter()
            .any(|id| id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()))
    {
        return Err(LogError::CorruptFile("invalid recovery contacts".into()));
    }
    Ok(contacts)
}

pub(in crate::node) fn load_recovery_origins(
    path: &Path,
    password: &str,
) -> Result<Vec<[u8; 32]>, LogError> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let (_, origins) = EncryptedRecordLog::<[u8; 32]>::open(path, password, RECOVERY_ORIGIN_MAGIC)?;
    if origins.is_empty()
        || origins.len() > 64
        || origins.iter().copied().collect::<HashSet<_>>().len() != origins.len()
    {
        return Err(LogError::CorruptFile("invalid recovery origin".into()));
    }
    Ok(origins)
}

fn save_recovery_origins(
    path: &Path,
    password: &str,
    sources: &[[u8; 32]],
) -> Result<(), LogError> {
    let (mut log, existing) =
        EncryptedRecordLog::<[u8; 32]>::open(path, password, RECOVERY_ORIGIN_MAGIC)?;
    if !existing.is_empty() {
        return Err(LogError::CorruptFile(
            "recovery origin already exists".into(),
        ));
    }
    for source in sources {
        log.append_durable(source)?;
    }
    Ok(())
}

impl Node {
    /// Open an existing profile without starting discovery, transport, or sync.
    /// The host must stop the live runtime and serialize profile operations first.
    pub fn open_recovery_profile(
        profile_dir: &Path,
        password: &str,
    ) -> Result<Arc<Self>, BackupError> {
        let identity = keystore::load(&profile_dir.join("identity.keystore"), password)
            .map_err(|_| SnapshotError::Io)?;
        let account = account_keystore::load(&profile_dir.join("account.keystore"), password)
            .map_err(|_| SnapshotError::Io)?;
        let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
        let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
        let (file_tx, _file_rx) = mpsc::unbounded_channel();
        Ok(Self::open_with_account(
            identity,
            account,
            Arc::new(Mutex::new(Roster::default())),
            dm_tx,
            channel_tx,
            file_tx,
            &profile_dir.join("messages.log"),
            &profile_dir.join("sent.log"),
            password,
        )
        .map_err(|_| SnapshotError::Io)?)
    }

    /// Export a stopped profile to a streaming encrypted archive. The desktop
    /// host owns the stop barrier and destination publication.
    pub fn write_encrypted_recovery_archive<W: Write>(
        &self,
        destination: W,
        key: &RecoveryKey,
        created_at_ms: u64,
    ) -> Result<archive::ArchiveInfo, BackupError> {
        let mut writer = archive::ArchiveWriter::new(destination, key.as_bytes())?;
        self.write_recovery_snapshot(&mut writer, created_at_ms)?;
        Ok(writer.finish()?)
    }

    /// Authenticate and validate the entire archive in private scratch space.
    /// The returned preview contains no account secret.
    pub fn inspect_encrypted_recovery_archive<R: Read>(
        source: R,
        key: &RecoveryKey,
        scratch_dir: &Path,
    ) -> Result<BackupPreview, BackupError> {
        let mut scratch = tempfile::Builder::new()
            .prefix("recovery-plaintext-")
            .tempfile_in(scratch_dir)?;
        archive::read_archive(source, scratch.as_file_mut(), key.as_bytes())?;
        scratch.as_file_mut().seek(SeekFrom::Start(0))?;
        preview_snapshot(scratch.as_file_mut()).map_err(Into::into)
    }

    /// Authenticate a portable archive before installing any of its data.
    /// `staging_dir` must be a fresh, unpublished profile path.
    pub fn install_encrypted_recovery_archive<R: Read>(
        source: R,
        key: &RecoveryKey,
        scratch_dir: &Path,
        staging_dir: &Path,
        new_password: &str,
    ) -> Result<(Arc<Self>, BackupPreview), BackupError> {
        let mut scratch = tempfile::Builder::new()
            .prefix("recovery-plaintext-")
            .tempfile_in(scratch_dir)?;
        archive::read_archive(source, scratch.as_file_mut(), key.as_bytes())?;
        scratch.as_file_mut().seek(SeekFrom::Start(0))?;
        let preview = preview_snapshot(scratch.as_file_mut())?;
        scratch.as_file_mut().seek(SeekFrom::Start(0))?;
        let node =
            Self::install_recovery_snapshot(scratch.as_file_mut(), staging_dir, new_password)?;
        Ok((node, preview))
    }

    /// Install the supported records into a fresh, unpublished account directory.
    /// The caller must first verify the outer encrypted archive and only publish
    /// this staging directory after the entire installation succeeds.
    pub(in crate::node) fn install_recovery_snapshot<R: Read + Seek>(
        mut source: R,
        staging_dir: &Path,
        new_password: &str,
    ) -> Result<Arc<Self>, SnapshotError> {
        let mut media_names = HashMap::new();
        let header = read_snapshot(&mut source, |record| {
            if let SnapshotRecordV1::FileCard { manifest, .. } = record {
                let card = crate::file::decode_manifest(&manifest)
                    .ok_or(SnapshotError::InvalidFileCard)?;
                media_names
                    .entry(*card.file_conv().as_bytes())
                    .or_insert_with(|| card.name().to_string());
            }
            Ok(())
        })?;
        source
            .seek(SeekFrom::Start(0))
            .map_err(|_| SnapshotError::Io)?;
        if staging_dir.exists() {
            return Err(SnapshotError::Conflict);
        }
        std::fs::create_dir(staging_dir).map_err(|_| SnapshotError::Io)?;
        let result = (|| {
            let identity = DeviceIdentity::generate();
            let account = Account::from_secret_bytes(header.account_secret);
            keystore::save(
                &staging_dir.join("identity.keystore"),
                new_password,
                &identity,
            )
            .map_err(|_| SnapshotError::Io)?;
            account_keystore::save(
                &staging_dir.join("account.keystore"),
                new_password,
                &account,
            )
            .map_err(|_| SnapshotError::Io)?;
            save_recovery_origins(
                &staging_dir.join("recovery-origin.log"),
                new_password,
                &header.source_device_ed25519_pubs,
            )
            .map_err(|_| SnapshotError::Io)?;
            let (mut contacts_log, existing_contacts) = EncryptedRecordLog::<String>::open(
                &staging_dir.join("recovery-contacts.log"),
                new_password,
                RECOVERY_CONTACT_MAGIC,
            )
            .map_err(|_| SnapshotError::Io)?;
            if !existing_contacts.is_empty() {
                return Err(SnapshotError::Conflict);
            }
            super::recovered_sent_ids::RecoveredSentIds::open(
                &staging_dir.join("recovery-sent-ids.log"),
                new_password,
            )
            .map_err(|_| SnapshotError::Io)?;
            let (dm_tx, _dm_rx) = mpsc::unbounded_channel();
            let (channel_tx, _channel_rx) = mpsc::unbounded_channel();
            let (file_tx, _file_rx) = mpsc::unbounded_channel();
            let node = Self::open_with_account(
                identity,
                account,
                Arc::new(Mutex::new(Roster::default())),
                dm_tx,
                channel_tx,
                file_tx,
                &staging_dir.join("messages.log"),
                &staging_dir.join("sent.log"),
                new_password,
            )
            .map_err(|_| SnapshotError::Io)?;
            let mut media_temp: HashMap<[u8; 32], (PathBuf, std::fs::File)> = HashMap::new();
            // Legacy/channel sent rows are keyed only by (conversation, seq).
            // Old and new devices may both start at sequence 1, so keep imported
            // rows in a disjoint local sequence range. Their original event IDs
            // are retained in recovered_sent_ids for history and later exports.
            let mut next_restored_legacy_seq = u64::MAX;
            let installed_header = read_snapshot(&mut source, |record| {
                match record {
                    SnapshotRecordV1::SentDm {
                        conversation,
                        message_id,
                        seq,
                        wall_clock,
                        plaintext,
                    } => {
                        let local_seq = if super::DmEnvelope::decode(&plaintext).is_none() {
                            let value = next_restored_legacy_seq;
                            next_restored_legacy_seq = next_restored_legacy_seq
                                .checked_sub(1)
                                .ok_or(SnapshotError::SizeLimit)?;
                            value
                        } else {
                            seq
                        };
                        let entry = super::sentlog::SentEntry {
                            conversation: ConversationId::new(conversation),
                            seq: local_seq,
                            wall_clock,
                            plaintext,
                        };
                        node.sentlog
                            .lock()
                            .expect("sentlog mutex not poisoned")
                            .record(
                                entry.conversation,
                                entry.seq,
                                entry.wall_clock,
                                &entry.plaintext,
                            )
                            .map_err(|_| SnapshotError::Io)?;
                        node.recovered_sent_ids
                            .lock()
                            .expect("recovered sent ids mutex not poisoned")
                            .as_mut()
                            .expect("recovery id store created before node open")
                            .record(&entry, EventId::new(message_id))
                            .map_err(|_| SnapshotError::Io)?;
                    }
                    SnapshotRecordV1::ReceivedDm {
                        conversation,
                        event_id,
                        from,
                        wall_clock,
                        plaintext,
                    } => node
                        .received
                        .lock()
                        .expect("received mutex not poisoned")
                        .record(
                            ConversationId::new(conversation),
                            from,
                            wall_clock,
                            &plaintext,
                            EventId::new(event_id),
                        )
                        .map_err(|_| SnapshotError::Io)?,
                    SnapshotRecordV1::Recall {
                        conversation,
                        target,
                        wall_clock,
                    } => node
                        .recalls
                        .lock()
                        .expect("recalls mutex not poisoned")
                        .record(
                            ConversationId::new(conversation),
                            EventId::new(target),
                            wall_clock,
                        )
                        .map_err(|_| SnapshotError::Io)?,
                    SnapshotRecordV1::DmLanguageProfile { payload } => {
                        let profile = super::profile::ProfilePayload::decode(&payload)
                            .ok_or(SnapshotError::InvalidEncoding)?;
                        node.account_profiles
                            .restore_language(&profile)
                            .map_err(|_| SnapshotError::Io)?;
                    }
                    SnapshotRecordV1::DmLanguageDecision { id, hidden } => {
                        node.account_profiles
                            .decide(EventId::new(id), hidden)
                            .map_err(|_| SnapshotError::Io)?;
                    }
                    SnapshotRecordV1::Profile {
                        account_id,
                        updated_at,
                        avatar,
                    } => {
                        node.account_profiles
                            .restore_avatar(&account_id, updated_at, avatar)
                            .map_err(|_| SnapshotError::Io)?;
                    }
                    SnapshotRecordV1::MediaDescriptor { file_conv, .. } => {
                        let path =
                            staging_dir.join(format!("recovery-media-{}", hex::encode(file_conv)));
                        let file = std::fs::File::create(&path).map_err(|_| SnapshotError::Io)?;
                        media_temp.insert(file_conv, (path, file));
                    }
                    SnapshotRecordV1::MediaChunk {
                        file_conv, bytes, ..
                    } => {
                        let (_, file) = media_temp
                            .get_mut(&file_conv)
                            .ok_or(SnapshotError::InvalidMedia)?;
                        file.write_all(&bytes).map_err(|_| SnapshotError::Io)?;
                    }
                    SnapshotRecordV1::SignedEvent { event } => {
                        node.log
                            .lock()
                            .expect("event log mutex not poisoned")
                            .append(event)
                            .map_err(|_| SnapshotError::InvalidEvent)?;
                    }
                    SnapshotRecordV1::FileCard {
                        conversation,
                        event_id,
                        from,
                        wall_clock,
                        manifest,
                    } => node
                        .received_files
                        .lock()
                        .expect("received_files mutex not poisoned")
                        .record(
                            ConversationId::new(conversation),
                            from,
                            wall_clock,
                            &manifest,
                            EventId::new(event_id),
                        )
                        .map_err(|_| SnapshotError::Io)?,
                    SnapshotRecordV1::AccountContact { account_id } => {
                        contacts_log
                            .append_durable(&account_id)
                            .map_err(|_| SnapshotError::Io)?;
                    }
                }
                Ok(())
            })?;
            if installed_header != header {
                return Err(SnapshotError::InvalidAccount);
            }
            for (file_conv, (path, file)) in media_temp {
                file.sync_all().map_err(|_| SnapshotError::Io)?;
                drop(file);
                let name = media_names
                    .get(&file_conv)
                    .map(String::as_str)
                    .unwrap_or("recovered.bin");
                node.media
                    .store_from_path(ConversationId::new(file_conv), name, &path)
                    .map_err(|_| SnapshotError::Io)?;
                std::fs::remove_file(path).map_err(|_| SnapshotError::Io)?;
            }
            // The node was opened before records were installed, so its cached
            // channel/file views are stale. Return a verified fresh view.
            drop(node);
            Self::open_recovery_profile(staging_dir, new_password).map_err(|_| SnapshotError::Io)
        })();
        if result.is_err() {
            let _ = std::fs::remove_dir_all(staging_dir);
        }
        result
    }

    /// Account IDs represented in durable message or file history, even when no
    /// device is currently discoverable.
    pub fn known_account_ids(&self) -> Vec<String> {
        let own = self.account.account_id();
        let mut ids: BTreeSet<String> = self.recovery_contacts.iter().cloned().collect();
        let mut observe = |plaintext: &[u8]| {
            let Some(envelope) = super::DmEnvelope::decode(plaintext) else {
                return;
            };
            let route = envelope.route;
            let other = if route.sender_account == own {
                route.recipient_account
            } else if route.recipient_account == own {
                route.sender_account
            } else {
                return;
            };
            if other != own && other.len() == 32 && other.bytes().all(|c| c.is_ascii_hexdigit()) {
                ids.insert(other);
            }
        };
        let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
        for conversation in sent.conversations() {
            if let Some(entry) = sent
                .entries_ref(&conversation)
                .iter()
                .find(|entry| super::DmEnvelope::decode(&entry.plaintext).is_some())
            {
                observe(&entry.plaintext);
            }
        }
        drop(sent);
        let received = self.received.lock().expect("received mutex not poisoned");
        for conversation in received.conversations() {
            if let Some(entry) = received
                .entries_ref(&conversation)
                .iter()
                .find(|entry| super::DmEnvelope::decode(&entry.plaintext).is_some())
            {
                observe(&entry.plaintext);
            }
        }
        drop(received);
        let peers = self.historical_dm_peers();
        let files = self
            .received_files
            .lock()
            .expect("received_files mutex not poisoned");
        for proof in peers {
            if let Some(account_id) = proof.account_id()
                && account_id != own
                && !files
                    .entries_ref(&super::conversation::account_conversation_id(
                        &own,
                        &account_id,
                    ))
                    .is_empty()
            {
                ids.insert(account_id);
            }
        }
        ids.into_iter().collect()
    }

    /// Write the currently supported local plaintext records as typed snapshot
    /// rows. The host calls this only after its runtime has fully stopped.
    pub(in crate::node) fn write_recovery_snapshot<W: Write>(
        &self,
        destination: W,
        created_at_ms: u64,
    ) -> Result<(), SnapshotError> {
        let own_account = self.account_id();
        let candidate_contacts = self.known_account_ids();
        let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
        let received = self.received.lock().expect("received mutex not poisoned");
        let log = self.log.lock().expect("event log mutex not poisoned");
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        let received_files = self
            .received_files
            .lock()
            .expect("received_files mutex not poisoned");
        let recovered_sent_ids = self
            .recovered_sent_ids
            .lock()
            .expect("recovered sent ids mutex not poisoned");
        let sent_conversations = sent.conversations();
        let received_conversations = received.conversations();
        let event_conversations = log.conversations();
        let file_conversations = received_files.conversations();
        let file_contacts: Vec<_> = candidate_contacts
            .into_iter()
            .filter(|account_id| {
                let conversation =
                    super::conversation::account_conversation_id(&own_account, account_id);
                received_files
                    .entries_ref(&conversation)
                    .iter()
                    .any(|entry| {
                        !delivery.file_erased(entry.conversation, entry.event_id)
                            && !delivery.manifest_event_erased(entry.event_id)
                    })
            })
            .collect();
        let sent_count: usize = sent_conversations
            .iter()
            .map(|conversation| sent.entries_ref(conversation).len())
            .sum();
        let received_count: usize = received_conversations
            .iter()
            .map(|conversation| received.entries_ref(conversation).len())
            .sum();
        let event_count: usize = event_conversations
            .iter()
            .map(|conversation| log.events(conversation).len())
            .sum();
        let file_count: usize = file_conversations
            .iter()
            .map(|conversation| {
                received_files
                    .entries_ref(conversation)
                    .iter()
                    .filter(|entry| {
                        !delivery.file_erased(entry.conversation, entry.event_id)
                            && !delivery.manifest_event_erased(entry.event_id)
                    })
                    .count()
            })
            .sum();
        let visible_media: HashSet<_> = file_conversations
            .iter()
            .flat_map(|conversation| received_files.entries_ref(conversation))
            .filter(|entry| {
                !delivery.file_erased(entry.conversation, entry.event_id)
                    && !delivery.manifest_event_erased(entry.event_id)
            })
            .filter_map(|entry| crate::file::decode_manifest(&entry.plaintext))
            .map(|manifest| manifest.file_conv())
            .collect();
        let recalls = self
            .recalls
            .lock()
            .expect("recalls mutex not poisoned")
            .snapshot_marks();
        let super::account_profile::ProfileSnapshot {
            avatars: profiles,
            language_profiles,
            language_decisions,
        } = self.account_profiles.snapshot();
        let mut record_count = u64::try_from(
            sent_count
                .checked_add(received_count)
                .and_then(|count| count.checked_add(recalls.len()))
                .and_then(|count| count.checked_add(profiles.len()))
                .and_then(|count| count.checked_add(language_profiles.len()))
                .and_then(|count| count.checked_add(language_decisions.len()))
                .and_then(|count| count.checked_add(event_count))
                .and_then(|count| count.checked_add(file_count))
                .and_then(|count| count.checked_add(file_contacts.len()))
                .ok_or(SnapshotError::SizeLimit)?,
        )
        .map_err(|_| SnapshotError::SizeLimit)?;
        let media_files = self.media.managed_files().map_err(|_| SnapshotError::Io)?;
        let mut media = Vec::with_capacity(media_files.len());
        for (file_conv, _) in media_files {
            if !visible_media.contains(&file_conv) {
                continue;
            }
            // MediaStore authenticates and decrypts each chunk before it reaches the
            // archive. The archive is encrypted separately and remains portable across
            // account passwords and media-store salts.
            let mut digest = MediaDigest(Sha256::new());
            let size = self
                .media
                .copy_decrypted(file_conv, &mut digest)
                .map_err(|_| SnapshotError::InvalidMedia)?;
            let chunks = size.div_ceil(MEDIA_CHUNK_BYTES as u64);
            record_count = record_count
                .checked_add(1)
                .and_then(|count| count.checked_add(chunks))
                .ok_or(SnapshotError::SizeLimit)?;
            media.push((file_conv, size, <[u8; 32]>::from(digest.0.finalize())));
        }
        let mut source_device_ed25519_pubs: Vec<_> = self
            .recovery_origins
            .iter()
            .map(|author| *author.ed25519_pub())
            .collect();
        let current_author = self.identity.public().ed25519_pub;
        if !source_device_ed25519_pubs.contains(&current_author) {
            source_device_ed25519_pubs.push(current_author);
        }
        let header = SnapshotHeaderV1 {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            account_secret: self.account.secret_bytes(),
            account_id: self.account.account_id(),
            source_device_ed25519_pubs,
            created_at_ms,
            record_count,
        };
        let mut writer = SnapshotWriter::new(destination, &header)?;
        for conversation in sent_conversations {
            let own_author =
                crate::eventlog::Author::from_ed25519(self.identity.public().ed25519_pub);
            let signed_ids: HashMap<_, _> = log
                .events(&conversation)
                .into_iter()
                .filter(|event| {
                    event.kind == crate::eventlog::EventKind::Message && event.author == own_author
                })
                .map(|event| (event.seq, event.id))
                .collect();
            for entry in sent.entries_ref(&conversation) {
                let message_id = super::DmEnvelope::decode(&entry.plaintext)
                    .map(|envelope| envelope.msg_id)
                    .or_else(|| {
                        recovered_sent_ids
                            .as_ref()
                            .and_then(|ids| ids.id_for(entry))
                            .map(|id| *id.as_bytes())
                    })
                    .or_else(|| signed_ids.get(&entry.seq).map(|id| *id.as_bytes()))
                    .unwrap_or_else(|| {
                        let mut hash = Sha256::new();
                        hash.update(b"mesh-talk-recovery-legacy-sent-v1");
                        hash.update(conversation.as_bytes());
                        hash.update(entry.seq.to_be_bytes());
                        hash.finalize().into()
                    });
                writer.push(&SnapshotRecordV1::SentDm {
                    conversation: *conversation.as_bytes(),
                    message_id,
                    seq: entry.seq,
                    wall_clock: entry.wall_clock,
                    plaintext: entry.plaintext.clone(),
                })?;
            }
        }
        let own_devices: std::collections::HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        for conversation in received_conversations {
            for entry in received.entries_ref(&conversation) {
                let envelope = super::DmEnvelope::decode(&entry.plaintext);
                let inner = envelope
                    .as_ref()
                    .map_or(entry.plaintext.as_slice(), |env| env.body.as_slice());
                if entry.from != self.account_id()
                    && !own_devices.contains(&entry.from)
                    && self.recipient_expired(inner, entry.event_id, entry.wall_clock)
                {
                    continue;
                }
                writer.push(&SnapshotRecordV1::ReceivedDm {
                    conversation: *conversation.as_bytes(),
                    event_id: *entry.event_id.as_bytes(),
                    from: entry.from.clone(),
                    wall_clock: entry.wall_clock,
                    plaintext: entry.plaintext.clone(),
                })?;
            }
        }
        for conversation in event_conversations {
            for event in log.events(&conversation) {
                writer.push(&SnapshotRecordV1::SignedEvent {
                    event: event.clone(),
                })?;
            }
        }
        for account_id in file_contacts {
            writer.push(&SnapshotRecordV1::AccountContact { account_id })?;
        }
        for conversation in file_conversations {
            for entry in received_files.entries_ref(&conversation) {
                if delivery.file_erased(entry.conversation, entry.event_id)
                    || delivery.manifest_event_erased(entry.event_id)
                    || (entry.from != self.account_id()
                        && !own_devices.contains(&entry.from)
                        && self.recipient_expired(
                            &entry.plaintext,
                            entry.event_id,
                            entry.wall_clock,
                        ))
                {
                    continue;
                }
                writer.push(&SnapshotRecordV1::FileCard {
                    conversation: *entry.conversation.as_bytes(),
                    event_id: *entry.event_id.as_bytes(),
                    from: entry.from.clone(),
                    wall_clock: entry.wall_clock,
                    manifest: entry.plaintext.clone(),
                })?;
            }
        }
        for mark in recalls {
            writer.push(&SnapshotRecordV1::Recall {
                conversation: *mark.conversation.as_bytes(),
                target: *mark.target.as_bytes(),
                wall_clock: mark.wall_clock,
            })?;
        }
        for (account_id, updated_at, avatar) in profiles {
            writer.push(&SnapshotRecordV1::Profile {
                account_id,
                updated_at,
                avatar,
            })?;
        }
        for profile in language_profiles {
            writer.push(&SnapshotRecordV1::DmLanguageProfile {
                payload: profile.encode(),
            })?;
        }
        for (id, hidden) in language_decisions {
            writer.push(&SnapshotRecordV1::DmLanguageDecision {
                id: *id.as_bytes(),
                hidden,
            })?;
        }
        for (file_conv, size, digest) in media {
            writer.push(&SnapshotRecordV1::MediaDescriptor {
                file_conv: *file_conv.as_bytes(),
                size,
                sha256: digest,
            })?;
            let mut sink = MediaRecordSink {
                writer: &mut writer,
                file_conv,
                buffer: Vec::with_capacity(MEDIA_CHUNK_BYTES),
                index: 0,
            };
            let written = self
                .media
                .copy_decrypted(file_conv, &mut sink)
                .map_err(|_| SnapshotError::InvalidMedia)?;
            if written != size {
                return Err(SnapshotError::InvalidMedia);
            }
            sink.finish()?;
        }
        writer.finish()
    }
}

fn preview_snapshot<R: Read>(source: R) -> Result<BackupPreview, SnapshotError> {
    let mut messages = 0u64;
    let mut channels = HashSet::new();
    let mut file_cards = 0u64;
    let mut media = HashSet::new();
    let mut file_references = Vec::new();
    let header = read_snapshot(source, |record| {
        match record {
            SnapshotRecordV1::SentDm { .. } | SnapshotRecordV1::ReceivedDm { .. } => {
                messages += 1;
            }
            SnapshotRecordV1::SignedEvent { event }
                if event.kind == crate::eventlog::EventKind::MembershipChange =>
            {
                channels.insert(event.conversation_id);
            }
            SnapshotRecordV1::FileCard { manifest, .. } => {
                file_cards += 1;
                let file = crate::file::decode_manifest(&manifest)
                    .ok_or(SnapshotError::InvalidFileCard)?;
                file_references.push(*file.file_conv().as_bytes());
            }
            SnapshotRecordV1::MediaDescriptor { file_conv, .. } => {
                media.insert(file_conv);
            }
            _ => {}
        }
        Ok(())
    })?;
    let missing_media = file_references
        .iter()
        .filter(|id| !media.contains(*id))
        .count() as u64;
    Ok(BackupPreview {
        account_id: header.account_id.clone(),
        created_at_ms: header.created_at_ms,
        messages,
        channels: channels.len() as u64,
        file_cards,
        media_files: media.len() as u64,
        missing_media,
    })
}
