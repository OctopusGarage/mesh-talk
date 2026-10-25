//! Versioned logical snapshot records. The format deliberately has no device
//! private key, ratchet state, sender chain, network route or filesystem path.

use crate::eventlog::event::{Event, EventKind};
use crate::identity::account::Account;
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{HashMap, HashSet};
use std::io::{Read, Write};
use zeroize::Zeroize;

pub const SNAPSHOT_SCHEMA_VERSION: u16 = 5;
const MAGIC: &[u8; 8] = b"MTSNAP1\0";
const MAX_RECORDS: u64 = 10_000_000;
const MAX_RECORD_BYTES: usize = 1024 * 1024;
const MAX_ENCODED_RECORD_BYTES: usize = MAX_RECORD_BYTES + 1024;
const MAX_HEADER_BYTES: usize = 4096;
const MAX_MEDIA_CHUNK_BYTES: usize = 64 * 1024;
const MAX_MEDIA_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_TOTAL_MEDIA_BYTES: u64 = 8 * 1024 * 1024 * 1024;

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SnapshotHeaderV1 {
    pub schema_version: u16,
    pub account_secret: [u8; 32],
    pub account_id: String,
    /// Public authors of devices that wrote local sent/file sidecars across migrations.
    pub source_device_ed25519_pubs: Vec<[u8; 32]>,
    pub created_at_ms: u64,
    pub record_count: u64,
}

impl Drop for SnapshotHeaderV1 {
    fn drop(&mut self) {
        self.account_secret.zeroize();
    }
}

impl std::fmt::Debug for SnapshotHeaderV1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SnapshotHeaderV1")
            .field("schema_version", &self.schema_version)
            .field("account_secret", &"[redacted]")
            .field("account_id", &self.account_id)
            .field(
                "source_device_count",
                &self.source_device_ed25519_pubs.len(),
            )
            .field("created_at_ms", &self.created_at_ms)
            .field("record_count", &self.record_count)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SnapshotRecordV1 {
    SentDm {
        conversation: [u8; 32],
        message_id: [u8; 32],
        seq: u64,
        wall_clock: u64,
        plaintext: Vec<u8>,
    },
    ReceivedDm {
        conversation: [u8; 32],
        event_id: [u8; 32],
        from: String,
        wall_clock: u64,
        plaintext: Vec<u8>,
    },
    Recall {
        conversation: [u8; 32],
        target: [u8; 32],
        wall_clock: u64,
    },
    Profile {
        account_id: String,
        updated_at: u64,
        avatar: Option<Vec<u8>>,
    },
    MediaDescriptor {
        file_conv: [u8; 32],
        size: u64,
        sha256: [u8; 32],
    },
    MediaChunk {
        file_conv: [u8; 32],
        index: u32,
        bytes: Vec<u8>,
    },
    /// Original signed ciphertext keeps channel metadata and file-card
    /// provenance verifiable without importing the old device's secrets.
    SignedEvent {
        event: Event,
    },
    FileCard {
        conversation: [u8; 32],
        event_id: [u8; 32],
        from: String,
        wall_clock: u64,
        manifest: Vec<u8>,
    },
    /// Account identity for a conversation represented only by file cards.
    AccountContact {
        account_id: String,
    },
    /// Retired V2 records: keep positional tags so existing backups remain readable.
    /// New exports omit these records and imports ignore their filtering metadata.
    DmLanguageProfile {
        payload: Vec<u8>,
    },
    DmLanguageDecision {
        id: [u8; 32],
        hidden: bool,
    },
    /// Schema 3 appends this tag; earlier positional tags stay unchanged.
    PeerTrustCard {
        card: crate::node::TrustCard,
    },
    /// Schema 4 appends account-owned Favorites without changing earlier tags.
    Favorite {
        payload: Vec<u8>,
    },
    FavoriteMediaDescriptor {
        file_conv: [u8; 32],
        size: u64,
        sha256: [u8; 32],
    },
    FavoriteMediaChunk {
        file_conv: [u8; 32],
        index: u32,
        bytes: Vec<u8>,
    },
    /// Schema 5 appends authoritative metadata after the frozen Favorites tags.
    AccountCreation {
        created_at_ms: Option<u64>,
    },
    AccountProfile {
        profile: crate::node::AccountProfile,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SnapshotError {
    UnsupportedVersion,
    InvalidAccount,
    InvalidEncoding,
    SizeLimit,
    DuplicateRecord,
    RecordCount,
    InvalidMedia,
    InvalidEvent,
    InvalidFileCard,
    Conflict,
    Io,
}

impl std::fmt::Display for SnapshotError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedVersion => f.write_str("unsupported snapshot version"),
            Self::InvalidAccount => f.write_str("snapshot account identity is invalid"),
            Self::InvalidEncoding => f.write_str("snapshot encoding is invalid"),
            Self::SizeLimit => f.write_str("snapshot exceeds a size limit"),
            Self::DuplicateRecord => f.write_str("snapshot contains a duplicate record"),
            Self::RecordCount => f.write_str("snapshot record count does not match"),
            Self::InvalidMedia => f.write_str("snapshot media is invalid or incomplete"),
            Self::InvalidEvent => f.write_str("snapshot signed event is invalid"),
            Self::InvalidFileCard => f.write_str("snapshot file card is invalid"),
            Self::Conflict => f.write_str("recovery destination already exists"),
            Self::Io => f.write_str("snapshot I/O error"),
        }
    }
}

