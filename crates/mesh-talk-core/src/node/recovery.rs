//! Offline export adapter. The desktop host must retire the runtime before
//! calling this method so all stores belong to one consistent snapshot.

use super::Node;
use crate::discovery::roster::Roster;
use crate::eventlog::LogError;
use crate::eventlog::event::{ConversationId, EventId, EventKind};
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

struct MediaRecordSink<'a> {
    emit: &'a mut dyn FnMut(SnapshotRecordV1) -> Result<(), SnapshotError>,
    file_conv: ConversationId,
    favorite: bool,
    buffer: Vec<u8>,
    index: u32,
}

impl MediaRecordSink<'_> {
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
        (self.emit)(record)?;
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

impl Write for MediaRecordSink<'_> {
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
        let mut signed_ids = HashSet::new();
        let mut manifest_parents = HashMap::new();
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
                SnapshotRecordV1::SignedEvent { event } => {
                    signed_ids.insert(event.id);
                    if event.kind == EventKind::FileManifest {
                        manifest_parents.insert(event.id, event.parents);
                    }
                }
                _ => {}
            }
            Ok(())
        })?;
        // Detached sibling proofs do not carry their original device-pair DAG.
        // Preserve them outside the log, including descendants of detached proofs.
        let mut detached = HashSet::new();
        let mut children: HashMap<EventId, Vec<EventId>> = HashMap::new();
        let mut pending = std::collections::VecDeque::new();
        for (id, parents) in &manifest_parents {
            if parents.iter().any(|parent| !signed_ids.contains(parent)) {
                pending.push_back(*id);
            }
            for parent in parents {
                children.entry(*parent).or_default().push(*id);
            }
        }
        while let Some(id) = pending.pop_front() {
            if detached.insert(id)
                && let Some(next) = children.get(&id)
            {
                pending.extend(next.iter().copied());
            }
        }
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
            let mut identity_bundles = Vec::new();
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
                            .map_err(|_| SnapshotError::Io)?
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
                        let erased = suppression
                            .contains(host, EventId::new(event_id))
                            .map_err(|_| SnapshotError::Io)?
                            || logical
                                .map(|id| suppression.contains(host, id))
                                .transpose()
                                .map_err(|_| SnapshotError::Io)?
                                .unwrap_or(false);
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
                        if super::file_message_identity::is_bundle(&event) {
                            identity_bundles.push(event);
                            return Ok(());
                        }
                        if detached.contains(&event.id) {
                            node.account_file_proofs
                                .lock()
                                .expect("file proofs lock")
                                .insert(event)
                                .map_err(|_| SnapshotError::InvalidEvent)?;
                            return Ok(());
                        }
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
                            .map_err(|_| SnapshotError::Io)?
                        {
                            return Ok(());
                        }
                        let event_id = EventId::new(event_id);
                        let original = node
                            .log
                            .lock()
                            .expect("log mutex")
                            .get(&event_id)
                            .map_err(|_| SnapshotError::Io)?;
                        let row = super::received_log::ReceivedEntry {
                            conversation: ConversationId::new(conversation),
                            event_id,
                            from,
                            wall_clock,
                            plaintext: manifest,
                        };
                        node.received_files
                            .lock()
                            .expect("received_files mutex not poisoned")
                            .record_file_with_reception(&row, original.as_ref(), None)
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
            for bundle in identity_bundles {
                node.import_file_message_identity(bundle)
                    .map_err(|_| SnapshotError::InvalidFileCard)?;
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
    pub fn known_account_ids(&self) -> Result<Vec<String>, super::NodeError> {
        let own = self.account.account_id();
        let mut ids: BTreeSet<String> = self.recovery_contacts.iter().cloned().collect();
        for received in [false, true] {
            let mut after = None;
            loop {
                let page = if received {
                    self.received
                        .lock()
                        .expect("received mutex")
                        .account_routes_page(after, 128)?
                } else {
                    self.sentlog
                        .lock()
                        .expect("sent mutex")
                        .account_routes_page(after, 128)?
                };
                if page.is_empty() {
                    break;
                }
                after = page.last().map(|(conversation, _)| *conversation);
                for (_, route) in page {
                    let Some(route) = route else {
                        continue;
                    };
                    let other = if route.sender_account == own {
                        route.recipient_account
                    } else if route.recipient_account == own {
                        route.sender_account
                    } else {
                        continue;
                    };
                    if other != own {
                        ids.insert(other);
                    }
                }
            }
        }
        let files = self.received_files.lock().expect("files mutex");
        for proof in self.historical_dm_peers() {
            if let Some(account) = proof.account_id()
                && account != own
                && files.count(Some(&super::conversation::account_conversation_id(
                    &own, &account,
                )))? > 0
            {
                ids.insert(account);
            }
        }
        Ok(ids.into_iter().collect())
    }

    /// Write the currently supported local plaintext records as typed snapshot
    /// rows. The host calls this only after its runtime has fully stopped.
    pub(in crate::node) fn write_recovery_snapshot<W: Write>(
        &self,
        destination: W,
        created_at_ms: u64,
    ) -> Result<(), SnapshotError> {
        self.prune_expired_received()
            .map_err(|_| SnapshotError::Io)?;
        let mut record_count = 0u64;
        self.visit_recovery_records(&mut |_| {
            record_count = record_count
                .checked_add(1)
                .ok_or(SnapshotError::SizeLimit)?;
            Ok(())
        })?;
        let mut sources: Vec<_> = self
            .recovery_origins
            .iter()
            .map(|a| *a.ed25519_pub())
            .collect();
        let current = self.identity.public().ed25519_pub;
        if !sources.contains(&current) {
            sources.push(current);
        }
        let header = SnapshotHeaderV1 {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            account_secret: self.account.secret_bytes(),
            account_id: self.account_id(),
            source_device_ed25519_pubs: sources,
            created_at_ms,
            record_count,
        };
        let mut writer = SnapshotWriter::new(destination, &header)?;
        self.visit_recovery_records(&mut |record| writer.push(&record))?;
        writer.finish()
    }

    /// Offline snapshot passes retain at most one body page and one media chunk.
    fn visit_recovery_records(
        &self,
        emit: &mut dyn FnMut(SnapshotRecordV1) -> Result<(), SnapshotError>,
    ) -> Result<(), SnapshotError> {
        use super::transcript_repository::TranscriptPageLimit;
        use crate::eventlog::repository::pages::PageBudget;
        let io = |_| SnapshotError::Io;
        let mut cursor = None;
        loop {
            let page = self
                .account_suppression
                .lock()
                .expect("suppression mutex")
                .page(cursor, 128)
                .map_err(io)?;
            if page.is_empty() {
                break;
            }
            for (conversation, id) in page {
                cursor = Some((conversation, id));
                emit(SnapshotRecordV1::LocalSuppression {
                    conversation: *conversation.as_bytes(),
                    message_id: *id.as_bytes(),
                })?;
            }
        }
        let mut cursor = None;
        loop {
            let page = self
                .sentlog
                .lock()
                .expect("sent mutex")
                .header_scan_page(None, cursor, TranscriptPageLimit::default())
                .map_err(io)?;
            if page.rows.is_empty() {
                break;
            }
            for header in page.rows {
                cursor = Some(header.ordinal);
                let Some(entry) = self
                    .sentlog
                    .lock()
                    .expect("sent mutex")
                    .row_by_ordinal(header.ordinal)
                    .map_err(io)?
                else {
                    continue;
                };
                let mut id = header
                    .logical_id
                    .map_or_else(|| self.sent_header_identity(&header), Ok)
                    .map_err(io)?;
                if id == EventId::new([0; 32]) {
                    let mut hash = Sha256::new();
                    hash.update(b"mesh-talk-recovery-legacy-sent-v1");
                    hash.update(header.conversation.as_bytes());
                    hash.update(header.sequence.to_be_bytes());
                    id = EventId::new(hash.finalize().into());
                }
                emit(SnapshotRecordV1::SentDm {
                    conversation: *entry.conversation.as_bytes(),
                    message_id: *id.as_bytes(),
                    seq: entry.seq,
                    wall_clock: entry.wall_clock,
                    plaintext: entry.plaintext,
                })?;
            }
        }
        let mut cursor = None;
        loop {
            let page = self
                .received
                .lock()
                .expect("received mutex")
                .scan_page(None, cursor, TranscriptPageLimit::default())
                .map_err(io)?;
            for entry in page.rows {
                emit(SnapshotRecordV1::ReceivedDm {
                    conversation: *entry.conversation.as_bytes(),
                    event_id: *entry.event_id.as_bytes(),
                    from: entry.from,
                    wall_clock: entry.wall_clock,
                    plaintext: entry.plaintext,
                })?;
            }
            let Some(next) = page.next else {
                break;
            };
            cursor = Some(next);
        }
        let mut conversations = None;
        loop {
            let page = self
                .log
                .lock()
                .expect("log mutex")
                .repository()
                .map_err(io)?
                .conversations_page(
                    conversations.as_ref(),
                    PageBudget {
                        rows: 128,
                        bytes: 8 * 1024 * 1024,
                    },
                )
                .map_err(io)?;
            for conversation in page.items {
                let mut cursor = None;
                loop {
                    let events = self
                        .log
                        .lock()
                        .expect("log mutex")
                        .repository()
                        .map_err(io)?
                        .events_page(
                            &conversation.conversation,
                            cursor.as_ref(),
                            PageBudget {
                                rows: 128,
                                bytes: 8 * 1024 * 1024,
                            },
                        )
                        .map_err(io)?;
                    for event in events.items {
                        emit(SnapshotRecordV1::SignedEvent { event })?;
                    }
                    let Some(next) = events.next else {
                        break;
                    };
                    cursor = Some(next);
                }
            }
            let Some(next) = page.next else {
                break;
            };
            conversations = Some(next);
        }
        // The auxiliary owners enumerate unique proof identities in bounded pages.
        self.account_file_proofs
            .lock()
            .expect("proofs mutex")
            .visit(|event| {
                if self
                    .log
                    .lock()
                    .expect("log mutex")
                    .get(&event.id)?
                    .is_none()
                    && let Some(row) = self
                        .received_files
                        .lock()
                        .expect("files mutex")
                        .entry(event.id)?
                    && !self.file_message_row_suppressed(&row)?
                {
                    emit(SnapshotRecordV1::SignedEvent { event })
                        .map_err(|_| LogError::CorruptFile("snapshot proof write failed".into()))?;
                }
                Ok(())
            })
            .map_err(io)?;
        let mut identities_after = None;
        loop {
            let page = self
                .file_message_identities
                .lock()
                .expect("identity mutex")
                .page(identities_after, 128)
                .map_err(io)?;
            if page.is_empty() {
                break;
            }
            for event in page {
                let (binding, aliases) =
                    super::file_message_identity::validated_snapshot_binding(&event)
                        .ok_or(SnapshotError::InvalidFileCard)?;
                identities_after = Some((binding.host, binding.logical));
                let local_source = event.author.ed25519_pub()
                    == &self.identity.public().ed25519_pub
                    || self
                        .recovery_origins
                        .iter()
                        .any(|author| author == &event.author);
                let mut retained = false;
                for alias in aliases {
                    let row = {
                        let files = self.received_files.lock().expect("files mutex");
                        if local_source {
                            files.entry(alias)
                        } else {
                            files.entry_in(&binding.host, alias)
                        }
                    }
                    .map_err(io)?;
                    if let Some(row) = row
                        && self.snapshot_file_visible(&row)?
                    {
                        retained = true;
                        break;
                    }
                }
                // Restore authenticates each identity against an included card.
                // Erased aliases travel as suppression records, without either
                // their plaintext card or a now-unverifiable identity bundle.
                if retained
                    && self
                        .log
                        .lock()
                        .expect("log mutex")
                        .get(&event.id)
                        .map_err(io)?
                        .is_none()
                {
                    emit(SnapshotRecordV1::SignedEvent { event })?;
                }
            }
        }
        let own = self.account_id();
        for account in self.known_account_ids().map_err(|_| SnapshotError::Io)? {
            let host = super::conversation::account_conversation_id(&own, &account);
            let mut cursor = None;
            let mut visible = false;
            loop {
                let page = self
                    .received_files
                    .lock()
                    .expect("files mutex")
                    .scan_page(Some(&host), cursor, TranscriptPageLimit::default())
                    .map_err(io)?;
                for row in page.rows {
                    if self.snapshot_file_visible(&row)? {
                        visible = true;
                        break;
                    }
                }
                if visible {
                    break;
                }
                let Some(next) = page.next else {
                    break;
                };
                cursor = Some(next);
            }
            if visible {
                emit(SnapshotRecordV1::AccountContact {
                    account_id: account,
                })?;
            }
        }
        let mut cursor = None;
        loop {
            let page = self
                .received_files
                .lock()
                .expect("files mutex")
                .scan_page(None, cursor, TranscriptPageLimit::default())
                .map_err(io)?;
            for row in page.rows {
                if self.snapshot_file_visible(&row)? {
                    emit(SnapshotRecordV1::FileCard {
                        conversation: *row.conversation.as_bytes(),
                        event_id: *row.event_id.as_bytes(),
                        from: row.from,
                        wall_clock: row.wall_clock,
                        manifest: row.plaintext,
                    })?;
                }
            }
            let Some(next) = page.next else {
                break;
            };
            cursor = Some(next);
        }
        let mut cursor = None;
        loop {
            let page = self
                .recalls
                .lock()
                .expect("recalls mutex")
                .page(None, cursor, 128)
                .map_err(io)?;
            if page.is_empty() {
                break;
            }
            for mark in page {
                cursor = Some((mark.conversation, mark.target));
                emit(SnapshotRecordV1::Recall {
                    conversation: *mark.conversation.as_bytes(),
                    target: *mark.target.as_bytes(),
                    wall_clock: mark.wall_clock,
                })?;
            }
        }
        for (account_id, updated_at, avatar) in self.account_profiles.snapshot() {
            emit(SnapshotRecordV1::Profile {
                account_id,
                updated_at,
                avatar,
            })?;
        }
        emit(SnapshotRecordV1::AccountCreation {
            created_at_ms: self.account.created_at_ms(),
        })?;
        for profile in self.profiles.lock().expect("profiles mutex").all() {
            emit(SnapshotRecordV1::AccountProfile { profile })?;
        }
        for card in self.trust_cards.lock().expect("trust cards mutex").all() {
            emit(SnapshotRecordV1::PeerTrustCard { card })?;
        }
        let now = self.expiry_now();
        let favorites = self.favorites.lock().expect("favorites mutex");
        let portable = favorites.portable_projection(self);
        let favorite_files = portable.portable_files(now)?;
        for payload in portable.portable_records(&own, now) {
            emit(SnapshotRecordV1::Favorite { payload: payload? })?;
        }
        for (favorite, store) in [(false, &self.media), (true, &self.favorite_media)] {
            for (file_conv, _) in store.managed_files().map_err(|_| SnapshotError::Io)? {
                let visible = if favorite {
                    favorite_files
                        .get(&file_conv)
                        .is_some_and(|f| f.size <= super::favorites::recovery::PORTABLE_FILE_LIMIT)
                } else {
                    self.snapshot_media_visible(file_conv)?
                };
                if !visible {
                    continue;
                }
                let mut hash = MediaDigest(Sha256::new());
                let size = store
                    .copy_decrypted(file_conv, &mut hash)
                    .map_err(|_| SnapshotError::InvalidMedia)?;
                if favorite
                    && favorite_files
                        .get(&file_conv)
                        .is_none_or(|f| f.size != size)
                {
                    return Err(SnapshotError::InvalidMedia);
                }
                let sha256 = hash.0.finalize().into();
                emit(if favorite {
                    SnapshotRecordV1::FavoriteMediaDescriptor {
                        file_conv: *file_conv.as_bytes(),
                        size,
                        sha256,
                    }
                } else {
                    SnapshotRecordV1::MediaDescriptor {
                        file_conv: *file_conv.as_bytes(),
                        size,
                        sha256,
                    }
                })?;
                let mut sink = MediaRecordSink {
                    emit,
                    file_conv,
                    favorite,
                    buffer: Vec::with_capacity(MEDIA_CHUNK_BYTES),
                    index: 0,
                };
                let written = store
                    .copy_decrypted(file_conv, &mut sink)
                    .map_err(|_| SnapshotError::InvalidMedia)?;
                if written != size {
                    return Err(SnapshotError::InvalidMedia);
                }
                sink.finish()?;
            }
        }
        Ok(())
    }
    fn snapshot_file_visible(
        &self,
        row: &super::received_log::ReceivedEntry,
    ) -> Result<bool, SnapshotError> {
        let delivery = self.delivery.lock().expect("delivery mutex");
        Ok(!delivery.file_erased(row.conversation, row.event_id)
            && !delivery.manifest_event_erased(row.event_id)
            && !self
                .file_message_row_suppressed(row)
                .map_err(|_| SnapshotError::Io)?)
    }
    fn snapshot_media_visible(&self, file: ConversationId) -> Result<bool, SnapshotError> {
        let mut cursor = None;
        loop {
            let page = self
                .received_files
                .lock()
                .expect("files mutex")
                .file_headers(
                    &file,
                    cursor,
                    super::transcript_repository::TranscriptPageLimit::default(),
                )
                .map_err(|_| SnapshotError::Io)?;
            if page.rows.is_empty() {
                return Ok(false);
            }
            for header in page.rows {
                cursor = Some(header.ordinal);
                if let Some(row) = self
                    .received_files
                    .lock()
                    .expect("files mutex")
                    .row_by_ordinal(header.ordinal)
                    .map_err(|_| SnapshotError::Io)?
                    && self.snapshot_file_visible(&row)?
                {
                    return Ok(true);
                }
            }
        }
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
                .unwrap()
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
                .id_for(&kept[0])
                .unwrap(),
            Some(EventId::new(id))
        );
        assert!(
            restored
                .received
                .lock()
                .unwrap()
                .entry(EventId::new(received_id))
                .unwrap()
                .is_none()
        );
        assert_eq!(
            restored.account_suppression.lock().unwrap().len().unwrap(),
            2
        );
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
            rows.entry(EventId::new(erased.event_id)).unwrap().is_none(),
            "restoring an archive must preserve exact erasure evidence before sibling catch-up"
        );
        assert!(
            rows.entry(EventId::new(retained.event_id))
                .unwrap()
                .is_some()
        );
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
                .unwrap()
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
                .unwrap()
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
