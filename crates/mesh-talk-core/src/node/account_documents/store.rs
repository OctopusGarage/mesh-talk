use super::*;
use crate::{
    eventlog::{LogError, event::ConversationId},
    node::media_store::MediaStore,
    storage::record_log::EncryptedRecordLog,
};
use std::{
    collections::{BTreeMap, BTreeSet, HashSet},
    path::Path,
};
const MAX_LIVE: u64 = 128 * 1024 * 1024;
pub(in crate::node) struct AccountDocumentStore {
    file: EncryptedRecordLog<Vec<u8>>,
    media: MediaStore,
    current: BTreeMap<AccountDocumentKey, AccountDocumentMeta>,
    account: [u8; 32],
    records: usize,
    journal_bytes: usize,
    counts: [usize; 6],
    live_bytes: u64,
    digest_xor: [[u8; 32]; 6],
    incomplete: BTreeSet<AccountDocumentKey>,
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
                    if i >= 8192 {
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
        let journal_bytes = rows.iter().map(|r| r.len() + 32).sum();
        let mut store = Self {
            file,
            media,
            current: BTreeMap::new(),
            account: *account,
            records: rows.len(),
            journal_bytes,
            counts: [0; 6],
            live_bytes: 0,
            digest_xor: [[0; 32]; 6],
            incomplete: BTreeSet::new(),
        };
        for bytes in rows {
            if bytes.len() > 8192 {
                return Err(corrupt());
            }
            let row: AccountDocumentMeta = serde_json::from_slice(&bytes).map_err(|_| corrupt())?;
            store.validate(&row)?;
            if store
                .current
                .get(&row.key)
                .is_none_or(|old| old.order() < row.order())
            {
                store.install(row);
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
    pub(super) fn meta(&self, key: &AccountDocumentKey) -> Option<AccountDocumentMeta> {
        self.current.get(key).cloned()
    }
    pub(super) fn list(
        &self,
        domain: AccountDocumentDomain,
        after: Option<&AccountDocumentKey>,
        limit: usize,
    ) -> Vec<AccountDocumentMeta> {
        self.current
            .values()
            .filter(|m| m.key.domain() == domain && after.is_none_or(|a| &m.key > a))
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
    fn install(&mut self, meta: AccountDocumentMeta) {
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
    pub(super) fn merge(
        &mut self,
        meta: AccountDocumentMeta,
        payload: &[u8],
    ) -> Result<bool, LogError> {
        self.validate(&meta)?;
        if meta.key.is_private_control() && !super::controls::valid_payload(&meta, payload) {
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
            if old == &meta && self.incomplete.contains(&meta.key) {
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
        if meta.content_len > 0 && !meta.tombstone {
            self.media
                .store_bytes(
                    ConversationId::new(meta.content_hash),
                    "document.bin",
                    payload,
                )
                .map_err(LogError::Io)?;
        }
        let bytes = serde_json::to_vec(&meta).map_err(|_| corrupt())?;
        let old_hash = self
            .current
            .get(&meta.key)
            .filter(|m| !m.tombstone && m.content_len > 0)
            .map(|m| m.content_hash);
        let commit = (|| {
            if self.records >= self.current.len().saturating_mul(2).saturating_add(32)
                || self.journal_bytes + bytes.len() + 32 > 120 * 1024 * 1024
            {
                let rows: Result<Vec<_>, _> =
                    self.current.values().map(serde_json::to_vec).collect();
                let rows = rows.map_err(|_| corrupt())?;
                self.file.rewrite(&rows)?;
                self.records = self.current.len();
                self.journal_bytes = rows.iter().map(|r| r.len() + 32).sum();
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
        self.journal_bytes += bytes.len() + 32;
        self.records += 1;
        self.install(meta);
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
        let count = if seen {
            self.counts[AccountDocumentDomain::ReadReceipt as usize]
        } else {
            self.counts.iter().sum::<usize>()
                - self.counts[AccountDocumentDomain::ReadReceipt as usize]
        };
        if !self.current.contains_key(&meta.key) && count >= if seen { 65536 } else { 4096 } {
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
mod budget_tests {
    use super::*;
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
        for i in 0..10 {
            payload[0] = i;
            store
                .put(
                    &account,
                    "a".repeat(32),
                    AccountDocumentKey::CustomizationPack {
                        pack_id: format!("pack-{i}"),
                    },
                    &payload,
                    None,
                    false,
                )
                .unwrap();
        }
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