fn codec() -> impl Options {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .reject_trailing_bytes()
}

pub struct SnapshotWriter<W: Write> {
    destination: W,
    validator: SnapshotValidator,
}

impl<W: Write> SnapshotWriter<W> {
    pub fn new(mut destination: W, header: &SnapshotHeaderV1) -> Result<Self, SnapshotError> {
        let validator = SnapshotValidator::new(header.clone())?;
        let encoded_header = codec()
            .serialize(header)
            .map_err(|_| SnapshotError::InvalidEncoding)?;
        if encoded_header.len() > MAX_HEADER_BYTES {
            return Err(SnapshotError::SizeLimit);
        }
        destination
            .write_all(MAGIC)
            .map_err(|_| SnapshotError::Io)?;
        destination
            .write_all(&(encoded_header.len() as u32).to_be_bytes())
            .map_err(|_| SnapshotError::Io)?;
        destination
            .write_all(&encoded_header)
            .map_err(|_| SnapshotError::Io)?;
        Ok(Self {
            destination,
            validator,
        })
    }

    pub fn push(&mut self, record: &SnapshotRecordV1) -> Result<(), SnapshotError> {
        self.validator.accept(record)?;
        let mut encoded = codec()
            .serialize(record)
            .map_err(|_| SnapshotError::InvalidEncoding)?;
        // Explicit legacy writers retain each original schema-4 family's tags.
        // Production exports always use SNAPSHOT_SCHEMA_VERSION.
        if self.validator.schema_version == 4 {
            let tag: Option<u32> = match record {
                SnapshotRecordV1::AccountCreation { .. } => Some(12),
                SnapshotRecordV1::AccountProfile { .. } => Some(13),
                _ => None,
            };
            if let Some(tag) = tag {
                encoded[..4].copy_from_slice(&tag.to_le_bytes());
            }
        }
        if encoded.len() > MAX_ENCODED_RECORD_BYTES {
            return Err(SnapshotError::SizeLimit);
        }
        self.destination
            .write_all(&(encoded.len() as u32).to_be_bytes())
            .map_err(|_| SnapshotError::Io)?;
        self.destination
            .write_all(&encoded)
            .map_err(|_| SnapshotError::Io)?;
        Ok(())
    }

    pub fn finish(self) -> Result<(), SnapshotError> {
        self.validator.finish()
    }
}

