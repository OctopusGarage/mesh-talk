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

/// Process-local first-DM metadata. Append-only revisions scan only unseen suffixes;
/// rewrites discard positions and routes, including an erased first envelope.
#[derive(Default)]
pub(in crate::node) struct KnownAccountCache {
    sent: FirstDmIndex,
    received: FirstDmIndex,
    #[cfg(test)]
    pub(in crate::node) scanned_rows: usize,
}

#[derive(Default)]
struct FirstDmIndex {
    revision: Option<u64>,
    rewrite_revision: u64,
    conversations: HashMap<ConversationId, FirstDmConversation>,
}

#[derive(Default)]
struct FirstDmConversation {
    scanned: usize,
    route: Option<super::DmRoute>,
}

impl FirstDmIndex {
    fn refresh(&mut self, revision: u64, rewrite_revision: u64) -> bool {
        if self.revision == Some(revision) {
            return false;
        }
        if self.rewrite_revision != rewrite_revision {
            self.conversations.clear();
        }
        self.revision = Some(revision);
        self.rewrite_revision = rewrite_revision;
        true
    }

    fn scan<'a>(
        &mut self,
        conversation: ConversationId,
        rows: impl Iterator<Item = &'a [u8]>,
    ) -> usize {
        let state = self.conversations.entry(conversation).or_default();
        if state.route.is_some() {
            return 0;
        }
        let mut scanned = 0;
        for plaintext in rows.skip(state.scanned) {
            scanned += 1;
            state.scanned += 1;
            if let Some(envelope) = super::DmEnvelope::decode(plaintext) {
                state.route = Some(envelope.route);
                break;
            }
        }
        scanned
    }
}

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
    favorite: bool,
    buffer: Vec<u8>,
    index: u32,
}

