//! Author-signed logical file-message identity, separate from exact device offers.
//! Supplemental metadata leaves deployed manifest and account-card formats intact.

use super::{Node, NodeError, conversation::account_conversation_id, received_log::ReceivedEntry};
use crate::eventlog::{Author, ConversationId, Event, EventId, EventKind, LogError};
use crate::identity::account::DeviceCertificate;
use crate::storage::record_log::EncryptedRecordLog;
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
#[cfg(test)]
use std::collections::HashMap;
use std::{collections::HashSet, path::Path};

mod indexed;
pub(crate) use indexed::IndexedFileMessageIdentities;

const MAGIC: &[u8; 6] = b"MTFMID";
const PREFIX: &[u8] =
    b"mesh-talk-file-consent-control-v1\0\0\0\0\0\0\0\0\xffmesh-talk-file-message-identity-v1\0";
const MAX_ALIASES: usize = 64;
const MAX_IDENTITIES: usize = 20_000;
pub(crate) const MAX_INDEXED_ALIASES: usize = 100_000;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Identity {
    source: DeviceCertificate,
    recipient_account: String,
    file: ConversationId,
    logical: EventId,
    manifest_digest: [u8; 32],
    aliases: Vec<EventId>,
}

impl Identity {
    fn host(&self) -> ConversationId {
        account_conversation_id(&self.source.account_id(), &self.recipient_account)
    }
    fn matches(&self, row: &ReceivedEntry) -> bool {
        self.host() == row.conversation && self.matches_content(row)
    }
    fn matches_content(&self, row: &ReceivedEntry) -> bool {
        Author::from_ed25519(self.source.device_ed25519_pub).user_id() == row.from
            && self.manifest_digest == digest(&row.plaintext)
            && self.aliases.contains(&row.event_id)
    }
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
fn scope(logical: EventId) -> ConversationId {
    let mut hasher = Sha256::new();
    hasher.update(PREFIX);
    hasher.update(logical.as_bytes());
    ConversationId::new(hasher.finalize().into())
}

fn decode(bundle: &Event) -> Option<Identity> {
    let body = bundle.ciphertext.strip_prefix(PREFIX)?;
    if body.len() > 8192
        || !bundle.is_canonical()
        || !bundle.verify_integrity()
        || !bundle.verify_signature()
    {
        return None;
    }
    let identity: Identity = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(8192)
        .reject_trailing_bytes()
        .deserialize(body)
        .ok()?;
    if bundle.kind != EventKind::Message
        || bundle.seq != 1
        || !bundle.parents.is_empty()
        || bundle.conversation_id != scope(identity.logical)
        || !identity.source.verify()
        || bundle.author.ed25519_pub() != &identity.source.device_ed25519_pub
        || identity.recipient_account.len() != 32
        || !identity
            .recipient_account
            .bytes()
            .all(|b| b.is_ascii_hexdigit())
        || identity.aliases.is_empty()
        || identity.aliases.len() > MAX_ALIASES
        || identity.aliases.first() != Some(&identity.logical)
        || identity
            .aliases
            .iter()
            .any(|alias| alias.as_bytes() == &[0; 32])
        || identity
            .aliases
            .iter()
            .copied()
            .collect::<HashSet<_>>()
            .len()
            != identity.aliases.len()
    {
        return None;
    }
    Some(identity)
}

pub(super) fn bundle_covers(bundle: &Event, file: ConversationId, manifest: EventId) -> bool {
    decode(bundle)
        .is_some_and(|identity| identity.file == file && identity.aliases.contains(&manifest))
}

pub(super) fn bundle_references(bundle: &Event) -> Vec<(ConversationId, EventId)> {
    decode(bundle).map_or_else(Vec::new, |identity| {
        identity
            .aliases
            .into_iter()
            .map(|alias| (identity.file, alias))
            .collect()
    })
}

pub(super) fn bundle_accounts(bundle: &Event) -> Option<(String, String)> {
    let identity = decode(bundle)?;
    Some((identity.source.account_id(), identity.recipient_account))
}

pub(super) fn bundle_matches_row(bundle: &Event, row: &ReceivedEntry) -> bool {
    decode(bundle).is_some_and(|identity| {
        identity.matches(row)
            && super::files::validated_manifest(&row.plaintext)
                .is_some_and(|manifest| manifest.file_conv() == identity.file)
    })
}

pub(super) fn bundle_matches_source_row(bundle: &Event, row: &ReceivedEntry) -> bool {
    decode(bundle).is_some_and(|identity| {
        identity.matches_content(row)
            && super::files::validated_manifest(&row.plaintext)
                .is_some_and(|manifest| manifest.file_conv() == identity.file)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct FileMessageIdentityBinding {
    pub(crate) host: ConversationId,
    pub(crate) file: ConversationId,
    pub(crate) logical: EventId,
    pub(crate) source: String,
    pub(crate) wall_clock: u64,
    pub(crate) manifest_digest: [u8; 32],
}

pub(crate) fn validated_snapshot_binding(
    bundle: &Event,
) -> Option<(FileMessageIdentityBinding, Vec<EventId>)> {
    let identity = decode(bundle)?;
    let binding = FileMessageIdentityBinding {
        host: identity.host(),
        file: identity.file,
        logical: identity.logical,
        source: Author::from_ed25519(identity.source.device_ed25519_pub).user_id(),
        wall_clock: bundle.wall_clock,
        manifest_digest: identity.manifest_digest,
    };
    Some((binding, identity.aliases))
}

pub(crate) fn is_bundle(bundle: &Event) -> bool {
    bundle.ciphertext.starts_with(PREFIX)
}

#[derive(Debug)]
pub(in crate::node) enum FileMessageIdentityImportError {
    MissingProof,
    Invalid(NodeError),
    Persistence(LogError),
}

/// Only explicit peer validation failures may retire an opaque envelope. Local
/// storage, decoding and capacity failures remain retryable across backends.
fn classify_import_failure(error: NodeError) -> FileMessageIdentityImportError {
    match error {
        NodeError::Log(error) if !error.is_peer_rejection() => {
            FileMessageIdentityImportError::Persistence(error)
        }
        error => FileMessageIdentityImportError::Invalid(error),
    }
}

#[cfg(test)]
pub(in crate::node) struct FileMessageIdentities {
    log: EncryptedRecordLog<Event>,
    bundles: HashMap<(ConversationId, EventId), (Event, Identity)>,
    aliases: HashMap<(ConversationId, EventId), EventId>,
    by_manifest: HashMap<EventId, (ConversationId, EventId)>,
}

#[cfg(test)]
impl FileMessageIdentities {
    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (log, events) = EncryptedRecordLog::<Event>::open(path, password, MAGIC)?;
        let mut store = Self {
            log,
            bundles: HashMap::new(),
            aliases: HashMap::new(),
            by_manifest: HashMap::new(),
        };
        for event in events {
            let identity = decode(&event)
                .ok_or_else(|| LogError::CorruptFile("invalid logical file identity".into()))?;
            store.check(&event, &identity)?;
            store.index(event, identity);
        }
        Ok(store)
    }
    fn check(&self, event: &Event, identity: &Identity) -> Result<(), LogError> {
        let host = identity.host();
        if let Some((old, _)) = self.bundles.get(&(host, identity.logical)) {
            return if old == event {
                Ok(())
            } else {
                Err(LogError::AdmissionRejected(
                    "conflicting logical file identity".into(),
                ))
            };
        }
        if self.bundles.len() >= MAX_IDENTITIES
            || self.aliases.len() + identity.aliases.len() > MAX_INDEXED_ALIASES
        {
            return Err(std::io::Error::other("logical file identity capacity exhausted").into());
        }
        if identity.aliases.iter().any(|id| {
            self.by_manifest
                .get(id)
                .is_some_and(|binding| *binding != (host, identity.logical))
        }) {
            return Err(LogError::AdmissionRejected(
                "conflicting logical file identity scope".into(),
            ));
        }
        if identity.aliases.iter().any(|id| {
            self.aliases
                .get(&(host, *id))
                .is_some_and(|logical| *logical != identity.logical)
        }) {
            return Err(LogError::AdmissionRejected(
                "conflicting manifest alias".into(),
            ));
        }
        Ok(())
    }
    fn index(&mut self, event: Event, identity: Identity) {
        for alias in &identity.aliases {
            self.by_manifest
                .insert(*alias, (identity.host(), identity.logical));
            self.aliases
                .insert((identity.host(), *alias), identity.logical);
        }
        self.bundles
            .insert((identity.host(), identity.logical), (event, identity));
    }
    fn insert(&mut self, event: Event, identity: Identity) -> Result<bool, LogError> {
        self.check(&event, &identity)?;
        if self
            .bundles
            .contains_key(&(identity.host(), identity.logical))
        {
            return Ok(false);
        }
        self.log.append_durable(&event)?;
        self.index(event, identity);
        Ok(true)
    }
    pub(super) fn logical(&self, host: ConversationId, event: EventId) -> EventId {
        self.aliases.get(&(host, event)).copied().unwrap_or(event)
    }
    pub(super) fn row_logical(&self, row: &ReceivedEntry) -> EventId {
        let logical = self.logical(row.conversation, row.event_id);
        self.bundles
            .get(&(row.conversation, logical))
            .filter(|(_, identity)| identity.matches(row))
            .map_or(row.event_id, |_| logical)
    }
    pub(super) fn snapshot(&self) -> Vec<Event> {
        self.bundles
            .values()
            .map(|(event, _)| event.clone())
            .collect()
    }
}

impl Node {
    pub(in crate::node) fn has_file_message_identity(
        &self,
        host: ConversationId,
        manifest: EventId,
    ) -> Result<bool, LogError> {
        self.file_message_identities
            .lock()
            .expect("file identity lock")
            .contains(host, manifest)
    }
    pub(in crate::node) fn file_message_logical_id(
        &self,
        host: ConversationId,
        event: EventId,
    ) -> Result<EventId, LogError> {
        self.file_message_identities
            .lock()
            .expect("file identity lock")
            .logical(host, event)
    }
    pub(in crate::node) fn file_message_row_logical_id(
        &self,
        row: &ReceivedEntry,
    ) -> Result<EventId, LogError> {
        self.file_message_identities
            .lock()
            .expect("file identity lock")
            .row_logical(row)
    }
    pub(in crate::node) fn file_message_row_suppressed(
        &self,
        row: &ReceivedEntry,
    ) -> Result<bool, LogError> {
        let identities = self
            .file_message_identities
            .lock()
            .expect("file identity lock");
        let suppression = self
            .account_suppression
            .lock()
            .expect("account suppression lock");
        identities.row_suppressed(row, &suppression)
    }
    /// Retained source cards repair old per-device copies without re-sending artwork.
    pub(in crate::node) fn file_message_identity_bundle(
        &self,
        manifest: EventId,
    ) -> Result<Option<Event>, LogError> {
        if let Some(bundle) = self
            .file_message_identities
            .lock()
            .expect("file identity lock")
            .bundle(manifest)?
        {
            return Ok(Some(bundle));
        }
        let Some(card) = ({
            let delivery = self.delivery.lock().expect("delivery lock");
            delivery
                .file_card(manifest)
                .filter(|card| card.completion_binding.is_none())
                .or_else(|| {
                    delivery.file_cards().find(|card| {
                        card.completion_binding.is_none()
                            && card
                                .destinations
                                .iter()
                                .any(|dest| dest.binding.event_id == manifest)
                    })
                })
                .cloned()
        }) else {
            return Ok(None);
        };
        if card.destinations.is_empty() || card.destinations.len() > MAX_ALIASES {
            return Ok(None);
        }
        let Some(recipient) = card
            .destinations
            .first()
            .and_then(|dest| dest.binding.account.clone())
        else {
            return Ok(None);
        };
        if card
            .destinations
            .iter()
            .any(|dest| dest.binding.account.as_deref() != Some(recipient.as_str()))
        {
            return Ok(None);
        }
        // The stable device key can survive account adoption, but a legacy
        // outgoing card retains its original account conversation. Never mint
        // its supplemental identity with the newly loaded account certificate.
        if card.conversation != account_conversation_id(&self.account_id(), &recipient) {
            return Ok(None);
        }
        let Some(row) = self
            .received_files
            .lock()
            .expect("files lock")
            .entry_in(&card.conversation, card.id)?
        else {
            return Ok(None);
        };
        if row.from != self.user_id() {
            return Ok(None);
        }
        let aliases: Vec<_> = card
            .destinations
            .iter()
            .map(|dest| dest.binding.event_id)
            .collect();
        if aliases.first() != Some(&card.id) {
            return Ok(None);
        }
        {
            let log = self.log.lock().expect("log lock");
            for dest in &card.destinations {
                if log.get(&dest.binding.event_id)?.is_none_or(|event| {
                    event.author.ed25519_pub() != &self.identity.public().ed25519_pub
                        || event.kind != EventKind::FileManifest
                        || event.wall_clock != card.wall_clock
                        || event.conversation_id
                            != super::conversation::dm_conversation_id(
                                &self.identity.public(),
                                &dest.binding.device,
                            )
                        || !event.verify_integrity()
                        || !event.verify_signature()
                }) {
                    return Ok(None);
                }
            }
        }
        let identity = Identity {
            source: self.account.certify(&self.identity.public().ed25519_pub),
            recipient_account: recipient,
            file: card.file_conversation,
            logical: card.id,
            manifest_digest: digest(&row.plaintext),
            aliases,
        };
        let mut body = PREFIX.to_vec();
        body.extend(
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(&identity)
                .map_err(|error| LogError::CorruptFile(error.to_string()))?,
        );
        let bundle = Event::new(
            &self.identity,
            scope(card.id),
            1,
            vec![],
            1,
            card.wall_clock,
            EventKind::Message,
            body,
        );
        self.file_message_identities
            .lock()
            .expect("file identity lock")
            .insert(bundle.clone(), identity)?;
        Ok(Some(bundle))
    }
    pub(in crate::node) fn import_file_message_identity(
        &self,
        bundle: Event,
    ) -> Result<(), NodeError> {
        self.import_file_message_identity_checked(bundle)
            .map_err(|error| match error {
                FileMessageIdentityImportError::MissingProof => NodeError::Authorization(
                    "logical file identity requires a retained signed manifest".into(),
                ),
                FileMessageIdentityImportError::Invalid(error) => error,
                FileMessageIdentityImportError::Persistence(error) => NodeError::Log(error),
            })
    }
    pub(in crate::node) fn import_file_message_identity_checked(
        &self,
        bundle: Event,
    ) -> Result<(), FileMessageIdentityImportError> {
        match self.import_file_message_identity_inner(bundle) {
            Ok(true) => Ok(()),
            Ok(false) => Err(FileMessageIdentityImportError::MissingProof),
            Err(error) => Err(classify_import_failure(error)),
        }
    }
    fn import_file_message_identity_inner(&self, bundle: Event) -> Result<bool, NodeError> {
        let identity = decode(&bundle)
            .ok_or_else(|| NodeError::Authorization("invalid logical file identity".into()))?;
        // Exact retained route/proof below also permits legitimate historical account
        // bindings during account adoption and portable recovery.
        let local_source = identity.source.device_ed25519_pub == self.identity.public().ed25519_pub
            || self
                .recovery_origins
                .iter()
                .any(|author| author.ed25519_pub() == &identity.source.device_ed25519_pub);
        let own = self.account_id();
        let mut matching_host = false;
        let mut matched = false;
        let log = self.log.lock().expect("log lock");
        let proofs = self.account_file_proofs.lock().expect("file proofs lock");
        for alias in &identity.aliases {
            let selected = match log.get(alias)? {
                Some(event) => Some(event),
                None => proofs.get(alias)?,
            };
            if let Some(proof) = selected {
                if proof.author.ed25519_pub() != &identity.source.device_ed25519_pub
                    || proof.kind != EventKind::FileManifest
                    || proof.wall_clock != bundle.wall_clock
                    || !proof.verify_integrity()
                    || !proof.verify_signature()
                {
                    return Err(NodeError::Authorization(
                        "logical file identity conflicts with signed manifest".into(),
                    ));
                }
                let row = {
                    let files = self.received_files.lock().expect("files lock");
                    if local_source {
                        files.entry(*alias)?
                    } else {
                        files.entry_in(&identity.host(), *alias)?
                    }
                };
                if let Some(row) = row {
                    if !(identity.matches(&row) || local_source && identity.matches_content(&row))
                        || super::files::validated_manifest(&row.plaintext)
                            .is_none_or(|manifest| manifest.file_conv() != identity.file)
                    {
                        return Err(NodeError::Authorization(
                            "logical file identity conflicts with manifest content".into(),
                        ));
                    }
                    matched = true;
                    matching_host |= identity.matches(&row);
                }
            }
        }
        if own != identity.source.account_id()
            && own != identity.recipient_account
            && !local_source
            && !matching_host
        {
            return Err(NodeError::Authorization(
                "logical file identity belongs to another account".into(),
            ));
        }
        if !matched {
            return Ok(false);
        }
        drop(proofs);
        drop(log);
        for alias in &identity.aliases {
            if self
                .sentlog
                .lock()
                .expect("sent log lock")
                .account_entry(&identity.host(), *alias.as_bytes())?
                .is_some()
                || self
                    .received
                    .lock()
                    .expect("received log lock")
                    .account_entry(&identity.host(), *alias)?
                    .is_some()
            {
                return Err(NodeError::Authorization(
                    "logical file identity conflicts with text message identity".into(),
                ));
            }
        }
        let erased = {
            let delivery = self.delivery.lock().expect("delivery lock");
            identity.aliases.iter().any(|id| {
                delivery.manifest_event_erased(*id) || delivery.file_erased(identity.host(), *id)
            })
        };
        let suppressed = {
            let mut suppression = self
                .account_suppression
                .lock()
                .expect("account suppression lock");
            let mut suppressed =
                erased || suppression.contains(identity.host(), identity.logical)?;
            for id in &identity.aliases {
                suppressed |= suppression.contains(identity.host(), *id)?;
            }
            if suppressed {
                // Tombstones precede publishing the canonical alias index.
                for id in &identity.aliases {
                    suppression
                        .suppress(identity.host(), *id)
                        .map_err(NodeError::Log)?;
                }
                suppression
                    .suppress(identity.host(), identity.logical)
                    .map_err(NodeError::Log)?;
            }
            suppressed
        };
        let host = identity.host();
        let logical = identity.logical;
        let file = identity.file;
        let fresh = self
            .file_message_identities
            .lock()
            .expect("file identity lock")
            .insert(bundle, identity)
            .map_err(NodeError::Log)?;
        if fresh {
            self.note_account_history_change();
        }
        if suppressed && let Err(error) = self.delete_message(host, logical, true) {
            // An older accepted deletion remains authoritative even when a late
            // replica cannot allocate another physical erasure tombstone.
            let delivery = self.delivery.lock().expect("delivery lock");
            self.reseed_live_file_book(&delivery)?;
            drop(delivery);
            self.remove_unreferenced_media(vec![file])?;
            return Err(error);
        }
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};

    #[test]
    fn local_media_cleanup_failure_remains_retryable_for_identity_import() {
        let f = Fixture::new();
        let media = f.dir.path().join("receiver/media");
        let retained = f.dir.path().join("receiver/media-before-failure");
        std::fs::rename(&media, &retained).unwrap();
        std::fs::write(&media, b"not a directory").unwrap();
        let result = f
            .receiver
            .remove_unreferenced_media(vec![ConversationId::new([240; 32])]);
        std::fs::remove_file(&media).unwrap();
        std::fs::rename(&retained, &media).unwrap();
        assert!(
            matches!(
                classify_import_failure(result.unwrap_err()),
                FileMessageIdentityImportError::Persistence(_)
            ),
            "local media cleanup must not acknowledge an opaque identity envelope as invalid"
        );
    }

    #[test]
    fn identity_import_keeps_local_failures_retryable_and_rejects_conflicts() {
        for error in [
            LogError::Io(std::io::Error::other("local read")),
            LogError::Database(Box::new(std::io::Error::other("database read"))),
            LogError::CorruptFile("local row".into()),
            LogError::StaleCursor,
            LogError::Serialization("local decode".into()),
            LogError::PageBudgetTooSmall { required: 1024 },
        ] {
            assert!(matches!(
                classify_import_failure(NodeError::Log(error)),
                FileMessageIdentityImportError::Persistence(_)
            ));
        }
        for error in [
            LogError::BadSignature,
            LogError::AdmissionRejected("conflicting alias".into()),
        ] {
            assert!(matches!(
                classify_import_failure(NodeError::Log(error)),
                FileMessageIdentityImportError::Invalid(_)
            ));
        }
        assert!(matches!(
            classify_import_failure(NodeError::Authorization("wrong account".into())),
            FileMessageIdentityImportError::Invalid(_)
        ));
    }

    fn signed(
        source: &DeviceIdentity,
        account: &Account,
        logical: u8,
        other: u8,
    ) -> (Event, Identity) {
        let identity = Identity {
            source: account.certify(&source.public().ed25519_pub),
            recipient_account: Account::generate().account_id(),
            file: ConversationId::new([4; 32]),
            logical: EventId::new([logical; 32]),
            manifest_digest: digest(b"identical reused sticker"),
            aliases: vec![EventId::new([logical; 32]), EventId::new([other; 32])],
        };
        (sign(source, &identity), identity)
    }
    fn sign(source: &DeviceIdentity, identity: &Identity) -> Event {
        let mut bytes = PREFIX.to_vec();
        bytes.extend(
            bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(identity)
                .unwrap(),
        );
        Event::new(
            source,
            scope(identity.logical),
            1,
            vec![],
            1,
            123,
            EventKind::Message,
            bytes,
        )
    }

    #[test]
    fn same_artwork_and_timestamp_preserve_distinct_logical_sends_and_survive_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identities.log");
        let source = DeviceIdentity::generate();
        let account = Account::generate();
        let (first, identity) = signed(&source, &account, 1, 2);
        let mut second_identity = identity.clone();
        second_identity.logical = EventId::new([3; 32]);
        second_identity.aliases = vec![second_identity.logical, EventId::new([5; 32])];
        let second = sign(&source, &second_identity);
        assert_eq!(first.wall_clock, second.wall_clock);
        let mut store = FileMessageIdentities::open(&path, "pw").unwrap();
        assert!(
            store
                .insert(first.clone(), decode(&first).unwrap())
                .unwrap()
        );
        assert!(
            !store
                .insert(first.clone(), decode(&first).unwrap())
                .unwrap()
        );
        store
            .insert(second.clone(), decode(&second).unwrap())
            .unwrap();
        let row = |id| ReceivedEntry {
            conversation: identity.host(),
            event_id: EventId::new([id; 32]),
            from: source.public().user_id(),
            wall_clock: 123,
            plaintext: b"identical reused sticker".to_vec(),
        };
        assert_eq!(store.row_logical(&row(2)), identity.logical);
        assert_eq!(store.row_logical(&row(5)), second_identity.logical);
        drop(store);
        let reopened = FileMessageIdentities::open(&path, "pw").unwrap();
        assert_eq!(reopened.row_logical(&row(2)), identity.logical);
        assert_eq!(reopened.row_logical(&row(5)), second_identity.logical);
        let mut foreign = row(2);
        foreign.from = DeviceIdentity::generate().public().user_id();
        assert_eq!(reopened.row_logical(&foreign), foreign.event_id);
    }

    #[test]
    fn forged_tampered_and_conflicting_alias_bundles_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let source = DeviceIdentity::generate();
        let account = Account::generate();
        let (first, identity) = signed(&source, &account, 1, 2);
        let mut store =
            FileMessageIdentities::open(&dir.path().join("identities.log"), "pw").unwrap();
        store
            .insert(first.clone(), decode(&first).unwrap())
            .unwrap();
        let mut tampered = first.clone();
        tampered.ciphertext.push(1);
        assert!(decode(&tampered).is_none());
        assert!(decode(&sign(&DeviceIdentity::generate(), &identity)).is_none());
        let mut conflict = identity;
        conflict.logical = EventId::new([3; 32]);
        conflict.aliases = vec![conflict.logical, EventId::new([2; 32])];
        let event = sign(&source, &conflict);
        assert!(
            store
                .insert(event.clone(), decode(&event).unwrap())
                .is_err()
        );
        assert_eq!(store.snapshot(), vec![first]);
    }

    fn open_node(dir: &Path, device: DeviceIdentity, account: &Account) -> std::sync::Arc<Node> {
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        Node::open_with_account(
            device,
            Account::from_secret_bytes(account.secret_bytes()),
            std::sync::Arc::new(std::sync::Mutex::new(crate::discovery::Roster::default())),
            dm,
            channel,
            file,
            &dir.join("events.log"),
            &dir.join("sent.log"),
            "pw",
        )
        .unwrap()
    }
    struct Fixture {
        dir: tempfile::TempDir,
        sender: std::sync::Arc<Node>,
        receiver: std::sync::Arc<Node>,
        other: std::sync::Arc<Node>,
        account: Account,
        path: std::path::PathBuf,
    }
    impl Fixture {
        fn new() -> Self {
            use base64::Engine;
            let dir = tempfile::tempdir().unwrap();
            let account = Account::generate();
            let sender = open_node(
                &dir.path().join("sender"),
                DeviceIdentity::generate(),
                &Account::generate(),
            );
            let receiver = open_node(
                &dir.path().join("receiver"),
                DeviceIdentity::generate(),
                &account,
            );
            let other = open_node(
                &dir.path().join("other"),
                DeviceIdentity::generate(),
                &account,
            );
            for recipient in [&receiver, &other] {
                sender.roster.lock().unwrap().update(
                    &crate::discovery::Announce::new_with_account(
                        &recipient.identity,
                        &recipient.account,
                        "recipient",
                        9,
                    ),
                    "127.0.0.1".parse().unwrap(),
                    &sender.user_id(),
                );
                recipient.roster.lock().unwrap().update(
                    &crate::discovery::Announce::new_with_account(
                        &sender.identity,
                        &sender.account,
                        "sender",
                        9,
                    ),
                    "127.0.0.1".parse().unwrap(),
                    &recipient.user_id(),
                );
            }
            let path = dir
                .path()
                .join("mt-sticker-v1-cGFjazpleGFtcGxlOndhdmUK8J-Riw.png");
            std::fs::write(&path, base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap()).unwrap();
            Self {
                dir,
                sender,
                receiver,
                other,
                account,
                path,
            }
        }
        async fn send(
            &self,
        ) -> (
            EventId,
            ConversationId,
            Vec<super::super::delivery_store::DeliveryDestination>,
        ) {
            let (id, file) = self
                .sender
                .enqueue_file_to_account(
                    &self.receiver.account_id(),
                    &self.path,
                    crate::file::FileKind::Media,
                )
                .await
                .unwrap();
            let destinations = self
                .sender
                .delivery
                .lock()
                .unwrap()
                .message(id)
                .unwrap()
                .destinations
                .clone();
            assert_eq!(destinations.len(), 2);
            let event = destinations
                .iter()
                .find(|destination| destination.device == self.receiver.identity.public())
                .unwrap()
                .event
                .clone();
            self.receiver
                .log
                .lock()
                .unwrap()
                .append_durable(event.clone())
                .unwrap();
            self.receiver.process_file_events(event.conversation_id);
            (id, file, destinations)
        }
        fn copy_other_device(
            &self,
            id: EventId,
            destinations: &[super::super::delivery_store::DeliveryDestination],
        ) {
            let event = destinations
                .iter()
                .find(|destination| destination.device == self.other.identity.public())
                .unwrap()
                .event
                .clone();
            let host =
                account_conversation_id(&self.sender.account_id(), &self.receiver.account_id());
            let mut row = self
                .sender
                .received_files
                .lock()
                .unwrap()
                .entries(&host)
                .into_iter()
                .find(|row| row.event_id == id)
                .unwrap();
            row.event_id = event.id;
            self.receiver
                .account_file_proofs
                .lock()
                .unwrap()
                .insert(event)
                .unwrap();
            self.receiver
                .received_files
                .lock()
                .unwrap()
                .record_file_alias_durable(&row)
                .unwrap();
        }
    }

    #[tokio::test]
    async fn aliases_keep_repeat_separate_and_delete_stays_deleted_after_late_import_and_restart() {
        let f = Fixture::new();
        let (first, file, first_destinations) = f.send().await;
        let first_bundle = f
            .sender
            .file_message_identity_bundle(first)
            .unwrap()
            .unwrap();
        f.receiver
            .import_file_message_identity(first_bundle.clone())
            .unwrap();
        let (second, repeated, second_destinations) = f.send().await;
        assert_eq!(file, repeated, "artwork reuse must not identify a message");
        assert_ne!(first, second);
        f.copy_other_device(first, &first_destinations);
        f.copy_other_device(second, &second_destinations);
        f.receiver
            .import_file_message_identity(
                f.sender
                    .file_message_identity_bundle(second)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
        let history = f
            .receiver
            .account_history(&f.sender.account_id(), 10)
            .unwrap();
        assert_eq!(history.len(), 2);
        assert!(
            history
                .iter()
                .all(|entry| entry.file_message_aliases.len() == 2)
        );
        let alternate = first_destinations
            .iter()
            .find(|destination| destination.event.id != first)
            .unwrap()
            .event
            .id;
        let context = f
            .receiver
            .project_history(
                account_conversation_id(&f.sender.account_id(), &f.receiver.account_id()),
                Some(&f.sender.account_id()),
                super::super::queries::HistorySelection::Context(alternate),
            )
            .unwrap()
            .unwrap();
        assert!(context.messages.iter().any(|entry| entry.id == first));
        f.receiver
            .delete_account_message(&f.sender.account_id(), alternate)
            .unwrap();
        assert_eq!(
            f.receiver
                .account_history(&f.sender.account_id(), 10)
                .unwrap()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![second]
        );
        f.copy_other_device(first, &first_destinations);
        f.receiver
            .import_file_message_identity(first_bundle)
            .unwrap();
        assert_eq!(
            f.receiver
                .account_history(&f.sender.account_id(), 10)
                .unwrap()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![second]
        );
        let secret = f.receiver.identity.secret_bytes();
        f.receiver.close_profile().unwrap();
        let reopened = open_node(
            &f.dir.path().join("receiver"),
            DeviceIdentity::from_secret_bytes(secret.0, secret.1),
            &f.account,
        );
        assert_eq!(
            reopened
                .account_history(&f.sender.account_id(), 10)
                .unwrap()
                .iter()
                .map(|row| row.id)
                .collect::<Vec<_>>(),
            vec![second]
        );
    }

    #[tokio::test]
    async fn recipient_route_tampering_is_rejected_and_backup_restores_canonical_identity() {
        let f = Fixture::new();
        let other_conv = super::super::conversation::dm_conversation_id(
            &f.sender.identity.public(),
            &f.other.identity.public(),
        );
        f.sender
            .append_event(
                other_conv,
                EventKind::Message,
                b"earlier source-only event".to_vec(),
            )
            .unwrap();
        let missing_parent = f
            .sender
            .log
            .lock()
            .unwrap()
            .events(&other_conv)
            .unwrap()
            .last()
            .unwrap()
            .id;
        let (id, file, destinations) = f.send().await;
        let detached = destinations
            .iter()
            .find(|destination| destination.device == f.other.identity.public())
            .unwrap()
            .event
            .id;
        assert!(
            f.sender
                .log
                .lock()
                .unwrap()
                .get(&detached)
                .unwrap()
                .unwrap()
                .parents
                .contains(&missing_parent)
        );
        f.copy_other_device(id, &destinations);
        let bundle = f.sender.file_message_identity_bundle(id).unwrap().unwrap();
        let mut wrong = decode(&bundle).unwrap();
        wrong.recipient_account = Account::generate().account_id();
        assert!(
            f.receiver
                .import_file_message_identity(sign(&f.sender.identity, &wrong))
                .is_err()
        );
        f.receiver
            .import_file_message_identity(bundle.clone())
            .unwrap();
        let mut snapshot = Vec::new();
        f.receiver
            .write_recovery_snapshot(&mut snapshot, 123)
            .unwrap();
        let restored = Node::install_recovery_snapshot(
            std::io::Cursor::new(snapshot),
            &f.dir.path().join("restored"),
            "restored-pw",
        )
        .unwrap();
        let history = restored
            .account_history(&f.sender.account_id(), 10)
            .unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, id);
        assert_eq!(history[0].file_message_aliases.len(), 2);
        assert!(
            restored
                .log
                .lock()
                .unwrap()
                .get(&detached)
                .unwrap()
                .is_none()
        );
        assert!(
            restored
                .account_file_proofs
                .lock()
                .unwrap()
                .get(&detached)
                .unwrap()
                .is_some()
        );
        let mut source_snapshot = Vec::new();
        f.sender
            .write_recovery_snapshot(&mut source_snapshot, 123)
            .unwrap();
        let restored_source = Node::install_recovery_snapshot(
            std::io::Cursor::new(source_snapshot),
            &f.dir.path().join("restored-source"),
            "restored-pw",
        )
        .unwrap();
        restored_source.roster.lock().unwrap().update(
            &crate::discovery::Announce::new_with_account(
                &f.receiver.identity,
                &f.receiver.account,
                "recipient",
                9,
            ),
            "127.0.0.1".parse().unwrap(),
            &restored_source.user_id(),
        );
        use crate::eventlog::sync::SyncStore;
        let store = restored_source.sync_store(&f.receiver.identity.public());
        assert_eq!(
            store
                .lock()
                .unwrap()
                .file_message_identity(&file, &id)
                .unwrap(),
            Some(bundle)
        );
    }

    #[tokio::test]
    async fn recall_targets_logical_message_when_only_another_device_offer_is_retained() {
        let f = Fixture::new();
        let (first, _, first_destinations) = f.send().await;
        let (second, _, _) = f.send().await;
        f.copy_other_device(first, &first_destinations);
        f.receiver
            .import_file_message_identity(
                f.sender
                    .file_message_identity_bundle(first)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
        f.receiver
            .import_file_message_identity(
                f.sender
                    .file_message_identity_bundle(second)
                    .unwrap()
                    .unwrap(),
            )
            .unwrap();
        let host = account_conversation_id(&f.sender.account_id(), &f.receiver.account_id());
        f.receiver
            .received_files
            .lock()
            .unwrap()
            .remove_where(|row| row.conversation == host && row.event_id == first)
            .unwrap();
        f.sender
            .recall_account(&f.receiver.account_id(), first)
            .await
            .unwrap();
        let conv = super::super::conversation::dm_conversation_id(
            &f.sender.identity.public(),
            &f.receiver.identity.public(),
        );
        let recalls: Vec<_> = f
            .sender
            .log
            .lock()
            .unwrap()
            .events(&conv)
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == EventKind::Delete)
            .collect();
        assert!(!recalls.is_empty());
        for event in recalls {
            f.receiver
                .log
                .lock()
                .unwrap()
                .append_durable(event)
                .unwrap();
        }
        f.receiver.project_private_dm_controls(conv).unwrap();
        let history = f
            .receiver
            .account_history(&f.sender.account_id(), 10)
            .unwrap();
        assert_eq!(history.len(), 2);
        assert!(first_destinations.iter().all(|destination| {
            f.receiver
                .private_dm_message_recalled(
                    &f.sender.account_id(),
                    destination.event.id,
                    &f.sender.account_id(),
                )
                .unwrap()
        }));
        assert!(
            history
                .iter()
                .find(|entry| entry.id == first)
                .unwrap()
                .recalled
        );
        assert!(
            history
                .iter()
                .find(|entry| entry.id == second)
                .unwrap()
                .file
                .is_some()
        );
    }

    #[tokio::test]
    async fn deletion_before_identity_hides_later_sibling_alias_and_survives_restart() {
        let f = Fixture::new();
        let (id, _, destinations) = f.send().await;
        let direct = destinations
            .iter()
            .find(|destination| destination.device == f.receiver.identity.public())
            .unwrap()
            .event
            .id;
        assert!(
            !f.receiver
                .has_file_message_identity(
                    account_conversation_id(&f.sender.account_id(), &f.receiver.account_id()),
                    direct
                )
                .unwrap()
        );
        f.receiver
            .delete_account_message(&f.sender.account_id(), direct)
            .unwrap();
        f.copy_other_device(id, &destinations);
        f.receiver
            .import_file_message_identity(
                f.sender.file_message_identity_bundle(id).unwrap().unwrap(),
            )
            .unwrap();
        assert!(
            f.receiver
                .account_history(&f.sender.account_id(), 10)
                .unwrap()
                .is_empty()
        );
        let host = account_conversation_id(&f.sender.account_id(), &f.receiver.account_id());
        assert!(
            f.receiver
                .account_suppression
                .lock()
                .unwrap()
                .contains(host, id)
                .unwrap()
        );
        assert!(destinations.iter().all(|destination| {
            f.receiver
                .account_suppression
                .lock()
                .unwrap()
                .contains(host, destination.event.id)
                .unwrap()
        }));
        let secrets = f.receiver.identity.secret_bytes();
        f.receiver.close_profile().unwrap();
        let reopened = open_node(
            &f.dir.path().join("receiver"),
            DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
            &f.account,
        );
        assert!(
            reopened
                .account_history(&f.sender.account_id(), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn canonical_and_legacy_search_references_resolve_without_canonical_device_event() {
        let f = Fixture::new();
        let (id, _, destinations) = f.send().await;
        let destination = destinations
            .iter()
            .find(|destination| destination.event.id != id)
            .unwrap();
        let receiver = if destination.device == f.receiver.identity.public() {
            &f.receiver
        } else {
            &f.other
        };
        if receiver
            .account_history(&f.sender.account_id(), 10)
            .unwrap()
            .is_empty()
        {
            receiver
                .log
                .lock()
                .unwrap()
                .append_durable(destination.event.clone())
                .unwrap();
            receiver.process_file_events(destination.event.conversation_id);
        }
        assert!(receiver.log.lock().unwrap().get(&id).unwrap().is_none());
        receiver
            .import_file_message_identity(
                f.sender.file_message_identity_bundle(id).unwrap().unwrap(),
            )
            .unwrap();
        for reference_id in [id, destination.event.id] {
            let reference = super::super::SearchReference::Account {
                account_id: f.sender.account_id(),
                message_id: Some(reference_id),
            };
            let context = receiver
                .search_context(&reference)
                .unwrap()
                .expect("canonical/old references resolve by logical history");
            assert_eq!(context.messages.len(), 1);
            assert_eq!(context.messages[0].id, id);
        }
        let host = account_conversation_id(&f.sender.account_id(), &receiver.account_id());
        receiver
            .my_dm_reactions
            .lock()
            .unwrap()
            .record(
                host,
                1,
                super::super::reaction::ReactionPayload {
                    target: destination.event.id,
                    emoji: "👍".into(),
                    remove: false,
                },
            )
            .unwrap();
        assert_eq!(
            receiver.account_reactions(&f.sender.account_id()).unwrap()[0].target,
            id.to_hex()
        );
        let reply_id = [17; 32];
        let reply = super::super::dm_envelope::DmEnvelope::new(
            f.sender.account_id(),
            receiver.account_id(),
            reply_id,
            super::super::message::MessageBody::new(b"reply".to_vec(), Some(destination.event.id))
                .encode(),
        )
        .encode();
        receiver
            .received
            .lock()
            .unwrap()
            .record(
                host,
                f.sender.account_id(),
                999,
                &reply,
                EventId::new([18; 32]),
            )
            .unwrap();
        assert_eq!(
            receiver
                .account_history(&f.sender.account_id(), 10)
                .unwrap()
                .iter()
                .find(|entry| entry.id == EventId::new(reply_id))
                .unwrap()
                .reply_to,
            Some(id)
        );
    }

    #[tokio::test]
    async fn failed_mapping_append_cannot_resurrect_a_previously_deleted_alias() {
        let f = Fixture::new();
        let (id, _, destinations) = f.send().await;
        let direct = destinations
            .iter()
            .find(|destination| destination.device == f.receiver.identity.public())
            .unwrap()
            .event
            .id;
        f.receiver
            .delete_account_message(&f.sender.account_id(), direct)
            .unwrap();
        f.copy_other_device(id, &destinations);
        let bundle = f.sender.file_message_identity_bundle(id).unwrap().unwrap();
        f.receiver.storage.database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER fail_identity BEFORE INSERT ON file_identity_headers BEGIN SELECT RAISE(ABORT, 'injected identity write failure'); END")?;
            Ok(())
        }).unwrap();
        assert!(f.receiver.import_file_message_identity(bundle).is_err());
        assert!(
            f.receiver
                .account_history(&f.sender.account_id(), 10)
                .unwrap()
                .is_empty()
        );
        let secrets = f.receiver.identity.secret_bytes();
        f.receiver.close_profile().unwrap();
        let reopened = open_node(
            &f.dir.path().join("receiver"),
            DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
            &f.account,
        );
        assert!(
            reopened
                .account_history(&f.sender.account_id(), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn source_adoption_cannot_mint_a_new_account_identity_for_a_legacy_outgoing_card() {
        let f = Fixture::new();
        let (id, _, destinations) = f.send().await;
        // No explicit bundle, exchange or recovery snapshot has run: this is
        // the deployed legacy shape, not an artificially deleted identity log.
        assert!(
            f.sender
                .file_message_identities
                .lock()
                .unwrap()
                .page(None, 1)
                .unwrap()
                .is_empty()
        );
        let old_host = account_conversation_id(&f.sender.account_id(), &f.receiver.account_id());
        let adopted = Account::generate();
        f.sender
            .delivery
            .lock()
            .unwrap()
            .prepare_profile_adoption(&adopted.account_id())
            .unwrap();
        let secrets = f.sender.identity.secret_bytes();
        let reopened = open_node(
            &f.dir.path().join("sender"),
            DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
            &adopted,
        );
        assert!(
            reopened
                .file_message_identities
                .lock()
                .unwrap()
                .page(None, 1)
                .unwrap()
                .is_empty()
        );
        for destination in destinations {
            assert!(
                reopened
                    .file_message_identity_bundle(destination.event.id)
                    .unwrap()
                    .is_none(),
                "an old-account card cannot acquire the adopted account certificate through lazy identity repair"
            );
        }
        assert!(reopened.file_message_identity_bundle(id).unwrap().is_none());
        assert!(
            reopened
                .file_message_identities
                .lock()
                .unwrap()
                .page(None, 1)
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            reopened.history(old_host, 10).unwrap().len(),
            1,
            "refusing a new identity must retain the original historical file row"
        );
        assert!(
            reopened
                .account_history(&f.receiver.account_id(), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn source_adoption_preserves_a_preexisting_historical_file_identity() {
        let f = Fixture::new();
        let (id, _, destinations) = f.send().await;
        let original_account = f.sender.account_id();
        let original = f.sender.file_message_identity_bundle(id).unwrap().unwrap();
        assert_eq!(
            decode(&original).unwrap().source.account_id(),
            original_account
        );
        let adopted = Account::generate();
        f.sender
            .delivery
            .lock()
            .unwrap()
            .prepare_profile_adoption(&adopted.account_id())
            .unwrap();
        let secrets = f.sender.identity.secret_bytes();
        let reopened = open_node(
            &f.dir.path().join("sender"),
            DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
            &adopted,
        );
        for destination in destinations {
            let retained = reopened
                .file_message_identity_bundle(destination.event.id)
                .unwrap()
                .unwrap();
            assert_eq!(
                retained, original,
                "an existing signed historical bundle remains immutable"
            );
            assert_eq!(
                decode(&retained).unwrap().source.account_id(),
                original_account
            );
        }
        assert!(
            reopened
                .account_history(&f.receiver.account_id(), 10)
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn account_adoption_preserves_existing_historical_alias_binding() {
        let f = Fixture::new();
        let (id, _, destinations) = f.send().await;
        f.copy_other_device(id, &destinations);
        f.receiver
            .import_file_message_identity(
                f.sender.file_message_identity_bundle(id).unwrap().unwrap(),
            )
            .unwrap();
        let historical_host =
            account_conversation_id(&f.sender.account_id(), &f.receiver.account_id());
        let adopted = Account::generate();
        f.receiver
            .persist_account_adoption(&adopted.public(), || Ok(()), || Ok(adopted.public()))
            .unwrap();
        let secrets = f.receiver.identity.secret_bytes();
        f.receiver.close_profile().unwrap();
        let reopened = open_node(
            &f.dir.path().join("receiver"),
            DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
            &adopted,
        );
        let history = reopened.history(historical_host, 10).unwrap();
        assert_eq!(history.len(), 1);
        assert_eq!(history[0].id, id);
        assert_eq!(history[0].file_message_aliases.len(), 2);
        assert!(
            reopened
                .account_history(&f.sender.account_id(), 10)
                .unwrap()
                .is_empty(),
            "historical identity never rebinds a new account route"
        );
    }

    #[tokio::test]
    async fn legacy_device_row_before_account_alias_still_allows_scoped_sibling_metadata() {
        let f = Fixture::new();
        let (id, file) = f
            .sender
            .enqueue_file_to_account(
                &f.receiver.account_id(),
                &f.path,
                crate::file::FileKind::Media,
            )
            .await
            .unwrap();
        let destination = f
            .sender
            .delivery
            .lock()
            .unwrap()
            .message(id)
            .unwrap()
            .destinations
            .iter()
            .find(|destination| destination.device == f.receiver.identity.public())
            .unwrap()
            .clone();
        f.receiver
            .log
            .lock()
            .unwrap()
            .append_durable(destination.event.clone())
            .unwrap();
        let plaintext = crate::dm::open(
            &f.receiver.identity,
            &f.sender.identity.public().x25519_pub,
            &destination.event.ciphertext,
        )
        .unwrap();
        let mut row = ReceivedEntry {
            conversation: destination.event.conversation_id,
            event_id: destination.event.id,
            from: f.sender.user_id(),
            wall_clock: destination.event.wall_clock,
            plaintext,
        };
        f.receiver
            .received_files
            .lock()
            .unwrap()
            .record_durable(&row)
            .unwrap();
        row.conversation =
            account_conversation_id(&f.sender.account_id(), &f.receiver.account_id());
        f.receiver
            .received_files
            .lock()
            .unwrap()
            .record_file_alias_durable(&row)
            .unwrap();
        assert_ne!(
            f.receiver
                .received_files
                .lock()
                .unwrap()
                .entry(row.event_id)
                .unwrap()
                .unwrap()
                .conversation,
            row.conversation
        );
        let bundle = f.sender.file_message_identity_bundle(id).unwrap().unwrap();
        f.receiver
            .import_file_message_identity(bundle.clone())
            .unwrap();
        f.receiver
            .account_devices
            .lock()
            .unwrap()
            .insert(super::super::account_devices::AccountDevice {
                public: f.other.identity.public(),
                certificate: f.account.certify(&f.other.identity.public().ed25519_pub),
            })
            .unwrap();
        f.receiver.roster.lock().unwrap().update(
            &crate::discovery::Announce::new_with_account(
                &f.other.identity,
                &f.other.account,
                "sibling",
                9,
            ),
            "127.0.0.1".parse().unwrap(),
            &f.receiver.user_id(),
        );
        f.receiver
            .set_device_sync_category(super::super::SyncCategory::History, true)
            .unwrap();
        use crate::eventlog::sync::SyncStore;
        let store = f.receiver.sync_store(&f.other.identity.public());
        assert_eq!(
            store
                .lock()
                .unwrap()
                .file_message_identity(&file, &row.event_id)
                .unwrap(),
            Some(bundle.clone())
        );
        assert_eq!(
            crate::eventlog::sync::network::NetworkSyncStore::file_message_identity(
                &*store.lock().unwrap(),
                &file,
                &row.event_id
            )
            .unwrap(),
            Some(bundle)
        );
        assert_eq!(
            f.receiver
                .account_history(&f.sender.account_id(), 10)
                .unwrap()[0]
                .id,
            id
        );
    }

    #[tokio::test]
    async fn erasure_capacity_refusal_preserves_every_logical_alias_and_key() {
        use super::super::delivery_store::{DeliveryLimits, DeliveryStore};
        for clear in [false, true] {
            let f = Fixture::new();
            let (id, file, destinations) = f.send().await;
            f.copy_other_device(id, &destinations);
            f.receiver
                .import_file_message_identity(
                    f.sender.file_message_identity_bundle(id).unwrap().unwrap(),
                )
                .unwrap();
            let host = account_conversation_id(&f.sender.account_id(), &f.receiver.account_id());
            let mut legacy = DeliveryStore::open_with_limits(
                &f.dir.path().join("legacy-capacity"),
                "pw",
                DeliveryLimits {
                    completed_receipts: 1,
                    ..DeliveryLimits::default()
                },
            )
            .unwrap();
            legacy
                .bind_profile(
                    &f.dir.path().join("receiver/events.log"),
                    "pw",
                    &f.receiver.identity.public(),
                    &f.receiver.account_id(),
                )
                .unwrap();
            f.receiver.reseed_live_file_book(&legacy).unwrap();
            *f.receiver.delivery.lock().unwrap() = legacy;
            let result = if clear {
                f.receiver
                    .clear_account_conversation(&f.sender.account_id())
            } else {
                f.receiver
                    .delete_account_message(&f.sender.account_id(), id)
            };
            assert!(
                result.is_err(),
                "two legacy aliases require two tombstone slots"
            );
            assert!(
                destinations.iter().all(|destination| !f
                    .receiver
                    .account_suppression
                    .lock()
                    .unwrap()
                    .contains(host, destination.event.id)
                    .unwrap()),
                "refused batch must not suppress any alias"
            );
            assert!(
                destinations.iter().all(|destination| !f
                    .receiver
                    .delivery
                    .lock()
                    .unwrap()
                    .file_erased(host, destination.event.id)),
                "capacity preflight must prevent a partial erase"
            );
            assert_eq!(
                f.receiver
                    .received_files
                    .lock()
                    .unwrap()
                    .entries_ref(&host)
                    .len(),
                2
            );
            assert!(
                f.receiver
                    .files
                    .lock()
                    .unwrap()
                    .manifest(&file)
                    .unwrap()
                    .is_some()
            );
            assert_eq!(
                f.receiver
                    .account_history(&f.sender.account_id(), 10)
                    .unwrap()
                    .iter()
                    .map(|entry| entry.id)
                    .collect::<Vec<_>>(),
                vec![id]
            );
        }
    }

    #[tokio::test]
    async fn accepted_delete_retires_late_alias_key_and_media_despite_erasure_quota() {
        use super::super::delivery_store::{DeliveryLimits, DeliveryStore};
        for keep_other_message in [false, true] {
            let f = Fixture::new();
            let (id, file, destinations) = f.send().await;
            let bundle = f.sender.file_message_identity_bundle(id).unwrap().unwrap();
            let direct = destinations
                .iter()
                .find(|destination| destination.device == f.receiver.identity.public())
                .unwrap()
                .event
                .id;
            f.receiver
                .delete_account_message(&f.sender.account_id(), direct)
                .unwrap();
            if keep_other_message {
                let (second, repeated, _) = f.send().await;
                assert_eq!(repeated, file);
                f.receiver
                    .import_file_message_identity(
                        f.sender
                            .file_message_identity_bundle(second)
                            .unwrap()
                            .unwrap(),
                    )
                    .unwrap();
            }
            f.copy_other_device(id, &destinations);
            // Legacy profiles can retain the exact same signed offer under both
            // its original device host and the account conversation alias.
            let late = destinations
                .iter()
                .find(|destination| destination.device == f.other.identity.public())
                .unwrap();
            let host = account_conversation_id(&f.sender.account_id(), &f.receiver.account_id());
            let mut raw = f
                .receiver
                .received_files
                .lock()
                .unwrap()
                .entries_ref(&host)
                .iter()
                .find(|row| row.event_id == late.event.id)
                .unwrap()
                .clone();
            raw.conversation = late.event.conversation_id;
            f.receiver
                .received_files
                .lock()
                .unwrap()
                .record_file_alias_durable(&raw)
                .unwrap();
            f.receiver
                .media
                .store_from_path(file, f.path.file_name().unwrap().to_str().unwrap(), &f.path)
                .unwrap();
            let mut legacy = DeliveryStore::open_with_limits(
                &f.dir.path().join("late-alias-capacity"),
                "pw",
                DeliveryLimits {
                    completed_receipts: 0,
                    ..DeliveryLimits::default()
                },
            )
            .unwrap();
            legacy
                .bind_profile(
                    &f.dir.path().join("receiver/events.log"),
                    "pw",
                    &f.receiver.identity.public(),
                    &f.receiver.account_id(),
                )
                .unwrap();
            f.receiver.reseed_live_file_book(&legacy).unwrap();
            *f.receiver.delivery.lock().unwrap() = legacy;
            assert!(
                f.receiver.read_media(file).is_some(),
                "late replica initially has a valid, completed media reference"
            );
            assert!(
                f.receiver.import_file_message_identity(bundle).is_err(),
                "physical erasure cannot allocate a legacy tombstone"
            );
            assert_eq!(
                f.receiver
                    .account_history(&f.sender.account_id(), 10)
                    .unwrap()
                    .len(),
                usize::from(keep_other_message)
            );
            assert_eq!(
                f.receiver
                    .files
                    .lock()
                    .unwrap()
                    .manifest(&file)
                    .unwrap()
                    .is_some(),
                keep_other_message,
                "accepted deletion retires its key while an independent same-artwork message keeps its own scope"
            );
            assert_eq!(f.receiver.read_media(file).is_some(), keep_other_message);
            assert_eq!(f.receiver.media.contains(file), keep_other_message);
            let secrets = f.receiver.identity.secret_bytes();
            f.receiver.close_profile().unwrap();
            let reopened = open_node(
                &f.dir.path().join("receiver"),
                DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
                &f.account,
            );
            assert_eq!(
                reopened
                    .files
                    .lock()
                    .unwrap()
                    .manifest(&file)
                    .unwrap()
                    .is_some(),
                keep_other_message
            );
            assert_eq!(reopened.read_media(file).is_some(), keep_other_message);
            assert_eq!(
                reopened
                    .account_history(&f.sender.account_id(), 10)
                    .unwrap()
                    .len(),
                usize::from(keep_other_message)
            );
            let mut snapshot = Vec::new();
            reopened
                .write_recovery_snapshot(&mut snapshot, 123)
                .unwrap();
            let restored = Node::install_recovery_snapshot(
                std::io::Cursor::new(snapshot),
                &f.dir.path().join("restored-after-delete"),
                "restored-pw",
            )
            .unwrap();
            assert_eq!(
                restored
                    .account_history(&f.sender.account_id(), 10)
                    .unwrap()
                    .len(),
                usize::from(keep_other_message)
            );
            assert_eq!(
                restored
                    .files
                    .lock()
                    .unwrap()
                    .manifest(&file)
                    .unwrap()
                    .is_some(),
                keep_other_message
            );
            assert_eq!(restored.read_media(file).is_some(), keep_other_message);
        }
    }
}