/// Write a validated sequence of records without a whole-history allocation.
pub fn write_snapshot<W: Write>(
    destination: W,
    header: &SnapshotHeaderV1,
    records: impl IntoIterator<Item = SnapshotRecordV1>,
) -> Result<(), SnapshotError> {
    let mut writer = SnapshotWriter::new(destination, header)?;
    for record in records {
        writer.push(&record)?;
    }
    writer.finish()
}

/// Decode into a private staging callback; the caller must publish only after
/// this function returns `Ok` and the outer archive seal has also been checked.
pub fn read_snapshot<R: Read>(
    mut source: R,
    mut on_record: impl FnMut(SnapshotRecordV1) -> Result<(), SnapshotError>,
) -> Result<SnapshotHeaderV1, SnapshotError> {
    let mut magic = [0u8; 8];
    source
        .read_exact(&mut magic)
        .map_err(|_| SnapshotError::InvalidEncoding)?;
    if &magic != MAGIC {
        return Err(SnapshotError::InvalidEncoding);
    }
    let mut len = [0u8; 4];
    source
        .read_exact(&mut len)
        .map_err(|_| SnapshotError::InvalidEncoding)?;
    let header_len = u32::from_be_bytes(len) as usize;
    if header_len > MAX_HEADER_BYTES {
        return Err(SnapshotError::SizeLimit);
    }
    let mut bytes = vec![0u8; header_len];
    source
        .read_exact(&mut bytes)
        .map_err(|_| SnapshotError::InvalidEncoding)?;
    let header: SnapshotHeaderV1 = codec()
        .with_limit(MAX_HEADER_BYTES as u64)
        .deserialize(&bytes)
        .map_err(|_| SnapshotError::InvalidEncoding)?;
    let mut validator = SnapshotValidator::new(header.clone())?;
    for _ in 0..header.record_count {
        source
            .read_exact(&mut len)
            .map_err(|_| SnapshotError::InvalidEncoding)?;
        let record_len = u32::from_be_bytes(len) as usize;
        if record_len > MAX_ENCODED_RECORD_BYTES {
            return Err(SnapshotError::SizeLimit);
        }
        let mut bytes = vec![0u8; record_len];
        source
            .read_exact(&mut bytes)
            .map_err(|_| SnapshotError::InvalidEncoding)?;
        let record = decode_record(header.schema_version, &bytes)?;
        validator.accept(&record)?;
        on_record(record)?;
    }
    validator.finish()?;
    let mut trailing = [0u8; 1];
    if source.read(&mut trailing).map_err(|_| SnapshotError::Io)? != 0 {
        return Err(SnapshotError::InvalidEncoding);
    }
    Ok(header)
}