impl<W: Write> MediaRecordSink<'_, W> {
    fn push_buffer(&mut self) -> Result<(), SnapshotError> {
        let bytes = std::mem::take(&mut self.buffer);
        let record = if self.favorite {
            SnapshotRecordV1::FavoriteMediaChunk {
                file_conv: *self.file_conv.as_bytes(),
                index: self.index,
                bytes,
            }
        } else {
            SnapshotRecordV1::MediaChunk {
                file_conv: *self.file_conv.as_bytes(),
                index: self.index,
                bytes,
            }
        };
        self.writer.push(&record)?;
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
        let lease = crate::storage::profile::ProfileStorageLease::acquire(profile_dir)
            .map_err(|_| SnapshotError::Io)?;
        Self::open_recovery_profile_with_lease(lease, password)
    }

    /// Offline callers that already acquired ownership pass it exactly once.
    pub fn open_recovery_profile_with_lease(
        lease: crate::storage::profile::ProfileStorageLease,
        password: &str,
    ) -> Result<Arc<Self>, BackupError> {
        let profile_dir = lease.canonical_root().to_owned();
        lease
            .validate_identity_sources(&profile_dir.join("identity.keystore"))
            .map_err(|_| SnapshotError::Io)?;
        let identity = keystore::load(&profile_dir.join("identity.keystore"), password)
            .map_err(|_| SnapshotError::Io)?;
        let account = account_keystore::load(&profile_dir.join("account.keystore"), password)
            .map_err(|_| SnapshotError::Io)?;
        let (dm_tx, _) = mpsc::unbounded_channel();
        let (channel_tx, _) = mpsc::unbounded_channel();
        let (file_tx, _) = mpsc::unbounded_channel();
        Ok(Self::open_with_profile_lease(
            lease,
            super::NodeOpenRequest {
                identity,
                account,
                roster: Arc::new(Mutex::new(Roster::default())),
                incoming: dm_tx,
                channel_incoming: channel_tx,
                file_incoming: file_tx,
                log_path: &profile_dir.join("messages.log"),
                sent_path: &profile_dir.join("sent.log"),
                password,
            },
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
        let mut account_created_at_ms = None;
        let header = read_snapshot(&mut source, |record| {
            match record {
                SnapshotRecordV1::FileCard { manifest, .. } => {
                    let card = crate::file::decode_manifest(&manifest)
                        .ok_or(SnapshotError::InvalidFileCard)?;
                    media_names
                        .entry((false, *card.file_conv().as_bytes()))
                        .or_insert_with(|| card.name().to_string());
                }
                SnapshotRecordV1::Favorite { payload } => {
                    for file in super::favorites::recovery::summarize(&payload)?.files {
                        let conv = super::favorites::conversation(&file.file_conv)
                            .ok_or(SnapshotError::InvalidEncoding)?;
                        media_names
                            .entry((true, *conv.as_bytes()))
                            .or_insert(file.name);
                    }
                }
                SnapshotRecordV1::AccountCreation { created_at_ms } => {
                    account_created_at_ms = created_at_ms;
                }
                _ => {}
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
            let lease = crate::storage::profile::ProfileStorageLease::acquire(staging_dir)
                .map_err(|_| SnapshotError::Io)?;
            let identity = DeviceIdentity::generate();
            let account = Account::from_secret_bytes_with_created_at(
                header.account_secret,
                account_created_at_ms,
            )
            .ok_or(SnapshotError::InvalidAccount)?;
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
            let node = Self::open_with_profile_lease(
                lease,
                super::NodeOpenRequest {
                    identity,
                    account,
                    roster: Arc::new(Mutex::new(Roster::default())),
                    incoming: dm_tx,
                    channel_incoming: channel_tx,
                    file_incoming: file_tx,
                    log_path: &staging_dir.join("messages.log"),
                    sent_path: &staging_dir.join("sent.log"),
                    password: new_password,
                },
            )
            .map_err(|_| SnapshotError::Io)?;
            // A marker may follow its body in a valid archive. Install every
            // exact local erasure before any history or signed replay source.
            // Stream the pass instead of collecting another set of markers.
            let suppression_header = read_snapshot(&mut source, |record| {
                if let SnapshotRecordV1::LocalSuppression {
                    conversation,
                    message_id,
                } = record
                {
                    node.account_suppression
                        .lock()
                        .expect("account suppression mutex not poisoned")
                        .suppress(ConversationId::new(conversation), EventId::new(message_id))
                        .map_err(|_| SnapshotError::Io)?;
                }
                Ok(())
            })?;
            if suppression_header != header {
                return Err(SnapshotError::InvalidAccount);
            }
            source
                .seek(SeekFrom::Start(0))
                .map_err(|_| SnapshotError::Io)?;
            // Keep staged paths, not open handles: a valid archive can retain
            // thousands of independently referenced Favorites attachments.
            let mut media_temp: HashMap<(bool, [u8; 32]), PathBuf> = HashMap::new();
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
                        if node
                            .account_suppression
                            .lock()
                            .expect("account suppression mutex not poisoned")
                            .contains(ConversationId::new(conversation), EventId::new(message_id))
                        {
                            return Ok(());
                        }
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
                    } => {
                        let host = ConversationId::new(conversation);
                        // Ordinary account receives retain the signed transport
                        // ID, while local deletion uses the envelope's logical
                        // ID. Only an envelope belonging to this exact account
                        // host supplies that additional identity; legacy and
                        // channel rows keep their original event-ID semantics.
                        let logical = super::DmEnvelope::decode(&plaintext)
                            .filter(|envelope| {
                                super::conversation::account_conversation_id(
                                    &envelope.route.sender_account,
                                    &envelope.route.recipient_account,
                                ) == host
                            })
                            .map(|envelope| EventId::new(envelope.msg_id));
                        let suppression = node
                            .account_suppression
                            .lock()
                            .expect("account suppression mutex not poisoned");
                        let erased = suppression.contains(host, EventId::new(event_id))
                            || logical.is_some_and(|id| suppression.contains(host, id));
                        drop(suppression);
                        if erased {
                            return Ok(());
                        }
                        node.received
                            .lock()
                            .expect("received mutex not poisoned")
                            .record(
                                ConversationId::new(conversation),
                                from,
                                wall_clock,
                                &plaintext,
                                EventId::new(event_id),
                            )
                            .map_err(|_| SnapshotError::Io)?;
                    }
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
                    // Preserve old snapshot tags without restoring retired filtering state.
                    SnapshotRecordV1::DmLanguageProfile { .. }
                    | SnapshotRecordV1::DmLanguageDecision { .. } => {}
                    SnapshotRecordV1::AccountCreation { .. }
                    | SnapshotRecordV1::LocalSuppression { .. } => {}
                    SnapshotRecordV1::AccountProfile { profile } => {
                        node.receive_account_profile(&profile)
                            .map_err(|_| SnapshotError::InvalidEncoding)?;
                    }
                    SnapshotRecordV1::PeerTrustCard { card } => {
                        if !node
                            .receive_trust_card(&card)
                            .map_err(|_| SnapshotError::Io)?
                        {
                            return Err(SnapshotError::InvalidEncoding);
                        }
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
                    SnapshotRecordV1::Favorite { payload } => {
                        node.favorites
                            .lock()
                            .expect("favorites mutex")
                            .restore_portable(&payload, &node.account_id())?;
                    }
                    SnapshotRecordV1::MediaDescriptor { file_conv, .. }
                    | SnapshotRecordV1::FavoriteMediaDescriptor { file_conv, .. } => {
                        let favorite =
                            matches!(record, SnapshotRecordV1::FavoriteMediaDescriptor { .. });
                        let path = staging_dir.join(format!(
                            "recovery-media-{favorite}-{}",
                            hex::encode(file_conv)
                        ));
                        drop(std::fs::File::create(&path).map_err(|_| SnapshotError::Io)?);
                        media_temp.insert((favorite, file_conv), path);
                    }
                    SnapshotRecordV1::MediaChunk {
                        file_conv,
                        ref bytes,
                        ..
                    }
                    | SnapshotRecordV1::FavoriteMediaChunk {
                        file_conv,
                        ref bytes,
                        ..
                    } => {
                        let favorite =
                            matches!(record, SnapshotRecordV1::FavoriteMediaChunk { .. });
                        let path = media_temp
                            .get(&(favorite, file_conv))
                            .ok_or(SnapshotError::InvalidMedia)?;
                        std::fs::OpenOptions::new()
                            .append(true)
                            .open(path)
                            .and_then(|mut file| file.write_all(bytes))
                            .map_err(|_| SnapshotError::Io)?;
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
                    } => {
                        if node
                            .account_suppression
                            .lock()
                            .expect("account suppression mutex not poisoned")
                            .contains(ConversationId::new(conversation), EventId::new(event_id))
                        {
                            return Ok(());
                        }
                        node.received_files
                            .lock()
                            .expect("received_files mutex not poisoned")
                            .record(
                                ConversationId::new(conversation),
                                from,
                                wall_clock,
                                &manifest,
                                EventId::new(event_id),
                            )
                            .map_err(|_| SnapshotError::Io)?;
                    }
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
            for ((favorite, file_conv), path) in media_temp {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(|_| SnapshotError::Io)?;
                let name = media_names
                    .get(&(favorite, file_conv))
                    .map(String::as_str)
                    .unwrap_or("recovered.bin");
                let store = if favorite {
                    &node.favorite_media
                } else {
                    &node.media
                };
                store
                    .store_from_path(ConversationId::new(file_conv), name, &path)
                    .map_err(|_| SnapshotError::Io)?;
                std::fs::remove_file(path).map_err(|_| SnapshotError::Io)?;
            }
            // The node was opened before records were installed, so its cached
            // channel/file views are stale. Return a verified fresh view.
            drop(contacts_log);
            node.close_profile().map_err(|_| SnapshotError::Io)?;
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
        let mut cache = self
            .known_account_cache
            .lock()
            .expect("known account cache mutex not poisoned");
        let mut scanned = 0;
        let sent = self.sentlog.lock().expect("sentlog mutex not poisoned");
        if cache.sent.refresh(sent.revision(), sent.rewrite_revision()) {
            for conversation in sent.conversations() {
                scanned += cache.sent.scan(
                    conversation,
                    sent.entries_ref(&conversation)
                        .iter()
                        .map(|entry| entry.plaintext.as_slice()),
                );
            }
        }
        drop(sent);
        let received = self.received.lock().expect("received mutex not poisoned");
        if cache
            .received
            .refresh(received.revision(), received.rewrite_revision())
        {
            for conversation in received.conversations() {
                scanned += cache.received.scan(
                    conversation,
                    received
                        .entries_ref(&conversation)
                        .iter()
                        .map(|entry| entry.plaintext.as_slice()),
                );
            }
        }
        drop(received);
        #[cfg(test)]
        {
            cache.scanned_rows += scanned;
        }
        #[cfg(not(test))]
        let _ = scanned;
        for route in cache
            .sent
            .conversations
            .values()
            .chain(cache.received.conversations.values())
            .filter_map(|state| state.route.as_ref())
        {
            let other = if route.sender_account == own {
                &route.recipient_account
            } else if route.recipient_account == own {
                &route.sender_account
            } else {
                continue;
            };
            if other != &own && other.len() == 32 && other.bytes().all(|c| c.is_ascii_hexdigit()) {
                ids.insert(other.clone());
            }
        }
        drop(cache);
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
        let now = self.expiry_now();
        let favorites = self.favorites.lock().expect("favorites mutex");
        // Protection projection reads canonical history. Resolve it before taking
        // the snapshot's history locks, then reuse it for records, files and count.
        let portable_favorites = favorites.portable_projection(self);
        let favorite_files = portable_favorites.portable_files(now)?;
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
        let profiles = self.account_profiles.snapshot();
        let current_profiles = self
            .profiles
            .lock()
            .expect("profiles mutex not poisoned")
            .all();
        let trust_cards = self
            .trust_cards
            .lock()
            .expect("trust cards mutex not poisoned")
            .all();
        let suppression = self
            .account_suppression
            .lock()
            .expect("account suppression mutex not poisoned");
        let mut record_count = u64::try_from(
            sent_count
                .checked_add(received_count)
                .and_then(|count| count.checked_add(recalls.len()))
                .and_then(|count| count.checked_add(profiles.len()))
                .and_then(|count| count.checked_add(trust_cards.len()))
                .and_then(|count| count.checked_add(current_profiles.len()))
                .and_then(|count| count.checked_add(1))
                .and_then(|count| count.checked_add(event_count))
                .and_then(|count| count.checked_add(file_count))
                .and_then(|count| count.checked_add(file_contacts.len()))
                .and_then(|count| count.checked_add(portable_favorites.len()))
                .and_then(|count| count.checked_add(suppression.len()))
                .ok_or(SnapshotError::SizeLimit)?,
        )
        .map_err(|_| SnapshotError::SizeLimit)?;
        let mut media = Vec::new();
        let visible_favorite_media = favorite_files
            .iter()
            .filter(|(_, file)| file.size <= super::favorites::recovery::PORTABLE_FILE_LIMIT)
            .map(|(conv, _)| *conv)
            .collect::<HashSet<_>>();
        for (favorite, store, visible) in [
            (false, &self.media, &visible_media),
            (true, &self.favorite_media, &visible_favorite_media),
        ] {
            for (file_conv, _) in store.managed_files().map_err(|_| SnapshotError::Io)? {
                if !visible.contains(&file_conv) {
                    continue;
                }
                // Authenticate every source chunk before wrapping portable plaintext
                // in the independently encrypted recovery archive.
                let mut digest = MediaDigest(Sha256::new());
                let size = store
                    .copy_decrypted(file_conv, &mut digest)
                    .map_err(|_| SnapshotError::InvalidMedia)?;
                if favorite
                    && favorite_files
                        .get(&file_conv)
                        .is_none_or(|file| file.size != size)
                {
                    return Err(SnapshotError::InvalidMedia);
                }
                record_count = record_count
                    .checked_add(1)
                    .and_then(|count| count.checked_add(size.div_ceil(MEDIA_CHUNK_BYTES as u64)))
                    .ok_or(SnapshotError::SizeLimit)?;
                media.push((
                    favorite,
                    file_conv,
                    size,
                    <[u8; 32]>::from(digest.0.finalize()),
                ));
            }
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
        for (conversation, id) in suppression.records() {
            writer.push(&SnapshotRecordV1::LocalSuppression {
                conversation: *conversation.as_bytes(),
                message_id: *id.as_bytes(),
            })?;
        }
        drop(suppression);
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
                if super::expiry_policy::plaintext_deadline(&entry.plaintext)
                    .is_some_and(|deadline| deadline <= self.expiry_now())
                {
                    continue;
                }
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
        for conversation in received_conversations {
            for entry in received.entries_ref(&conversation) {
                let envelope = super::DmEnvelope::decode(&entry.plaintext);
                let inner = envelope
                    .as_ref()
                    .map_or(entry.plaintext.as_slice(), |env| env.body.as_slice());
                if self.recipient_expired(inner, entry.event_id, entry.wall_clock) {
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
                    || self.recipient_expired(&entry.plaintext, entry.event_id, entry.wall_clock)
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
        writer.push(&SnapshotRecordV1::AccountCreation {
            created_at_ms: self.account.created_at_ms(),
        })?;
        for profile in current_profiles {
            writer.push(&SnapshotRecordV1::AccountProfile { profile })?;
        }
        for card in trust_cards {
            writer.push(&SnapshotRecordV1::PeerTrustCard { card })?;
        }
        for payload in portable_favorites.portable_records(&own_account, now) {
            writer.push(&SnapshotRecordV1::Favorite { payload: payload? })?;
        }
        for (favorite, file_conv, size, digest) in media {
            let descriptor = if favorite {
                SnapshotRecordV1::FavoriteMediaDescriptor {
                    file_conv: *file_conv.as_bytes(),
                    size,
                    sha256: digest,
                }
            } else {
                SnapshotRecordV1::MediaDescriptor {
                    file_conv: *file_conv.as_bytes(),
                    size,
                    sha256: digest,
                }
            };
            writer.push(&descriptor)?;
            let mut sink = MediaRecordSink {
                writer: &mut writer,
                file_conv,
                favorite,
                buffer: Vec::with_capacity(MEDIA_CHUNK_BYTES),
                index: 0,
            };
            let store = if favorite {
                &self.favorite_media
            } else {
                &self.media
            };
            let written = store
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
    let mut favorites = 0u64;
    let mut favorite_media = HashSet::new();
    let mut favorite_references = HashSet::new();
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
            SnapshotRecordV1::Favorite { payload } => {
                let favorite = super::favorites::recovery::summarize(&payload)?;
                if !favorite.removed {
                    favorites += 1;
                }
                favorite_references.extend(favorite.files.into_iter().map(|file| file.file_conv));
            }
            SnapshotRecordV1::FavoriteMediaDescriptor { file_conv, .. } => {
                favorite_media.insert(hex::encode(file_conv));
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
        favorites,
        favorite_files: favorite_media.len() as u64,
        missing_favorite_files: favorite_references.difference(&favorite_media).count() as u64,
    })
}

#[cfg(test)]
mod suppression_recovery_tests {
    use super::*;

    #[test]
    fn restore_suppresses_account_logical_id_when_transport_event_id_differs() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::from_secret_bytes([71; 32]);
        let peer = Account::from_secret_bytes([72; 32]).account_id();
        let host =
            super::super::conversation::account_conversation_id(&account.account_id(), &peer);
        let logical = [76; 32];
        let wire = [77; 32];
        let body = super::super::DmEnvelope::new(
            peer.clone(),
            account.account_id(),
            logical,
            b"deleted account message".to_vec(),
        )
        .encode();
        let records = [
            SnapshotRecordV1::AccountCreation {
                created_at_ms: None,
            },
            SnapshotRecordV1::ReceivedDm {
                conversation: *host.as_bytes(),
                event_id: wire,
                from: peer,
                wall_clock: 100,
                plaintext: body,
            },
            SnapshotRecordV1::LocalSuppression {
                conversation: *host.as_bytes(),
                message_id: logical,
            },
        ];
        let header = SnapshotHeaderV1 {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            account_secret: account.secret_bytes(),
            account_id: account.account_id(),
            source_device_ed25519_pubs: vec![DeviceIdentity::generate().public().ed25519_pub],
            created_at_ms: 200,
            record_count: records.len() as u64,
        };
        let mut bytes = Vec::new();
        crate::recovery::snapshot::write_snapshot(&mut bytes, &header, records).unwrap();
        let restored = Node::install_recovery_snapshot(
            std::io::Cursor::new(bytes),
            &dir.path().join("restored"),
            "pw",
        )
        .unwrap();
        assert!(
            restored
                .received
                .lock()
                .unwrap()
                .entry(EventId::new(wire))
                .is_none(),
            "a logical account erasure must hide every transport copy before installation"
        );
    }

    #[test]
    fn restore_installs_late_markers_before_bodies_and_preserves_other_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::from_secret_bytes([71; 32]);
        let a = [72; 32];
        let b = [73; 32];
        let id = [74; 32];
        let received_id = [75; 32];
        let records = [
            SnapshotRecordV1::AccountCreation {
                created_at_ms: None,
            },
            SnapshotRecordV1::SentDm {
                conversation: a,
                message_id: id,
                seq: 1,
                wall_clock: 100,
                plaintext: b"erased source".to_vec(),
            },
            SnapshotRecordV1::SentDm {
                conversation: b,
                message_id: id,
                seq: 1,
                wall_clock: 100,
                plaintext: b"independent host".to_vec(),
            },
            SnapshotRecordV1::ReceivedDm {
                conversation: a,
                event_id: received_id,
                from: "peer".into(),
                wall_clock: 100,
                plaintext: b"erased received source".to_vec(),
            },
            SnapshotRecordV1::LocalSuppression {
                conversation: a,
                message_id: id,
            },
            SnapshotRecordV1::LocalSuppression {
                conversation: a,
                message_id: received_id,
            },
            SnapshotRecordV1::LocalSuppression {
                conversation: a,
                message_id: id,
            },
        ];
        let header = SnapshotHeaderV1 {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            account_secret: account.secret_bytes(),
            account_id: account.account_id(),
            source_device_ed25519_pubs: vec![DeviceIdentity::generate().public().ed25519_pub],
            created_at_ms: 200,
            record_count: records.len() as u64,
        };
        let mut bytes = Vec::new();
        crate::recovery::snapshot::write_snapshot(&mut bytes, &header, records).unwrap();
        let restored = Node::install_recovery_snapshot(
            std::io::Cursor::new(bytes),
            &dir.path().join("restored"),
            "pw",
        )
        .unwrap();
        let sent = restored.sentlog.lock().unwrap();
        assert!(sent.entries_ref(&ConversationId::new(a)).is_empty());
        let kept = sent.entries_ref(&ConversationId::new(b));
        assert_eq!(kept.len(), 1);
        assert_eq!(kept[0].plaintext, b"independent host");
        assert_eq!(
            restored
                .recovered_sent_ids
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .id_for(&kept[0]),
            Some(EventId::new(id))
        );
        assert!(
            restored
                .received
                .lock()
                .unwrap()
                .entry(EventId::new(received_id))
                .is_none()
        );
        assert_eq!(restored.account_suppression.lock().unwrap().len(), 2);
    }

    fn open(root: &Path) -> Arc<Node> {
        let (d, _) = mpsc::unbounded_channel();
        let (c, _) = mpsc::unbounded_channel();
        let (f, _) = mpsc::unbounded_channel();
        Node::open_with_account(
            DeviceIdentity::generate(),
            Account::generate(),
            Arc::new(Mutex::new(Roster::default())),
            d,
            c,
            f,
            &root.join("messages.log"),
            &root.join("sent.log"),
            "pw",
        )
        .unwrap()
    }

    fn backfill(node: &Node, peer: &str, id: u8) -> super::super::pairing::BackfillRecord {
        let conversation =
            super::super::conversation::account_conversation_id(&node.account_id(), peer);
        super::super::pairing::BackfillRecord {
            conv: *conversation.as_bytes(),
            event_id: [id; 32],
            from: peer.into(),
            wall_clock: 100,
            plaintext: super::super::DmEnvelope::new(
                peer.into(),
                node.account_id(),
                [id; 32],
                b"retained text".to_vec(),
            )
            .encode(),
        }
    }

    #[test]
    fn recovery_preserves_erasure_against_stale_account_backfill() {
        let dir = tempfile::tempdir().unwrap();
        let node = open(dir.path());
        let peer = Account::generate().account_id();
        let erased = backfill(&node, &peer, 41);
        let retained = backfill(&node, &peer, 42);
        node.import_account_backfill(&[erased.clone(), retained.clone()])
            .unwrap();
        assert_eq!(
            node.delete_account_message(&peer, EventId::new(erased.event_id))
                .unwrap(),
            1
        );
        let mut bytes = Vec::new();
        node.write_recovery_snapshot(&mut bytes, 200).unwrap();
        let restored = Node::install_recovery_snapshot(
            std::io::Cursor::new(bytes),
            &dir.path().join("restored"),
            "new-password",
        )
        .unwrap();
        assert_ne!(restored.identity.user_id(), node.identity.user_id());
        assert_eq!(restored.account_id(), node.account_id());
        restored
            .import_account_backfill(&[erased.clone(), retained.clone()])
            .unwrap();
        let rows = restored.received.lock().unwrap();
        assert!(
            rows.entry(EventId::new(erased.event_id)).is_none(),
            "restoring an archive must preserve exact erasure evidence before sibling catch-up"
        );
        assert!(rows.entry(EventId::new(retained.event_id)).is_some());
        drop(rows);
        let new_row = backfill(&restored, &peer, 43);
        restored
            .import_account_backfill(std::slice::from_ref(&new_row))
            .unwrap();
        assert!(
            restored
                .received
                .lock()
                .unwrap()
                .entry(EventId::new(new_row.event_id))
                .is_some()
        );
        let mut second = Vec::new();
        restored.write_recovery_snapshot(&mut second, 300).unwrap();
        let again = Node::install_recovery_snapshot(
            std::io::Cursor::new(second),
            &dir.path().join("restored-again"),
            "newer-password",
        )
        .unwrap();
        again.import_account_backfill(&[erased]).unwrap();
        assert!(
            again
                .received
                .lock()
                .unwrap()
                .entry(EventId::new([41; 32]))
                .is_none()
        );
    }
}

#[cfg(test)]
mod creation_metadata_tests {
    use super::*;

    #[test]
    fn legacy_snapshot_claimed_dates_never_become_authoritative_account_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let header = SnapshotHeaderV1 {
            schema_version: 3,
            account_secret: account.secret_bytes(),
            account_id: account.account_id(),
            source_device_ed25519_pubs: vec![DeviceIdentity::generate().public().ed25519_pub],
            created_at_ms: 100,
            record_count: 1,
        };
        let card = crate::node::TrustCard::new(
            &account,
            1,
            "Legacy".into(),
            String::new(),
            Some(42),
            None,
        );
        let mut bytes = Vec::new();
        crate::recovery::snapshot::write_snapshot(
            &mut bytes,
            &header,
            [SnapshotRecordV1::PeerTrustCard { card }],
        )
        .unwrap();
        let target = dir.path().join("legacy-restored");
        let restored =
            Node::install_recovery_snapshot(std::io::Cursor::new(bytes), &target, "pw").unwrap();
        assert_eq!(restored.account.created_at_ms(), None);
        assert_eq!(
            account_keystore::load(&target.join("account.keystore"), "pw")
                .unwrap()
                .created_at_ms(),
            None
        );
    }

    #[test]
    fn recovery_preserves_authoritative_creation_and_never_uses_backup_time() {
        for account in [Account::generate(), Account::from_secret_bytes([39; 32])] {
            let dir = tempfile::tempdir().unwrap();
            let expected = account.created_at_ms();
            let (d, _) = mpsc::unbounded_channel();
            let (c, _) = mpsc::unbounded_channel();
            let (f, _) = mpsc::unbounded_channel();
            let node = Node::open_with_account(
                DeviceIdentity::generate(),
                account,
                Arc::new(Mutex::new(Roster::default())),
                d,
                c,
                f,
                &dir.path().join("events.log"),
                &dir.path().join("sent.log"),
                "pw",
            )
            .unwrap();
            let mut bytes = Vec::new();
            node.write_recovery_snapshot(&mut bytes, 100).unwrap();
            let target = dir.path().join("restored");
            let restored = Node::install_recovery_snapshot(
                std::io::Cursor::new(bytes),
                &target,
                "new-password",
            )
            .unwrap();
            assert_eq!(restored.account.created_at_ms(), expected);
            assert_eq!(
                account_keystore::load(&target.join("account.keystore"), "new-password")
                    .unwrap()
                    .created_at_ms(),
                expected
            );
            let mut bytes = Vec::new();
            restored.write_recovery_snapshot(&mut bytes, 200).unwrap();
            let restored_again = Node::install_recovery_snapshot(
                std::io::Cursor::new(bytes),
                &dir.path().join("restored-again"),
                "newer-password",
            )
            .unwrap();
            assert_eq!(restored_again.account.created_at_ms(), expected);
        }
    }
}
