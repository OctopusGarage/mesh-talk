use super::*;
use crate::{
    eventlog::{EventId, LogError, event::ConversationId},
    identity::device::PublicIdentity,
    node::media_store::MediaStore,
    storage::record_log::EncryptedRecordLog,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::Path,
};
const MAX_ORDINARY_KEYS: usize = 4096;
const MAX_MESSAGE_KEYS: usize = 65536;
const MAX_REFERENCE_KEYS: usize = 4096;
const MAX_GROUP_CONTROL_KEYS: usize = 4096;
const MAX_GROUP_OPERATION_KEYS: usize = 4096;
const MAX_OPERATION_LIVE: u64 = 32 * 1024 * 1024;
// Read receipts have no blobs; each other retained key owns at most one blob.
const MAX_OWNED_BLOBS: usize = MAX_ORDINARY_KEYS
    + MAX_MESSAGE_KEYS
    + MAX_REFERENCE_KEYS
    + MAX_GROUP_CONTROL_KEYS
    + MAX_GROUP_OPERATION_KEYS;
// Include key.salt and one replacement/partial blob interrupted before cleanup.
const MAX_MEDIA_ENTRIES: usize = MAX_OWNED_BLOBS + 2;
const MAX_LIVE: u64 = 128 * 1024 * 1024;
// These are local encrypted-index limits, independent of document wire quotas.
const MAX_LOCAL_ROW: usize = 12 * 1024 * 1024;
const MAX_LOCAL_FRONTIER: usize = 131_072;
const MAX_LOCAL_RECEIPTS: usize = 32 * 1024 * 1024;
const MAX_COMPACT_INDEX: usize = 112 * 1024 * 1024;
const INDEX_HEADER: usize = 6 + crate::storage::encryption::SALT_SIZE;
// bincode Vec length, frame length, nonce, and AES-GCM tag.
const INDEX_FRAME: usize = 8 + 4 + crate::storage::encryption::NONCE_SIZE + 16;