// Original development schema 4 assigned tags 12/13 twice. The exact encodings
// are disjoint: AccountCreation is 5 or 13 bytes including its tag, while a valid
// Favorite contains a much larger bounded journal record. Favorite descriptors
// are exactly 76 bytes; even an empty signed AccountProfile needs 126 bytes.
// Decode only the selected exact shape, never retry a failed semantic validation.
fn decode_record(schema: u16, bytes: &[u8]) -> Result<SnapshotRecordV1, SnapshotError> {
    let tag = bytes.get(..4).ok_or(SnapshotError::InvalidEncoding)?;
    let tag = u32::from_le_bytes(tag.try_into().map_err(|_| SnapshotError::InvalidEncoding)?);
    if schema == 4 {
        match tag {
            12 if matches!(bytes.len(), 5 | 13) => {
                let (_, created_at_ms): (u32, Option<u64>) = codec()
                    .with_limit(MAX_ENCODED_RECORD_BYTES as u64)
                    .deserialize(bytes)
                    .map_err(|_| SnapshotError::InvalidEncoding)?;
                return Ok(SnapshotRecordV1::AccountCreation { created_at_ms });
            }
            13 if bytes.len() != 76 => {
                let (_, profile): (u32, crate::node::AccountProfile) = codec()
                    .with_limit(MAX_ENCODED_RECORD_BYTES as u64)
                    .deserialize(bytes)
                    .map_err(|_| SnapshotError::InvalidEncoding)?;
                return Ok(SnapshotRecordV1::AccountProfile { profile });
            }
            15.. => return Err(SnapshotError::UnsupportedVersion),
            _ => {}
        }
    }
    codec()
        .with_limit(MAX_ENCODED_RECORD_BYTES as u64)
        .deserialize(bytes)
        .map_err(|_| SnapshotError::InvalidEncoding)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LegacyFourFamily {
    Favorites,
    Profiles,
}

impl std::error::Error for SnapshotError {}

#[derive(Debug)]
pub struct SnapshotValidator {
    own_account_id: String,
    schema_version: u16,
    legacy_four_family: Option<LegacyFourFamily>,
    language_profiles: HashSet<String>,
    language_decisions: HashSet<[u8; 32]>,
    expected: u64,
    accepted: u64,
    sent_ids: HashSet<([u8; 32], [u8; 32])>,
    received_ids: HashSet<[u8; 32]>,
    recalls: HashSet<([u8; 32], [u8; 32])>,
    profiles: HashSet<String>,
    trust_cards: HashSet<String>,
    current_profiles: HashSet<String>,
    current_profile_bytes: usize,
    account_created_at_ms: Option<Option<u64>>,
    own_profile_created_at_ms: Option<Option<u64>>,
    trust_card_bytes: usize,
    account_contacts: HashSet<String>,
    event_ids: HashMap<[u8; 32], SignedEventInfo>,
    file_cards: HashMap<[u8; 32], FileCardInfo>,
    media: HashMap<(bool, [u8; 32]), MediaProgress>,
    favorites: HashSet<String>,
    favorite_files: HashMap<[u8; 32], u64>,
    created_at_ms: u64,
    total_media_bytes: u64,
}

#[derive(Debug)]
struct MediaProgress {
    size: u64,
    sha256: [u8; 32],
    written: u64,
    next_index: u32,
    hasher: Sha256,
}

#[derive(Debug)]
struct SignedEventInfo {
    kind: EventKind,
    conversation: [u8; 32],
    from: String,
    wall_clock: u64,
}

#[derive(Debug)]
struct FileCardInfo {
    conversation: [u8; 32],
    file_conv: [u8; 32],
    from: String,
    wall_clock: u64,
}

impl SnapshotValidator {
    pub fn new(header: SnapshotHeaderV1) -> Result<Self, SnapshotError> {
        if !(1..=SNAPSHOT_SCHEMA_VERSION).contains(&header.schema_version) {
            return Err(SnapshotError::UnsupportedVersion);
        }
        if header.record_count > MAX_RECORDS {
            return Err(SnapshotError::SizeLimit);
        }
        if Account::from_secret_bytes(header.account_secret).account_id() != header.account_id {
            return Err(SnapshotError::InvalidAccount);
        }
        if header.source_device_ed25519_pubs.is_empty()
            || header.source_device_ed25519_pubs.len() > 64
            || header
                .source_device_ed25519_pubs
                .iter()
                .copied()
                .collect::<HashSet<_>>()
                .len()
                != header.source_device_ed25519_pubs.len()
        {
            return Err(SnapshotError::InvalidAccount);
        }
        Ok(Self {
            own_account_id: header.account_id.clone(),
            schema_version: header.schema_version,
            legacy_four_family: None,
            language_profiles: HashSet::new(),
            language_decisions: HashSet::new(),
            expected: header.record_count,
            accepted: 0,
            sent_ids: HashSet::new(),
            received_ids: HashSet::new(),
            recalls: HashSet::new(),
            profiles: HashSet::new(),
            trust_cards: HashSet::new(),
            current_profiles: HashSet::new(),
            current_profile_bytes: 0,
            account_created_at_ms: None,
            own_profile_created_at_ms: None,
            trust_card_bytes: 0,
            account_contacts: HashSet::new(),
            event_ids: HashMap::new(),
            file_cards: HashMap::new(),
            media: HashMap::new(),
            favorites: HashSet::new(),
            favorite_files: HashMap::new(),
            created_at_ms: header.created_at_ms,
            total_media_bytes: 0,
        })
    }

    pub fn accept(&mut self, record: &SnapshotRecordV1) -> Result<(), SnapshotError> {
        if self.accepted >= self.expected {
            return Err(SnapshotError::RecordCount);
        }
        if self.schema_version == 4 {
            let family = match record {
                SnapshotRecordV1::Favorite { .. }
                | SnapshotRecordV1::FavoriteMediaDescriptor { .. }
                | SnapshotRecordV1::FavoriteMediaChunk { .. } => Some(LegacyFourFamily::Favorites),
                SnapshotRecordV1::AccountCreation { .. }
                | SnapshotRecordV1::AccountProfile { .. } => Some(LegacyFourFamily::Profiles),
                _ => None,
            };
            if let Some(family) = family {
                if self.legacy_four_family.is_some_and(|old| old != family) {
                    return Err(SnapshotError::InvalidEncoding);
                }
                self.legacy_four_family = Some(family);
            }
        }
        match record {
            SnapshotRecordV1::SentDm {
                conversation,
                message_id,
                plaintext,
                ..
            } => {
                if plaintext.len() > MAX_RECORD_BYTES {
                    return Err(SnapshotError::SizeLimit);
                }
                if !self.sent_ids.insert((*conversation, *message_id)) {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
            SnapshotRecordV1::ReceivedDm {
                event_id,
                from,
                plaintext,
                ..
            } => {
                if plaintext.len() > MAX_RECORD_BYTES || from.len() > 256 {
                    return Err(SnapshotError::SizeLimit);
                }
                if !self.received_ids.insert(*event_id) {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
            SnapshotRecordV1::Recall {
                conversation,
                target,
                ..
            } => {
                if !self.recalls.insert((*conversation, *target)) {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
            SnapshotRecordV1::Profile {
                account_id, avatar, ..
            } => {
                if account_id.len() > 256
                    || avatar
                        .as_ref()
                        .is_some_and(|bytes| bytes.len() > MAX_RECORD_BYTES)
                {
                    return Err(SnapshotError::SizeLimit);
                }
                if !self.profiles.insert(account_id.clone()) {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
            SnapshotRecordV1::AccountCreation { created_at_ms } => {
                if self.schema_version < 4 {
                    return Err(SnapshotError::UnsupportedVersion);
                }
                if crate::identity::account::Account::from_secret_bytes_with_created_at(
                    [0; 32],
                    *created_at_ms,
                )
                .is_none()
                {
                    return Err(SnapshotError::InvalidAccount);
                }
                if self.account_created_at_ms.is_some() {
                    return Err(SnapshotError::DuplicateRecord);
                }
                self.account_created_at_ms = Some(*created_at_ms);
            }
            SnapshotRecordV1::AccountProfile { profile } => {
                if self.schema_version < 4 {
                    return Err(SnapshotError::UnsupportedVersion);
                }
                if !profile.verify() {
                    return Err(SnapshotError::InvalidEncoding);
                }
                if profile.account_id() == self.own_account_id {
                    self.own_profile_created_at_ms = Some(profile.created_at_ms);
                }
                if !self.current_profiles.insert(profile.account_id()) {
                    return Err(SnapshotError::DuplicateRecord);
                }
                self.current_profile_bytes = self
                    .current_profile_bytes
                    .saturating_add(profile.encode().len());
                if self.current_profiles.len() > 1024
                    || self.current_profile_bytes > 32 * 1024 * 1024
                {
                    return Err(SnapshotError::SizeLimit);
                }
            }
            SnapshotRecordV1::PeerTrustCard { card } => {
                if self.schema_version < 3 {
                    return Err(SnapshotError::UnsupportedVersion);
                }
                if !card.verify() {
                    return Err(SnapshotError::InvalidEncoding);
                }
                if !self.trust_cards.insert(card.account_id()) {
                    return Err(SnapshotError::DuplicateRecord);
                }
                self.trust_card_bytes = self.trust_card_bytes.saturating_add(card.encode().len());
                if self.trust_cards.len() > 1024 || self.trust_card_bytes > 32 * 1024 * 1024 {
                    return Err(SnapshotError::SizeLimit);
                }
            }
            SnapshotRecordV1::DmLanguageProfile { payload } => {
                if self.schema_version < 2 {
                    return Err(SnapshotError::UnsupportedVersion);
                }
                let profile = crate::node::profile::ProfilePayload::decode(payload)
                    .filter(|profile| profile.language_supported && profile.verify())
                    .ok_or(SnapshotError::InvalidEncoding)?;
                if !self.language_profiles.insert(profile.account_id()) {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
            SnapshotRecordV1::DmLanguageDecision { id, .. } => {
                if self.schema_version < 2 {
                    return Err(SnapshotError::UnsupportedVersion);
                }
                if !self.language_decisions.insert(*id) {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
            SnapshotRecordV1::AccountContact { account_id } => {
                if account_id == &self.own_account_id
                    || account_id.len() != 32
                    || !account_id.bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Err(SnapshotError::InvalidAccount);
                }
                if !self.account_contacts.insert(account_id.clone()) {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
            SnapshotRecordV1::Favorite { payload } => {
                if self.schema_version < 4 {
                    return Err(SnapshotError::UnsupportedVersion);
                }
                let favorite = crate::node::favorites::recovery::validate(
                    payload,
                    &self.own_account_id,
                    self.created_at_ms,
                )?;
                if !self.favorites.insert(favorite.id) {
                    return Err(SnapshotError::DuplicateRecord);
                }
                if self.favorites.len() > crate::node::favorites::recovery::PORTABLE_RECORD_LIMIT {
                    return Err(SnapshotError::SizeLimit);
                }
                for file in favorite.files {
                    let mut conv = [0; 32];
                    hex::decode_to_slice(file.file_conv, &mut conv)
                        .map_err(|_| SnapshotError::InvalidEncoding)?;
                    if self
                        .favorite_files
                        .insert(conv, file.size)
                        .is_some_and(|old| old != file.size)
                    {
                        return Err(SnapshotError::InvalidMedia);
                    }
                }
            }
            SnapshotRecordV1::MediaDescriptor {
                file_conv,
                size,
                sha256,
            }
            | SnapshotRecordV1::FavoriteMediaDescriptor {
                file_conv,
                size,
                sha256,
            } => {
                let favorite = matches!(record, SnapshotRecordV1::FavoriteMediaDescriptor { .. });
                if favorite && self.schema_version < 4 {
                    return Err(SnapshotError::UnsupportedVersion);
                }
                let limit = if favorite {
                    crate::node::favorites::recovery::PORTABLE_FILE_LIMIT
                } else {
                    MAX_MEDIA_BYTES
                };
                self.total_media_bytes = self
                    .total_media_bytes
                    .checked_add(*size)
                    .ok_or(SnapshotError::SizeLimit)?;
                if *size > limit || self.total_media_bytes > MAX_TOTAL_MEDIA_BYTES {
                    return Err(SnapshotError::SizeLimit);
                }
                if self
                    .media
                    .insert(
                        (favorite, *file_conv),
                        MediaProgress {
                            size: *size,
                            sha256: *sha256,
                            written: 0,
                            next_index: 0,
                            hasher: Sha256::new(),
                        },
                    )
                    .is_some()
                {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
            SnapshotRecordV1::MediaChunk {
                file_conv,
                index,
                bytes,
            }
            | SnapshotRecordV1::FavoriteMediaChunk {
                file_conv,
                index,
                bytes,
            } => {
                let favorite = matches!(record, SnapshotRecordV1::FavoriteMediaChunk { .. });
                if favorite && self.schema_version < 4 {
                    return Err(SnapshotError::UnsupportedVersion);
                }
                let progress = self
                    .media
                    .get_mut(&(favorite, *file_conv))
                    .ok_or(SnapshotError::InvalidMedia)?;
                if *index != progress.next_index
                    || bytes.is_empty()
                    || bytes.len() > MAX_MEDIA_CHUNK_BYTES
                {
                    return Err(SnapshotError::InvalidMedia);
                }
                progress.written = progress
                    .written
                    .checked_add(bytes.len() as u64)
                    .ok_or(SnapshotError::SizeLimit)?;
                if progress.written > progress.size {
                    return Err(SnapshotError::InvalidMedia);
                }
                progress.next_index = progress
                    .next_index
                    .checked_add(1)
                    .ok_or(SnapshotError::SizeLimit)?;
                progress.hasher.update(bytes);
            }
            SnapshotRecordV1::SignedEvent { event } => {
                if event.ciphertext.len() > MAX_RECORD_BYTES || event.parents.len() > 1024 {
                    return Err(SnapshotError::SizeLimit);
                }
                if !event.verify_integrity() || !event.verify_signature() || !event.is_canonical() {
                    return Err(SnapshotError::InvalidEvent);
                }
                if self
                    .event_ids
                    .insert(
                        *event.id.as_bytes(),
                        SignedEventInfo {
                            kind: event.kind,
                            conversation: *event.conversation_id.as_bytes(),
                            from: event.author.user_id(),
                            wall_clock: event.wall_clock,
                        },
                    )
                    .is_some()
                {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
            SnapshotRecordV1::FileCard {
                conversation,
                event_id,
                from,
                wall_clock,
                manifest,
            } => {
                if manifest.len() > MAX_RECORD_BYTES || from.len() > 256 {
                    return Err(SnapshotError::SizeLimit);
                }
                let card =
                    crate::file::decode_manifest(manifest).ok_or(SnapshotError::InvalidFileCard)?;
                if self
                    .file_cards
                    .insert(
                        *event_id,
                        FileCardInfo {
                            conversation: *conversation,
                            file_conv: *card.file_conv().as_bytes(),
                            from: from.clone(),
                            wall_clock: *wall_clock,
                        },
                    )
                    .is_some()
                {
                    return Err(SnapshotError::DuplicateRecord);
                }
            }
        }
        self.accepted += 1;
        Ok(())
    }

    pub fn finish(self) -> Result<(), SnapshotError> {
        if self.accepted != self.expected {
            return Err(SnapshotError::RecordCount);
        }
        if (self.schema_version >= 5 || self.legacy_four_family == Some(LegacyFourFamily::Profiles))
            && self.account_created_at_ms.is_none()
        {
            return Err(SnapshotError::InvalidAccount);
        }
        if self.own_profile_created_at_ms.is_some()
            && self.own_profile_created_at_ms != self.account_created_at_ms
        {
            return Err(SnapshotError::InvalidAccount);
        }
        for ((favorite, file_conv), progress) in &self.media {
            if (*favorite && self.favorite_files.get(file_conv) != Some(&progress.size))
                || progress.written != progress.size
                || progress.hasher.clone().finalize().as_slice() != progress.sha256
            {
                return Err(SnapshotError::InvalidMedia);
            }
        }
        if self.file_cards.iter().any(|(id, card)| {
            let Some(event) = self.event_ids.get(id) else {
                return true;
            };
            event.kind != EventKind::FileManifest
                || event.from != card.from
                || event.wall_clock != card.wall_clock
                || event.conversation != card.conversation
                || event.conversation == card.file_conv
                || card.conversation == card.file_conv
        }) {
            return Err(SnapshotError::InvalidFileCard);
        }
        for account_id in &self.account_contacts {
            let conversation = crate::node::conversation::account_conversation_id(
                &self.own_account_id,
                account_id,
            );
            if !self
                .file_cards
                .values()
                .any(|card| card.conversation == *conversation.as_bytes())
            {
                return Err(SnapshotError::InvalidFileCard);
            }
        }
        Ok(())
    }
}