/// Never serialized into a wire metadata header or accepted from a remote merge.
#[derive(Clone, Serialize, Deserialize)]
struct LocalPublicationReceipt {
    version: u8,
    account_public: [u8; 32],
    source: PublicIdentity,
    group: [u8; 32],
    revision: u64,
    content_hash: [u8; 32],
    frontier_hex: String,
}
impl LocalPublicationReceipt {
    fn matches(&self, meta: &AccountDocumentMeta, source: &PublicIdentity) -> bool {
        self.version == 1
            && self.account_public == meta.account_public
            && self.source == *source
            && self.revision == meta.revision
            && self.content_hash == meta.content_hash
            && !meta.tombstone
            && matches!(&meta.key,
                AccountDocumentKey::GroupControl { channel, source_device }
                | AccountDocumentKey::GroupInventory { channel, source_device }
                if *channel == hex::encode(self.group) && *source_device == source.user_id()
                    && meta.writer_device == *source_device)
    }
    fn frontier(&self) -> Option<Vec<EventId>> {
        if self.frontier_hex.is_empty()
            || !self.frontier_hex.len().is_multiple_of(64)
            || self.frontier_hex.len() > MAX_LOCAL_FRONTIER * 64
        {
            return None;
        }
        let bytes = hex::decode(&self.frontier_hex).ok()?;
        let ids: Vec<_> = bytes
            .as_chunks::<32>()
            .0
            .iter()
            .map(|bytes| EventId::new(*bytes))
            .collect();
        ids.windows(2).all(|pair| pair[0] < pair[1]).then_some(ids)
    }
}
#[derive(Serialize, Deserialize)]
struct LocalIndexRow {
    #[serde(flatten)]
    meta: AccountDocumentMeta,
    #[serde(
        default,
        rename = "_local_group_publication",
        skip_serializing_if = "Option::is_none"
    )]
    receipt: Option<serde_json::Value>,
    #[serde(
        default,
        rename = "_local_inventory_source_authenticated",
        skip_serializing_if = "is_false"
    )]
    source_authenticated: bool,
}
fn is_false(value: &bool) -> bool {
    !*value
}
struct RetainedPublicationReceipt {
    receipt: LocalPublicationReceipt,
    encoded_len: usize,
    row_len: usize,
}
fn encode_row(
    meta: &AccountDocumentMeta,
    receipt: Option<&LocalPublicationReceipt>,
    source_authenticated: bool,
) -> Result<Vec<u8>, LogError> {
    if receipt.is_none() && !source_authenticated {
        return serde_json::to_vec(meta).map_err(|_| corrupt());
    }
    serde_json::to_vec(&LocalIndexRow {
        meta: meta.clone(),
        receipt: receipt
            .map(serde_json::to_value)
            .transpose()
            .map_err(|_| corrupt())?,
        source_authenticated,
    })
    .map_err(|_| corrupt())
}
pub(in crate::node) struct AccountDocumentStore {
    file: EncryptedRecordLog<Vec<u8>>,
    media: MediaStore,
    current: BTreeMap<AccountDocumentKey, AccountDocumentMeta>,
    account: [u8; 32],
    records: usize,
    journal_bytes: usize,
    counts: [usize; 9],
    live_bytes: u64,
    operation_keys: usize,
    operation_bytes: u64,
    digest_xor: [[u8; 32]; 9],
    incomplete: BTreeSet<AccountDocumentKey>,
    local_receipts: BTreeMap<AccountDocumentKey, RetainedPublicationReceipt>,
    // Bounded by current inventory keys; never exported in document metadata.
    inventory_source_floors: BTreeSet<AccountDocumentKey>,
    receipt_bytes: usize,
    index_bytes: usize,
    #[cfg(test)]
    receipt_limit: usize,
}
fn metadata_hash(meta: &AccountDocumentMeta) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(meta.message());
    hash.finalize().into()
}
fn corrupt() -> LogError {
    LogError::CorruptFile("invalid account document storage".into())
}
impl AccountDocumentStore {
    pub(in crate::node) fn open(
        dir: &Path,
        password: &str,
        account: &[u8; 32],
    ) -> Result<Self, LogError> {
        let id = crate::identity::account::AccountPublic {
            ed25519_pub: *account,
        }
        .account_id();
        let scoped =
            zeroize::Zeroizing::new(format!("mesh-talk-account-documents-v1:{id}:{password}"));
        let path = dir.join(format!("account-documents-{id}.log"));
        match std::fs::symlink_metadata(&path) {
            Ok(m) if !m.is_file() || m.len() > 128 * 1024 * 1024 => return Err(corrupt()),
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(LogError::Io(e)),
        }
        let (file, rows): (_, Vec<Vec<u8>>) = EncryptedRecordLog::open(&path, &scoped, b"MTADO1")?;
        let blob_root = dir.join(format!("account-document-blobs-{id}"));
        for path in [&blob_root, &blob_root.join("media")] {
            match std::fs::symlink_metadata(path) {
                Ok(m) if !m.is_dir() => return Err(corrupt()),
                Ok(_) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => return Err(LogError::Io(e)),
            }
        }
        // This namespace belongs exclusively to account documents. Other media
        // stores keep their own recovery policy and are never scanned here.
        match std::fs::read_dir(blob_root.join("media")) {
            Ok(entries) => {
                for (i, entry) in entries.enumerate() {
                    if i >= MAX_MEDIA_ENTRIES {
                        return Err(corrupt());
                    }
                    let entry = entry.map_err(LogError::Io)?;
                    if entry.file_type().map_err(LogError::Io)?.is_file()
                        && entry.file_name().to_str().is_some_and(|name| {
                            name.strip_suffix(".bin.part")
                                .is_some_and(|stem| hex(stem, 64))
                        })
                    {
                        std::fs::remove_file(entry.path()).map_err(LogError::Io)?;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(LogError::Io(e)),
        }
        let media = MediaStore::open(
            &dir.join(format!("account-document-blobs-{id}")),
            &scoped,
            &id,
        )
        .map_err(LogError::Io)?;
        let journal_bytes =
            INDEX_HEADER + rows.iter().map(|r| r.len() + INDEX_FRAME).sum::<usize>();
        let mut store = Self {
            file,
            media,
            current: BTreeMap::new(),
            account: *account,
            records: rows.len(),
            journal_bytes,
            counts: [0; 9],
            live_bytes: 0,
            operation_keys: 0,
            operation_bytes: 0,
            digest_xor: [[0; 32]; 9],
            incomplete: BTreeSet::new(),
            local_receipts: BTreeMap::new(),
            inventory_source_floors: BTreeSet::new(),
            receipt_bytes: 0,
            index_bytes: INDEX_HEADER,
            #[cfg(test)]
            receipt_limit: MAX_LOCAL_RECEIPTS,
        };
        for bytes in rows {
            if bytes.len() > MAX_LOCAL_ROW {
                return Err(corrupt());
            }
            let row: LocalIndexRow = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
            if bytes.len() > 8192
                && (row.receipt.is_none()
                    || !matches!(
                        row.meta.key,
                        AccountDocumentKey::GroupControl { .. }
                            | AccountDocumentKey::GroupInventory { .. }
                    ))
            {
                return Err(corrupt());
            }
            store.validate(&row.meta)?;
            let receipt = row
                .receipt
                .and_then(|value| serde_json::from_value::<LocalPublicationReceipt>(value).ok())
                .filter(|receipt| {
                    receipt.matches(&row.meta, &receipt.source) && receipt.frontier().is_some()
                });
            if row.source_authenticated
                && !matches!(row.meta.key, AccountDocumentKey::GroupInventory { .. })
            {
                return Err(corrupt());
            }
            let same = store.current.get(&row.meta.key) == Some(&row.meta);
            let newer = store
                .current
                .get(&row.meta.key)
                .is_none_or(|old| old.order() < row.meta.order());
            let local_repair = same
                && (receipt.is_some() && !store.local_receipts.contains_key(&row.meta.key)
                    || row.source_authenticated
                        && !store.inventory_source_floors.contains(&row.meta.key));
            if newer || local_repair {
                // Local auxiliary state may be durably repaired without changing
                // the public signed metadata. Replay must retain that same-head
                // receipt/floor, and cannot erase an already retained receipt.
                let receipt = if same {
                    receipt.or_else(|| {
                        store
                            .local_receipts
                            .get(&row.meta.key)
                            .map(|retained| retained.receipt.clone())
                    })
                } else {
                    receipt
                };
                store.install_record(row.meta, receipt, row.source_authenticated);
            }
        }
        for key in store.current.keys() {
            if store.get(key).is_err() {
                store.incomplete.insert(key.clone());
            }
        }
        store
            .media
            .prune_unreferenced(&store.live())
            .map_err(LogError::Io)?;
        Ok(store)
    }
    #[cfg(test)]
    pub(super) fn remove_blob_for_test(&self, hash: [u8; 32]) {
        self.media.remove(ConversationId::new(hash)).unwrap();
    }
    #[cfg(test)]
    pub(super) fn fail_index_for_test(&mut self, fail: bool) {
        self.file.fail_appends_for_test(fail).unwrap();
    }
    #[cfg(test)]
    pub(super) fn publication_budget_for_test(&mut self, limit: usize) {
        self.receipt_limit = limit;
    }
    #[cfg(test)]
    pub(super) fn compact_next_for_test(&mut self) {
        self.records = self.current.len().saturating_mul(2).saturating_add(32);
    }
    pub(super) fn meta(&self, key: &AccountDocumentKey) -> Option<AccountDocumentMeta> {
        self.current.get(key).cloned()
    }
    pub(super) fn list(
        &self,
        domain: AccountDocumentDomain,
        after: Option<&AccountDocumentKey>,
        limit: usize,
    ) -> Vec<AccountDocumentMeta> {
        self.list_scope(domain, after, limit, true)
    }
    pub(super) fn list_scope(
        &self,
        domain: AccountDocumentDomain,
        after: Option<&AccountDocumentKey>,
        limit: usize,
        include_operations: bool,
    ) -> Vec<AccountDocumentMeta> {
        self.current
            .values()
            .filter(|m| {
                m.key.domain() == domain
                    && (include_operations
                        || !matches!(m.key, AccountDocumentKey::GroupOperation { .. }))
                    && after.is_none_or(|a| &m.key > a)
            })
            .take(limit.min(64))
            .cloned()
            .collect()
    }
    pub(super) fn digest(&self, domain: AccountDocumentDomain) -> [u8; 32] {
        if self.incomplete.iter().any(|key| key.domain() == domain) {
            return [0; 32];
        }
        let index = domain as usize;
        let mut hash = Sha256::new();
        hash.update(b"mesh-talk-account-document-index-v1");
        hash.update([index as u8]);
        hash.update(self.digest_xor[index]);
        hash.update((self.counts[index] as u64).to_be_bytes());
        hash.finalize().into()
    }
    pub(super) fn digest_scope(
        &self,
        domain: AccountDocumentDomain,
        include_operations: bool,
    ) -> [u8; 32] {
        if include_operations || domain != AccountDocumentDomain::GroupControl {
            return self.digest(domain);
        }
        if self
            .incomplete
            .iter()
            .any(|key| matches!(key, AccountDocumentKey::GroupControl { .. }))
        {
            return [0; 32];
        }
        let mut xor = [0; 32];
        let mut count = 0u64;
        for meta in self
            .current
            .values()
            .filter(|meta| matches!(meta.key, AccountDocumentKey::GroupControl { .. }))
        {
            count += 1;
            for (byte, part) in xor.iter_mut().zip(metadata_hash(meta)) {
                *byte ^= part;
            }
        }
        let mut hash = Sha256::new();
        hash.update(b"mesh-talk-account-document-index-v1");
        hash.update([domain as u8]);
        hash.update(xor);
        hash.update(count.to_be_bytes());
        hash.finalize().into()
    }
    pub(super) fn operation_rows(&self) -> Vec<AccountDocumentMeta> {
        self.current
            .values()
            .filter(|meta| matches!(meta.key, AccountDocumentKey::GroupOperation { .. }))
            .take(MAX_GROUP_OPERATION_KEYS)
            .cloned()
            .collect()
    }
    pub(super) fn list_custody(
        &self,
        start: &AccountDocumentKey,
        channel: &str,
        event: &str,
    ) -> Vec<AccountDocumentMeta> {
        self.current.range(start.clone()..).take_while(|(key, _)| matches!(key, AccountDocumentKey::ChannelCustody { channel: c, event: e, .. } if c == channel && e == event)).take(4096).map(|(_, meta)| meta.clone()).collect()
    }
    pub(super) fn inventory_channel_rows(&self, channel: &str) -> Vec<AccountDocumentMeta> {
        let start = AccountDocumentKey::GroupInventory {
            channel: channel.to_owned(),
            source_device: String::new(),
        };
        self.current.range(start..).take_while(|(key, _)| matches!(key, AccountDocumentKey::GroupInventory { channel: id, .. } if id == channel)).take(MAX_REFERENCE_KEYS).map(|(_, meta)| meta.clone()).collect()
    }
    pub(super) fn inventory_rows(&self) -> Vec<AccountDocumentMeta> {
        self.current
            .values()
            .filter(|m| m.key.domain() == AccountDocumentDomain::GroupInventory)
            .take(MAX_REFERENCE_KEYS)
            .cloned()
            .collect()
    }
    pub(super) fn group_control_rows(&self, channel: &str) -> Vec<AccountDocumentMeta> {
        let start = AccountDocumentKey::GroupControl {
            channel: channel.to_owned(),
            source_device: String::new(),
        };
        self.current.range(start..)
            .take_while(|(key, _)| matches!(key, AccountDocumentKey::GroupControl { channel: id, .. } if id == channel))
            .take(MAX_GROUP_CONTROL_KEYS).map(|(_, meta)| meta.clone()).collect()
    }
    #[cfg(test)]
    fn install(&mut self, meta: AccountDocumentMeta) {
        self.install_record(meta, None, false);
    }
    fn install_record(
        &mut self,
        meta: AccountDocumentMeta,
        receipt: Option<LocalPublicationReceipt>,
        source_authenticated: bool,
    ) {
        if let Some(old) = self.current.get(&meta.key) {
            let row_len = self.local_receipts.get(&meta.key).map_or_else(
                || {
                    encode_row(old, None, self.inventory_source_floors.contains(&meta.key))
                        .expect("metadata serializes")
                        .len()
                },
                |old| old.row_len,
            );
            self.index_bytes -= row_len + INDEX_FRAME;
        }
        if let Some(old) = self.local_receipts.remove(&meta.key) {
            self.receipt_bytes -= old.encoded_len;
        }
        if source_authenticated {
            self.inventory_source_floors.insert(meta.key.clone());
        }
        let row_len = encode_row(
            &meta,
            receipt.as_ref(),
            self.inventory_source_floors.contains(&meta.key),
        )
        .expect("local metadata serializes")
        .len();
        self.index_bytes += row_len + INDEX_FRAME;
        if let Some(receipt) = receipt {
            let encoded_len = serde_json::to_vec(&receipt)
                .expect("local receipt serializes")
                .len();
            self.receipt_bytes += encoded_len;
            self.local_receipts.insert(
                meta.key.clone(),
                RetainedPublicationReceipt {
                    receipt,
                    encoded_len,
                    row_len,
                },
            );
        }
        if matches!(meta.key, AccountDocumentKey::GroupOperation { .. }) {
            if let Some(old) = self.current.get(&meta.key) {
                if !old.tombstone {
                    self.operation_bytes -= u64::from(old.content_len);
                }
            } else {
                self.operation_keys += 1;
            }
            if !meta.tombstone {
                self.operation_bytes += u64::from(meta.content_len);
            }
        }
        let index = meta.key.domain() as usize;
        if let Some(old) = self.current.get(&meta.key) {
            let old_hash = metadata_hash(old);
            for (byte, part) in self.digest_xor[index].iter_mut().zip(old_hash) {
                *byte ^= part;
            }
            if !old.tombstone {
                self.live_bytes -= u64::from(old.content_len);
            }
        } else {
            self.counts[index] += 1;
        }
        let new_hash = metadata_hash(&meta);
        for (byte, part) in self.digest_xor[index].iter_mut().zip(new_hash) {
            *byte ^= part;
        }
        if !meta.tombstone {
            self.live_bytes += u64::from(meta.content_len);
        }
        self.incomplete.remove(&meta.key);
        self.current.insert(meta.key.clone(), meta);
    }
    pub(super) fn get(
        &self,
        key: &AccountDocumentKey,
    ) -> Result<Option<AccountDocument>, LogError> {
        let Some(meta) = self.current.get(key).filter(|m| !m.tombstone).cloned() else {
            return Ok(None);
        };
        let payload = if meta.content_len == 0 {
            Vec::new()
        } else {
            struct Bounded {
                bytes: zeroize::Zeroizing<Vec<u8>>,
                limit: usize,
            }
            impl std::io::Write for Bounded {
                fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                    if self.bytes.len() + bytes.len() > self.limit {
                        return Err(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "oversized account document blob",
                        ));
                    }
                    self.bytes.extend_from_slice(bytes);
                    Ok(bytes.len())
                }
                fn flush(&mut self) -> std::io::Result<()> {
                    Ok(())
                }
            }
            let mut output = Bounded {
                bytes: zeroize::Zeroizing::new(Vec::new()),
                limit: meta.content_len as usize,
            };
            self.media
                .copy_decrypted(ConversationId::new(meta.content_hash), &mut output)
                .map_err(LogError::Io)?;
            std::mem::take(&mut *output.bytes)
        };
        if payload.len() != meta.content_len as usize || content_hash(&payload) != meta.content_hash
        {
            return Err(corrupt());
        }
        Ok(Some(AccountDocument { meta, payload }))
    }
    pub(super) fn put(
        &mut self,
        account: &Account,
        writer: String,
        key: AccountDocumentKey,
        payload: &[u8],
        expected: Option<&DocumentVersion>,
        removed: bool,
    ) -> Result<AccountDocumentMeta, NodeError> {
        if account.public().ed25519_pub != self.account
            || key.is_private_control()
            || !key.valid()
            || payload.len() > key.limit()
            || !hex(&writer, 32)
            || (removed && matches!(key, AccountDocumentKey::ReadReceipt { .. }))
        {
            return Err(invalid());
        }
        let old = self.current.get(&key);
        if old.map(AccountDocumentMeta::version).as_ref() != expected {
            return Err(NodeError::InvalidInput("account document changed".into()));
        }
        let revision = old
            .map_or(0, |m| m.revision)
            .checked_add(1)
            .ok_or_else(invalid)?
            .max(super::super::node::now_millis());
        let meta = AccountDocumentMeta::new(account, writer, key, revision, payload, removed);
        self.merge(meta.clone(), payload).map_err(NodeError::Log)?;
        Ok(meta)
    }
    pub(super) fn group_control_payload_fits(
        &self,
        key: &AccountDocumentKey,
        bytes: usize,
    ) -> bool {
        let old = self.current.get(key);
        (old.is_some()
            || self.counts[AccountDocumentDomain::GroupControl as usize]
                .saturating_sub(self.operation_keys)
                < MAX_GROUP_CONTROL_KEYS)
            && self
                .live_bytes
                .saturating_sub(
                    old.filter(|meta| !meta.tombstone)
                        .map_or(0, |meta| u64::from(meta.content_len)),
                )
                .saturating_add(bytes as u64)
                <= MAX_LIVE
    }
    pub(super) fn operation_payload_fits(&self, key: &AccountDocumentKey, bytes: usize) -> bool {
        let old = self
            .current
            .get(key)
            .filter(|meta| !meta.tombstone)
            .map_or(0, |meta| u64::from(meta.content_len));
        (self.current.contains_key(key) || self.operation_keys < MAX_GROUP_OPERATION_KEYS)
            && self
                .operation_bytes
                .saturating_sub(old)
                .saturating_add(bytes as u64)
                <= MAX_OPERATION_LIVE
            && self
                .live_bytes
                .saturating_sub(old)
                .saturating_add(bytes as u64)
                <= MAX_LIVE
    }
    pub(super) fn merge(
        &mut self,
        meta: AccountDocumentMeta,
        payload: &[u8],
    ) -> Result<bool, LogError> {
        self.merge_record(meta, payload, None)
    }
    pub(super) fn merge_local_group_control(
        &mut self,
        meta: AccountDocumentMeta,
        payload: &[u8],
        frontier: Vec<EventId>,
    ) -> Result<bool, LogError> {
        let head =
            crate::node::account_group::wire::GroupControlHead::decode(payload, &self.account)
                .ok_or_else(corrupt)?;
        self.merge_local_publication(
            meta,
            payload,
            frontier,
            head.proof.public(),
            *head.group.as_bytes(),
            head.access.complete_frontier,
        )
    }
    pub(super) fn merge_local_group_inventory(
        &mut self,
        meta: AccountDocumentMeta,
        payload: &[u8],
        frontier: Vec<EventId>,
    ) -> Result<bool, LogError> {
        let (source, group, complete) =
            super::inventory::local_publication(payload, &self.account).ok_or_else(corrupt)?;
        self.merge_local_publication(meta, payload, frontier, source, *group.as_bytes(), complete)
    }
    fn merge_local_publication(
        &mut self,
        meta: AccountDocumentMeta,
        payload: &[u8],
        frontier: Vec<EventId>,
        source: PublicIdentity,
        group: [u8; 32],
        complete: bool,
    ) -> Result<bool, LogError> {
        let receipt = if complete {
            None
        } else {
            if frontier.is_empty() || frontier.len() > MAX_LOCAL_FRONTIER {
                return Err(corrupt());
            }
            let receipt = LocalPublicationReceipt {
                version: 1,
                account_public: self.account,
                source: source.clone(),
                group,
                revision: meta.revision,
                content_hash: meta.content_hash,
                frontier_hex: frontier.iter().map(EventId::to_hex).collect(),
            };
            if !receipt.matches(&meta, &source) || receipt.frontier().as_ref() != Some(&frontier) {
                return Err(corrupt());
            }
            Some(receipt)
        };
        self.merge_record(meta, payload, receipt)
    }
    pub(super) fn local_publication_frontier(
        &self,
        meta: &AccountDocumentMeta,
        source: &PublicIdentity,
    ) -> Option<Vec<EventId>> {
        let retained = self.local_receipts.get(&meta.key)?;
        retained
            .receipt
            .matches(meta, source)
            .then(|| retained.receipt.frontier())
            .flatten()
    }
    fn merge_record(
        &mut self,
        meta: AccountDocumentMeta,
        payload: &[u8],
        receipt: Option<LocalPublicationReceipt>,
    ) -> Result<bool, LogError> {
        self.validate(&meta)?;
        if meta.key.is_private_control()
            && !match meta.key {
                AccountDocumentKey::GroupOperation { .. } => {
                    crate::node::account_group::operations::valid_payload(&meta, payload)
                }
                AccountDocumentKey::GroupControl { .. } => {
                    super::group_control::valid_payload(&meta, payload)
                }
                AccountDocumentKey::GroupInventory { .. } => {
                    super::inventory::valid_payload(&meta, payload)
                }
                AccountDocumentKey::ChannelCustody { .. } => {
                    super::custody::valid_payload(&meta, payload)
                }
                _ => super::controls::valid_payload(&meta, payload),
            }
        {
            return Err(corrupt());
        }
        if (!meta.tombstone
            && (payload.len() != meta.content_len as usize
                || content_hash(payload) != meta.content_hash))
            || (meta.tombstone && !payload.is_empty())
        {
            return Err(corrupt());
        }
        if let Some(old) = self
            .current
            .get(&meta.key)
            .filter(|old| old.order() >= meta.order())
        {
            // An ordinary import intentionally cannot carry a source's private
            // recovery receipt. The real source can reconstruct it from its
            // exact validated public view without changing the signed head.
            let recovering_local_receipt =
                old == &meta && receipt.is_some() && !self.local_receipts.contains_key(&meta.key);
            if !recovering_local_receipt {
                if old == &meta
                    && !meta.tombstone
                    && (self.incomplete.contains(&meta.key) || self.get(&meta.key).is_err())
                {
                    self.media
                        .store_bytes(
                            ConversationId::new(meta.content_hash),
                            "document.bin",
                            payload,
                        )
                        .map_err(LogError::Io)?;
                    self.incomplete.remove(&meta.key);
                    return Ok(true);
                }
                return Ok(false);
            }
        }
        let inventory = matches!(meta.key, AccountDocumentKey::GroupInventory { .. });
        let source_authenticated =
            inventory && !meta.tombstone && super::inventory::source_authenticated(payload);
        let old_floor = self.inventory_source_floors.contains(&meta.key);
        if inventory && !meta.tombstone && !source_authenticated {
            if old_floor {
                return Err(corrupt());
            }
            if self
                .current
                .get(&meta.key)
                .is_some_and(|old| !old.tombstone)
            {
                match self.get(&meta.key) {
                    Ok(Some(old)) if super::inventory::source_authenticated(old.payload()) => {
                        return Err(corrupt());
                    }
                    Err(_) => return Err(corrupt()),
                    _ => {}
                }
            }
        }
        let source_authenticated = old_floor || source_authenticated;
        let bytes = encode_row(&meta, receipt.as_ref(), source_authenticated)?;
        let old_receipt = self.local_receipts.get(&meta.key);
        let receipt_len = receipt
            .as_ref()
            .map(|receipt| serde_json::to_vec(receipt).map(|bytes| bytes.len()))
            .transpose()
            .map_err(|_| corrupt())?
            .unwrap_or(0);
        let old_row_len = self
            .current
            .get(&meta.key)
            .map(|old| {
                old_receipt.map_or_else(
                    || encode_row(old, None, old_floor).map(|bytes| bytes.len()),
                    |old| Ok(old.row_len),
                )
            })
            .transpose()?
            .unwrap_or(0);
        let index_bytes = self
            .index_bytes
            .saturating_sub(if self.current.contains_key(&meta.key) {
                old_row_len + INDEX_FRAME
            } else {
                0
            })
            .saturating_add(bytes.len() + INDEX_FRAME);
        let receipt_bytes = self
            .receipt_bytes
            .saturating_sub(old_receipt.map_or(0, |old| old.encoded_len))
            .saturating_add(receipt_len);
        let receipt_limit = MAX_LOCAL_RECEIPTS;
        #[cfg(test)]
        let receipt_limit = receipt_limit.min(self.receipt_limit);
        if bytes.len()
            > if receipt.is_some() {
                MAX_LOCAL_ROW
            } else {
                8192
            }
            || receipt_bytes > receipt_limit
            || index_bytes > MAX_COMPACT_INDEX
        {
            // Never orphan a retained incomplete head by evicting its receipt.
            return Err(corrupt());
        }
        if meta.content_len > 0 && !meta.tombstone {
            self.media
                .store_bytes(
                    ConversationId::new(meta.content_hash),
                    "document.bin",
                    payload,
                )
                .map_err(LogError::Io)?;
        }
        let old_hash = self
            .current
            .get(&meta.key)
            .filter(|m| !m.tombstone && m.content_len > 0)
            .map(|m| m.content_hash);
        let commit = (|| {
            if self.records >= self.current.len().saturating_mul(2).saturating_add(32)
                || self.journal_bytes + bytes.len() + INDEX_FRAME > 120 * 1024 * 1024
            {
                let rows: Result<Vec<_>, _> = self
                    .current
                    .values()
                    .map(|meta| {
                        encode_row(
                            meta,
                            self.local_receipts
                                .get(&meta.key)
                                .map(|retained| &retained.receipt),
                            self.inventory_source_floors.contains(&meta.key),
                        )
                    })
                    .collect();
                let rows = rows.map_err(|_| corrupt())?;
                self.file.rewrite(&rows)?;
                self.records = self.current.len();
                self.journal_bytes =
                    INDEX_HEADER + rows.iter().map(|r| r.len() + INDEX_FRAME).sum::<usize>();
            }
            self.file.append_durable(&bytes)
        })();
        if let Err(error) = commit {
            if !self
                .live()
                .contains(&ConversationId::new(meta.content_hash))
            {
                let _ = self.media.remove(ConversationId::new(meta.content_hash));
            }
            return Err(error);
        }
        self.journal_bytes += bytes.len() + INDEX_FRAME;
        self.records += 1;
        self.install_record(meta, receipt, source_authenticated);
        if let Some(old_hash) = old_hash
            && !self.live().contains(&ConversationId::new(old_hash))
        {
            let _ = self.media.remove(ConversationId::new(old_hash));
        }
        Ok(true)
    }
    fn validate(&self, meta: &AccountDocumentMeta) -> Result<(), LogError> {
        if !meta.valid(&self.account) {
            return Err(corrupt());
        }
        let seen = matches!(meta.key, AccountDocumentKey::ReadReceipt { .. });
        let custody = matches!(meta.key, AccountDocumentKey::ChannelCustody { .. });
        let reference = matches!(meta.key, AccountDocumentKey::GroupInventory { .. });
        let operation = matches!(meta.key, AccountDocumentKey::GroupOperation { .. });
        let group_control = matches!(meta.key, AccountDocumentKey::GroupControl { .. });
        let count = if operation {
            self.operation_keys
        } else if group_control {
            self.counts[AccountDocumentDomain::GroupControl as usize]
                .saturating_sub(self.operation_keys)
        } else if reference {
            self.counts[AccountDocumentDomain::GroupInventory as usize]
        } else if custody {
            self.counts[AccountDocumentDomain::ChannelCustody as usize]
        } else if seen {
            self.counts[AccountDocumentDomain::ReadReceipt as usize]
        } else {
            self.counts.iter().sum::<usize>()
                - self.counts[AccountDocumentDomain::ReadReceipt as usize]
                - self.counts[AccountDocumentDomain::ChannelCustody as usize]
                - self.counts[AccountDocumentDomain::GroupInventory as usize]
                - self.counts[AccountDocumentDomain::GroupControl as usize]
        };
        if !self.current.contains_key(&meta.key)
            && count
                >= if operation {
                    MAX_GROUP_OPERATION_KEYS
                } else if group_control {
                    MAX_GROUP_CONTROL_KEYS
                } else if reference {
                    MAX_REFERENCE_KEYS
                } else if seen || custody {
                    MAX_MESSAGE_KEYS
                } else {
                    MAX_ORDINARY_KEYS
                }
        {
            return Err(corrupt());
        }
        let previous = self
            .current
            .get(&meta.key)
            .filter(|m| !m.tombstone)
            .map_or(0, |m| u64::from(m.content_len));
        let bytes = self.live_bytes - previous
            + if meta.tombstone {
                0
            } else {
                u64::from(meta.content_len)
            };
        if operation
            && self
                .operation_bytes
                .saturating_sub(previous)
                .saturating_add(if meta.tombstone {
                    0
                } else {
                    u64::from(meta.content_len)
                })
                > MAX_OPERATION_LIVE
        {
            return Err(corrupt());
        }
        if bytes > MAX_LIVE {
            return Err(corrupt());
        }
        Ok(())
    }
    fn live(&self) -> HashSet<ConversationId> {
        self.current
            .values()
            .filter(|m| !m.tombstone && m.content_len > 0)
            .map(|m| ConversationId::new(m.content_hash))
            .collect()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn account_document_index_reopen_counts_physical_encrypted_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let path = dir
            .path()
            .join(format!("account-documents-{}.log", account.account_id()));
        let open =
            || AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
        let check = |store: &AccountDocumentStore| {
            assert_eq!(
                store.journal_bytes as u64,
                std::fs::metadata(&path).unwrap().len(),
                "index budget must count physical encrypted bytes after reopen or compaction"
            );
        };
        let mut store = open();
        check(&store);
        drop(store);
        store = open();
        check(&store);
        for revision in 1..=4 {
            if revision == 4 {
                store.compact_next_for_test();
            }
            let payload = format!("draft revision {revision}");
            let meta = AccountDocumentMeta::new(
                &account,
                "a".repeat(32),
                AccountDocumentKey::Draft {
                    conversation: DocumentConversation::Account("b".repeat(32)),
                },
                revision,
                payload.as_bytes(),
                false,
            );
            assert!(store.merge(meta, payload.as_bytes()).unwrap());
            check(&store);
            drop(store);
            store = open();
            check(&store);
        }
    }

    #[test]
    fn ordinary_index_rows_remain_exact_raw_metadata_json() {
        let account = Account::generate();
        let meta = AccountDocumentMeta::new(
            &account,
            "a".repeat(32),
            AccountDocumentKey::Draft {
                conversation: DocumentConversation::Account("b".repeat(32)),
            },
            1,
            b"private draft",
            false,
        );
        let raw = serde_json::to_vec(&meta).unwrap();
        assert!(encode_row(&meta, None, false).unwrap() == raw);
        let row: LocalIndexRow = serde_json::from_slice(&raw).unwrap();
        assert!(row.meta == meta && row.receipt.is_none());
        // Match the storage owner's actual bincode/encrypted frame shape.
        assert_eq!(
            bincode::serialized_size(&raw).unwrap() as usize + 4 + 12 + 16,
            raw.len() + INDEX_FRAME
        );
    }
    #[test]
    fn publication_receipts_bind_exact_metadata_and_reject_invalid_frontiers() {
        let account = Account::generate();
        let device = DeviceIdentity::generate().public();
        let meta = AccountDocumentMeta::new(
            &account,
            device.user_id(),
            AccountDocumentKey::GroupControl {
                channel: hex::encode([1; 32]),
                source_device: device.user_id(),
            },
            9,
            b"signed head placeholder for local record scope",
            false,
        );
        let receipt = LocalPublicationReceipt {
            version: 1,
            account_public: meta.account_public,
            source: device.clone(),
            group: [1; 32],
            revision: meta.revision,
            content_hash: meta.content_hash,
            frontier_hex: hex::encode([2; 32]),
        };
        assert!(receipt.matches(&meta, &device));
        assert_eq!(receipt.frontier().unwrap(), vec![EventId::new([2; 32])]);
        for field in 0..6 {
            let mut wrong = receipt.clone();
            match field {
                0 => wrong.account_public[0] ^= 1,
                1 => wrong.source.x25519_pub[0] ^= 1,
                2 => wrong.group[0] ^= 1,
                3 => wrong.revision += 1,
                4 => wrong.content_hash[0] ^= 1,
                _ => wrong.version += 1,
            }
            assert!(!wrong.matches(&meta, &device));
        }
        for frontier in [
            String::new(),
            "00".into(),
            hex::encode([2; 32]).repeat(2),
            "00".repeat((MAX_LOCAL_FRONTIER + 1) * 32),
        ] {
            let mut wrong = receipt.clone();
            wrong.frontier_hex = frontier;
            assert!(wrong.frontier().is_none());
        }
        let ordinary = encode_row(&meta, None, false).unwrap();
        let augmented = encode_row(&meta, Some(&receipt), false).unwrap();
        assert!(augmented.len() > ordinary.len());
        let parsed: LocalIndexRow = serde_json::from_slice(&augmented).unwrap();
        assert!(parsed.meta == meta && parsed.receipt.is_some());
    }
    #[test]
    fn group_control_quota_is_separate_and_existing_heads_can_advance_at_capacity() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let mut store =
            AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
        let writer = "a".repeat(32);
        let key = AccountDocumentKey::GroupControl {
            channel: "1".repeat(64),
            source_device: writer.clone(),
        };
        let meta = AccountDocumentMeta::new(&account, writer.clone(), key.clone(), 1, &[], false);
        store.counts[AccountDocumentDomain::Draft as usize] = MAX_ORDINARY_KEYS;
        store.counts[AccountDocumentDomain::ReadReceipt as usize] = MAX_MESSAGE_KEYS;
        store.counts[AccountDocumentDomain::ChannelCustody as usize] = MAX_MESSAGE_KEYS;
        store.counts[AccountDocumentDomain::GroupInventory as usize] = MAX_REFERENCE_KEYS;
        assert!(store.validate(&meta).is_ok());
        store.counts[AccountDocumentDomain::GroupControl as usize] = MAX_GROUP_CONTROL_KEYS;
        assert!(store.validate(&meta).is_err());
        store.counts[AccountDocumentDomain::GroupControl as usize] -= 1;
        store.install(meta);
        let updated = AccountDocumentMeta::new(&account, writer, key, 2, &[], false);
        assert!(
            store.validate(&updated).is_ok(),
            "a head revision must not allocate a new document key"
        );
    }
    #[test]
    fn group_inventory_quota_is_independent_and_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let mut store =
            AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
        let writer = "a".repeat(32);
        let key = AccountDocumentKey::GroupInventory {
            channel: "1".repeat(64),
            source_device: writer.clone(),
        };
        let meta = AccountDocumentMeta::new(&account, writer, key, 1, &[], true);
        store.counts[AccountDocumentDomain::Draft as usize] = MAX_ORDINARY_KEYS;
        store.counts[AccountDocumentDomain::ReadReceipt as usize] = MAX_MESSAGE_KEYS;
        store.counts[AccountDocumentDomain::ChannelCustody as usize] = MAX_MESSAGE_KEYS;
        store.counts[AccountDocumentDomain::GroupControl as usize] = MAX_GROUP_CONTROL_KEYS;
        assert!(
            store.validate(&meta).is_ok(),
            "other domains must not exhaust reference quota"
        );
        store.counts[AccountDocumentDomain::GroupInventory as usize] = MAX_REFERENCE_KEYS;
        assert!(store.validate(&meta).is_err());
        assert_eq!(
            MAX_OWNED_BLOBS,
            MAX_ORDINARY_KEYS
                + MAX_MESSAGE_KEYS
                + MAX_REFERENCE_KEYS
                + MAX_GROUP_CONTROL_KEYS
                + MAX_GROUP_OPERATION_KEYS
        );
    }

    #[test]
    fn channel_custody_quota_is_independent_and_blob_scan_accepts_legal_history() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let mut store =
            AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
        let key = AccountDocumentKey::ChannelCustody {
            channel: "1".repeat(64),
            event: "2".repeat(64),
            holder: "3".repeat(32),
        };
        let meta = AccountDocumentMeta::new(&account, "a".repeat(32), key, 1, &[], true);
        store.counts[AccountDocumentDomain::Draft as usize] = MAX_ORDINARY_KEYS;
        store.counts[AccountDocumentDomain::ReadReceipt as usize] = MAX_MESSAGE_KEYS;
        assert!(
            store.validate(&meta).is_ok(),
            "other domains cannot exhaust custody capacity"
        );
        store.counts[AccountDocumentDomain::ChannelCustody as usize] = MAX_MESSAGE_KEYS;
        assert!(
            store.validate(&meta).is_err(),
            "custody must retain its own finite cap"
        );
        drop(store);
        let media = dir
            .path()
            .join(format!("account-document-blobs-{}", account.account_id()))
            .join("media");
        // Legal histories can exceed the former arbitrary 8192-directory-entry
        // limit. Magic-prefixed orphans avoid triggering unrelated plaintext migration.
        for index in 0..8193 {
            std::fs::write(media.join(format!("{index:064x}.bin")), b"MTMEDIA1").unwrap();
        }
        assert!(
            AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).is_ok()
        );
    }

    #[test]
    fn ordinary_and_seen_metadata_caps_are_independent_and_never_evict_rows() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let mut store =
            AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
        for i in 0..4096 {
            let key = AccountDocumentKey::Draft {
                conversation: DocumentConversation::Account(format!("{i:032x}")),
            };
            store.install(AccountDocumentMeta::new(
                &account,
                "a".repeat(32),
                key,
                1,
                &[],
                true,
            ));
        }
        let new = AccountDocumentKey::Draft {
            conversation: DocumentConversation::Account("f".repeat(32)),
        };
        assert!(
            store
                .put(&account, "a".repeat(32), new, b"x", None, false)
                .is_err()
        );
        for i in 0..65536 {
            let key = AccountDocumentKey::ReadReceipt {
                conversation: DocumentConversation::Account("f".repeat(32)),
                message_id: format!("message-{i}"),
            };
            store.install(AccountDocumentMeta::new(
                &account,
                "a".repeat(32),
                key,
                1,
                &[],
                false,
            ));
        }
        let key = AccountDocumentKey::ReadReceipt {
            conversation: DocumentConversation::Account("f".repeat(32)),
            message_id: "another".into(),
        };
        assert!(
            store
                .put(&account, "a".repeat(32), key, &[], None, false)
                .is_err()
        );
        assert_eq!(store.current.len(), 4096 + 65536);
    }
    #[test]
    fn payload_and_aggregate_live_asset_budget_reject_without_overwriting() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let mut store =
            AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
        let mut payload = vec![3; 12 * 1024 * 1024];
        let first = store
            .put(
                &account,
                "a".repeat(32),
                AccountDocumentKey::CustomizationPack {
                    pack_id: "pack-0".into(),
                },
                &payload,
                None,
                false,
            )
            .unwrap();
        // The budget counts logical bytes per live key, including shared blobs.
        // Seed valid signed rows referencing the real maximum-size blob instead
        // of encrypting another 108 MiB just to establish the admission boundary.
        for i in 1..10 {
            let mut meta = first.clone();
            meta.key = AccountDocumentKey::CustomizationPack {
                pack_id: format!("pack-{i}"),
            };
            meta.signature = account.sign(&meta.message()).to_vec();
            store.validate(&meta).unwrap();
            store.install(meta);
        }
        assert_eq!(store.live_bytes, 120 * 1024 * 1024);
        let tail = AccountDocumentMeta::new(
            &account,
            "a".repeat(32),
            AccountDocumentKey::CustomizationPack {
                pack_id: "pack-tail".into(),
            },
            1,
            &vec![4; 8 * 1024 * 1024],
            false,
        );
        store.validate(&tail).unwrap();
        store.install(tail);
        assert_eq!(store.live_bytes, MAX_LIVE);
        let one_byte_over = AccountDocumentMeta::new(
            &account,
            "a".repeat(32),
            AccountDocumentKey::CustomizationPack {
                pack_id: "one-byte-over".into(),
            },
            1,
            b"x",
            false,
        );
        assert!(store.validate(&one_byte_over).is_err());
        let before_rows = store.current.clone();
        let before = store.digest(AccountDocumentDomain::CustomizationPack);
        let key = AccountDocumentKey::CustomizationPack {
            pack_id: "overflow".into(),
        };
        assert!(
            store
                .put(&account, "a".repeat(32), key.clone(), &payload, None, false)
                .is_err()
        );
        assert!(store.meta(&key).is_none());
        assert_eq!(store.current, before_rows);
        assert_eq!(store.live_bytes, MAX_LIVE);
        assert_eq!(
            store.digest(AccountDocumentDomain::CustomizationPack),
            before
        );
        payload.push(0);
        assert!(
            store
                .put(&account, "a".repeat(32), key, &payload, None, false)
                .is_err()
        );
        assert!(
            store
                .put(
                    &account,
                    "a".repeat(32),
                    AccountDocumentKey::AvatarOverride {
                        target: DocumentConversation::Account("a".repeat(32))
                    },
                    &vec![0; 1024 * 1024 + 1],
                    None,
                    false
                )
                .is_err()
        );
        assert_eq!(store.current, before_rows);
        assert_eq!(store.live_bytes, MAX_LIVE);
        assert_eq!(
            store.digest(AccountDocumentDomain::CustomizationPack),
            before
        );
        let retained = store.get(&first.key).unwrap().unwrap();
        assert!(
            retained.payload.as_slice() == &payload[..payload.len() - 1],
            "failed admissions must preserve the persisted maximum-size payload"
        );
    }
    #[test]
    fn startup_cleans_only_document_orphans_and_managed_partial_files() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let store =
            AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
        let hash = [7; 32];
        store
            .media
            .store_bytes(ConversationId::new(hash), "document.bin", b"unpublished")
            .unwrap();
        let own = dir
            .path()
            .join(format!("account-document-blobs-{}", account.account_id()))
            .join("media");
        let partial = own.join(format!("{}.bin.part", "a".repeat(64)));
        std::fs::write(&partial, b"interrupted ciphertext").unwrap();
        let unrelated = dir.path().join("unrelated-media");
        std::fs::write(&unrelated, b"preserve").unwrap();
        drop(store);
        let store =
            AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub).unwrap();
        assert!(store.media.path(ConversationId::new(hash)).is_none());
        assert!(!partial.exists());
        assert_eq!(std::fs::read(unrelated).unwrap(), b"preserve");
    }
}
