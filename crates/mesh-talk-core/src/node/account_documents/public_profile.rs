//! Original signed contact profiles, privately relayed through certified siblings.
//!
//! A source retains its own document head for each account. Document generations
//! order that source's snapshot; the original account signature and profile owner
//! determine which public presentation wins across sources. Private avatar
//! overrides and own-account profile publication retain their existing owners.
use super::*;
use crate::eventlog::LogError;
use crate::node::AccountProfile;
use rusqlite::{OptionalExtension, params};

pub(super) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    !meta.tombstone
        && matches!(&meta.key,
            AccountDocumentKey::PublicProfile { account, source_device }
                if source_device == &meta.writer_device
                    && AccountProfile::decode(payload)
                        .is_some_and(|profile| profile.account_id() == *account))
}

// Derived exact completion receipts. The canonical profile and signed document
// owners remain authoritative; a missing receipt is pending work after restart.
const RECONCILIATION_SCHEMA: &str = "
CREATE TABLE public_profile_reconciliation(owner BLOB NOT NULL CHECK(length(owner)=32),device BLOB NOT NULL CHECK(length(device)=32),direction INTEGER NOT NULL CHECK(direction IN(0,1)),account TEXT NOT NULL CHECK(length(account)=32),source_device TEXT NOT NULL CHECK(length(source_device)=32),version BLOB NOT NULL CHECK(length(version)=8),content_hash BLOB NOT NULL CHECK(length(content_hash)=32),PRIMARY KEY(owner,device,direction,account,source_device)) WITHOUT ROWID;
";

struct PublicProfileReceipt {
    version: Vec<u8>,
    content_hash: Vec<u8>,
}

impl Node {
    pub(in crate::node) fn initialize_public_profile_reconciliation(&self) -> Result<(), LogError> {
        self.storage.database().transaction(|c| {
            let exists: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='public_profile_reconciliation')",
                [], |row| row.get(0),
            )?;
            if !exists {
                c.execute_batch(RECONCILIATION_SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, RECONCILIATION_SCHEMA)
        })
    }

    fn public_profile_receipt(
        &self,
        direction: i64,
        account: &str,
        source: &str,
    ) -> Result<Option<PublicProfileReceipt>, LogError> {
        self.storage.database().read(|c| {
            c.query_row(
                "SELECT version,content_hash FROM public_profile_reconciliation WHERE owner=?1 AND device=?2 AND direction=?3 AND account=?4 AND source_device=?5",
                params![self.account.public().ed25519_pub.as_slice(), self.identity.public().ed25519_pub.as_slice(), direction, account, source],
                |row| Ok(PublicProfileReceipt { version: row.get(0)?, content_hash: row.get(1)? }),
            ).optional().map_err(Into::into)
        })
    }

    fn acknowledge_public_profile(
        &self,
        direction: i64,
        account: &str,
        source: &str,
        version: u64,
        hash: &[u8; 32],
    ) -> Result<(), LogError> {
        self.storage.database().transaction(|c| {
            c.execute(
                "INSERT INTO public_profile_reconciliation VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(owner,device,direction,account,source_device) DO UPDATE SET version=excluded.version,content_hash=excluded.content_hash",
                params![self.account.public().ed25519_pub.as_slice(), self.identity.public().ed25519_pub.as_slice(), direction, account, source, version.to_be_bytes().as_slice(), hash.as_slice()],
            )?;
            Ok(())
        })
    }

    #[cfg(test)]
    pub(in crate::node) fn publish_public_profile_documents(&self) -> Result<(), NodeError> {
        let mut after = None;
        loop {
            self.publish_public_profile_documents_page(&mut after)?;
            if after.is_none() {
                return Ok(());
            }
        }
    }

    /// Repair at most sixteen selected profiles per turn. Unchanged current
    /// revisions and document heads are checked before loading any avatar.
    pub(in crate::node) fn publish_public_profile_documents_page(
        &self,
        after: &mut Option<String>,
    ) -> Result<(), NodeError> {
        let accounts = self
            .profiles
            .lock()
            .expect("profiles mutex")
            .names_page(after.as_deref())?;
        if accounts.is_empty() {
            *after = None;
            return Ok(());
        }
        let exhausted = accounts.len() <= 16;
        for (account, _) in accounts.into_iter().take(16) {
            *after = Some(account.clone());
            self.publish_public_profile_document(&account)?;
        }
        if exhausted {
            *after = None;
        }
        Ok(())
    }

    /// Profile lock fences the accepted revision through document installation
    /// and receipt acknowledgement. Interrupted writes retain the old receipt.
    pub(in crate::node) fn publish_public_profile_document(
        &self,
        account: &str,
    ) -> Result<(), NodeError> {
        if account == self.account_id() {
            return Ok(());
        }
        let (changed, acknowledgement) = {
            let profiles = self.profiles.lock().expect("profiles mutex not poisoned");
            let Some(change) = profiles.change_revision(account)? else {
                return Ok(());
            };
            let source = self.user_id();
            let key = AccountDocumentKey::PublicProfile {
                account: account.to_owned(),
                source_device: source.clone(),
            };
            let mut documents = self.account_documents.lock().expect("documents mutex");
            let previous = documents.meta(&key);
            if let Some(receipt) = self.public_profile_receipt(0, account, &source)?
                && receipt.version == change.to_be_bytes()
                && previous.as_ref().is_some_and(|meta| {
                    !meta.tombstone && receipt.content_hash == meta.content_hash
                })
            {
                return Ok(());
            }
            let Some(profile) = profiles.get(account)? else {
                return Ok(());
            };
            let payload = profile.encode();
            let hash = content_hash(&payload);
            let changed = if previous
                .as_ref()
                .is_some_and(|meta| !meta.tombstone && meta.content_hash == hash)
            {
                false
            } else {
                let revision = previous
                    .map_or(0, |meta| meta.revision)
                    .checked_add(1)
                    .ok_or_else(invalid)?
                    .max(super::super::node::now_millis());
                let meta = AccountDocumentMeta::new(
                    &self.account,
                    source.clone(),
                    key.clone(),
                    revision,
                    &payload,
                    false,
                );
                documents.merge(meta, &payload).map_err(NodeError::Log)?
            };
            let acknowledgement = (|| {
                self.acknowledge_public_profile(0, account, &source, change, &hash)?;
                // Normal admission already installed the compatibility avatar. A
                // publication of a pending older import must still be projected.
                if profiles.projection_revision(account)?.is_none()
                    && let Some(meta) = documents.meta(&key)
                {
                    self.acknowledge_public_profile(1, account, &source, meta.revision, &hash)?;
                }
                Ok::<_, LogError>(())
            })();
            (changed, acknowledgement)
        };
        // The document is already durable even if receipt repair fails.
        if changed {
            self.documents_changed();
        }
        acknowledgement?;
        Ok(())
    }

    /// Explicit profile-policy reenabling retains its existing synchronous
    /// repair contract, composed from the same bounded runtime turns.
    pub(in crate::node) fn project_public_profile_documents(&self) -> Result<(), NodeError> {
        let mut after = None;
        loop {
            self.project_public_profile_documents_page(&mut after)?;
            if after.is_none() {
                return Ok(());
            }
        }
    }

    /// Missing receipts cover cold migration and interrupted projection. The
    /// metadata cursor wraps, so a failure cannot lose work behind later heads.
    pub(in crate::node) fn project_public_profile_documents_page(
        &self,
        after: &mut Option<AccountDocumentKey>,
    ) -> Result<(), NodeError> {
        if !self.device_sync_policy().profile {
            *after = None;
            return Ok(());
        }
        let page =
            self.account_documents(AccountDocumentDomain::PublicProfile, after.as_ref(), 16)?;
        for meta in &page.items {
            *after = Some(meta.key.clone());
            let AccountDocumentKey::PublicProfile {
                account,
                source_device,
            } = &meta.key
            else {
                return Err(invalid());
            };
            if let Some(receipt) = self.public_profile_receipt(1, account, source_device)?
                && receipt.version == meta.revision.to_be_bytes()
                && receipt.content_hash == meta.content_hash
            {
                continue;
            }
            let document = match self.account_document(&meta.key) {
                Ok(Some(document)) => document,
                Ok(None) => continue,
                Err(error) => {
                    log::debug!("contact profile projection deferred: {error}");
                    continue;
                }
            };
            if !valid_payload(&document.meta, document.payload()) {
                log::debug!("invalid retained contact profile ignored");
                continue;
            }
            let profile = AccountProfile::decode(document.payload()).ok_or_else(invalid)?;
            if profile.account_id() != self.account_id() {
                self.receive_account_profile(&profile)?;
            }
            // A replacement that raced the selected payload stays pending.
            let documents = self.account_documents.lock().expect("documents mutex");
            if documents.meta(&document.meta.key).as_ref() == Some(&document.meta) {
                let AccountDocumentKey::PublicProfile {
                    account,
                    source_device,
                } = &document.meta.key
                else {
                    return Err(invalid());
                };
                self.acknowledge_public_profile(
                    1,
                    account,
                    source_device,
                    document.meta.revision,
                    &document.meta.content_hash,
                )?;
            }
        }
        *after = page.next;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn foreign_profile(account: &Account, revision: u64, avatar: Option<&[u8]>) -> AccountProfile {
        AccountProfile::new(
            account,
            revision,
            "Contact".into(),
            "Signed biography".into(),
            avatar.map(<[u8]>::to_vec),
        )
    }

    fn copy_documents(source: &Node, receiver: &Node) {
        let page = source
            .account_documents(AccountDocumentDomain::PublicProfile, None, 64)
            .unwrap();
        for meta in page.items {
            let document = source.account_document(&meta.key).unwrap().unwrap();
            receiver
                .account_documents
                .lock()
                .unwrap()
                .merge(meta, document.payload())
                .unwrap();
        }
    }

    #[test]
    fn periodic_publication_retries_failed_receipts_without_losing_the_wake() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Account::generate();
        let node = super::super::tests::node(dir.path(), &owner);
        let foreign = Account::generate();
        let account = foreign.account_id();
        node.profiles
            .lock()
            .unwrap()
            .record(foreign_profile(&foreign, 10, Some(b"photo")))
            .unwrap();
        let changed = node.subscribe_account_documents();
        node.storage.database().transaction(|c| {
            c.execute_batch("CREATE TEMP TRIGGER fail_public_profile_receipt BEFORE INSERT ON public_profile_reconciliation WHEN NEW.direction=0 BEGIN SELECT RAISE(ABORT,'receipt failed'); END;")?;
            Ok(())
        }).unwrap();
        assert!(node.publish_public_profile_document(&account).is_err());
        assert!(
            changed.has_changed().unwrap(),
            "durable publication must still wake consumers"
        );
        assert_eq!(
            node.account_documents(AccountDocumentDomain::PublicProfile, None, 64)
                .unwrap()
                .items
                .len(),
            1
        );
        assert!(
            node.public_profile_receipt(0, &account, &node.user_id())
                .unwrap()
                .is_none()
        );
        node.storage
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER fail_public_profile_receipt")?;
                Ok(())
            })
            .unwrap();
        node.publish_public_profile_document(&account).unwrap();
        assert!(
            node.public_profile_receipt(0, &account, &node.user_id())
                .unwrap()
                .is_some()
        );
        node.project_public_profile_documents_page(&mut None)
            .unwrap();
        crate::node::account_profile_data::reset_indexed_body_reads();
        node.publish_public_profile_documents_page(&mut None)
            .unwrap();
        node.project_public_profile_documents_page(&mut None)
            .unwrap();
        assert_eq!(
            crate::node::account_profile_data::indexed_body_reads(),
            0,
            "unchanged repair turns must remain metadata-only"
        );
    }

    #[test]
    fn periodic_projection_bounds_cold_repair_and_retries_failed_heads() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Account::generate();
        let source = super::super::tests::node(&dir.path().join("source"), &owner);
        let receiver = super::super::tests::node(&dir.path().join("receiver"), &owner);
        for _ in 0..18 {
            source
                .receive_account_profile(&foreign_profile(&Account::generate(), 10, Some(b"photo")))
                .unwrap();
        }
        copy_documents(&source, &receiver);
        receiver.account_profiles.fail_appends_for_test(true);
        let mut after = None;
        assert!(
            receiver
                .project_public_profile_documents_page(&mut after)
                .is_err()
        );
        receiver.account_profiles.fail_appends_for_test(false);
        let (ed25519, x25519) = receiver.identity.secret_bytes();
        drop(receiver);
        let receiver = super::super::tests::node_with_identity(
            &dir.path().join("receiver"),
            &owner,
            DeviceIdentity::from_secret_bytes(ed25519, x25519),
        );
        // The durable document and missing receipt survive the failed write;
        // a new runtime starts its bounded metadata cursor from the first head.
        after = None;
        receiver
            .project_public_profile_documents_page(&mut after)
            .unwrap();
        assert!(after.is_some());
        assert_eq!(
            receiver
                .profiles
                .lock()
                .unwrap()
                .names_page(None)
                .unwrap()
                .len(),
            16
        );
        for _ in 0..8 {
            receiver
                .project_public_profile_documents_page(&mut after)
                .unwrap();
            if after.is_none() {
                break;
            }
        }
        assert!(after.is_none());
        assert_eq!(
            receiver
                .profiles
                .lock()
                .unwrap()
                .names_page(None)
                .unwrap()
                .len(),
            18
        );
        crate::node::account_profile_data::reset_indexed_body_reads();
        receiver.project_public_profile_documents().unwrap();
        assert_eq!(crate::node::account_profile_data::indexed_body_reads(), 0);
    }

    #[test]
    fn original_signature_account_and_source_are_required() {
        let owner = Account::generate();
        let foreign = Account::generate();
        let writer = DeviceIdentity::generate().public().user_id();
        let profile = foreign_profile(&foreign, 10, Some(b"photo"));
        let key = AccountDocumentKey::PublicProfile {
            account: foreign.account_id(),
            source_device: writer.clone(),
        };
        let payload = profile.encode();
        let meta =
            AccountDocumentMeta::new(&owner, writer.clone(), key.clone(), 1, &payload, false);
        assert!(valid_payload(&meta, &payload));
        let mut forged = profile;
        forged.avatar = Some(b"forged".to_vec());
        assert!(!valid_payload(&meta, &forged.encode()));
        let wrong_account = AccountDocumentMeta::new(
            &owner,
            writer.clone(),
            AccountDocumentKey::PublicProfile {
                account: owner.account_id(),
                source_device: writer.clone(),
            },
            1,
            &payload,
            false,
        );
        assert!(!valid_payload(&wrong_account, &payload));
        let wrong_writer = AccountDocumentMeta::new(
            &owner,
            DeviceIdentity::generate().public().user_id(),
            key.clone(),
            1,
            &payload,
            false,
        );
        assert!(!valid_payload(&wrong_writer, &payload));
        let tombstone = AccountDocumentMeta::new(&owner, writer, key, 2, &[], true);
        assert!(!valid_payload(&tombstone, &[]));
    }

    #[test]
    fn periodic_publication_repairs_at_most_sixteen_profiles_per_turn() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Account::generate();
        let node = super::super::tests::node(dir.path(), &owner);
        // Seed accepted canonical records before their optional publication, as
        // after an interrupted document write or an older binary's startup.
        for _ in 0..18 {
            let foreign = Account::generate();
            node.profiles
                .lock()
                .unwrap()
                .record(foreign_profile(&foreign, 10, Some(b"photo")))
                .unwrap();
        }
        let mut after = None;
        node.publish_public_profile_documents_page(&mut after)
            .unwrap();
        assert!(after.is_some());
        assert_eq!(
            node.account_documents(AccountDocumentDomain::PublicProfile, None, 64)
                .unwrap()
                .items
                .len(),
            16
        );
        node.publish_public_profile_documents_page(&mut after)
            .unwrap();
        assert!(after.is_none());
        let complete = node
            .account_documents(AccountDocumentDomain::PublicProfile, None, 64)
            .unwrap();
        assert_eq!(complete.items.len(), 18);
        node.publish_public_profile_documents_page(&mut after)
            .unwrap();
        assert_eq!(
            node.account_documents(AccountDocumentDomain::PublicProfile, None, 64)
                .unwrap()
                .items,
            complete.items
        );
    }

    #[test]
    fn periodic_publication_finishes_an_exact_sixteen_profile_page() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Account::generate();
        let node = super::super::tests::node(dir.path(), &owner);
        for _ in 0..16 {
            let foreign = Account::generate();
            node.profiles
                .lock()
                .unwrap()
                .record(foreign_profile(&foreign, 10, Some(b"photo")))
                .unwrap();
        }
        let mut after = None;
        node.publish_public_profile_documents_page(&mut after)
            .unwrap();
        assert!(after.is_none(), "a full final page completes the pass");
        let complete = node
            .account_documents(AccountDocumentDomain::PublicProfile, None, 64)
            .unwrap();
        assert_eq!(complete.items.len(), 16);
        node.publish_public_profile_documents_page(&mut after)
            .unwrap();
        assert!(after.is_none());
        assert_eq!(
            node.account_documents(AccountDocumentDomain::PublicProfile, None, 64)
                .unwrap()
                .items,
            complete.items,
            "starting the next pass must preserve existing signed document heads"
        );
    }

    #[test]
    fn sibling_snapshot_preserves_foreign_signature_clear_and_restart_without_overrides() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Account::generate();
        let foreign = Account::generate();
        let source = super::super::tests::node(&dir.path().join("source"), &owner);
        let receiver_path = dir.path().join("receiver");
        let receiver = super::super::tests::node(&receiver_path, &owner);
        let photo = foreign_profile(&foreign, 10, Some(b"contact photo"));
        source.receive_account_profile(&photo).unwrap();
        let others = [Account::generate(), Account::generate()].map(|account| {
            let profile = foreign_profile(&account, 10, Some(b"other contact"));
            source.receive_account_profile(&profile).unwrap();
            profile
        });
        source.publish_public_profile_documents().unwrap();
        let first = source
            .account_documents(AccountDocumentDomain::PublicProfile, None, 64)
            .unwrap();
        assert_eq!(first.items.len(), 3);
        source.publish_public_profile_documents().unwrap();
        assert_eq!(
            first.items,
            source
                .account_documents(AccountDocumentDomain::PublicProfile, None, 64)
                .unwrap()
                .items
        );
        copy_documents(&source, &receiver);
        receiver.project_public_profile_documents().unwrap();
        assert_eq!(
            receiver.account_profile(&foreign.account_id()).unwrap(),
            Some(photo.clone())
        );
        assert_eq!(
            receiver.peer_avatar(&foreign.account_id()).unwrap(),
            photo.avatar
        );
        for profile in &others {
            assert_eq!(
                receiver.account_profile(&profile.account_id()).unwrap(),
                Some(profile.clone())
            );
        }
        assert!(
            receiver
                .account_documents(AccountDocumentDomain::AvatarOverride, None, 64)
                .unwrap()
                .items
                .is_empty()
        );
        // A second source's stale signed snapshot cannot resurrect a cleared photo.
        let stale = super::super::tests::node(&dir.path().join("stale"), &owner);
        stale.receive_account_profile(&photo).unwrap();
        stale.publish_public_profile_documents().unwrap();
        // Equal signed revisions retain the public owner's clear-first order,
        // independently of each source document's generation and writer order.
        let clear = foreign_profile(&foreign, 10, None);
        source.receive_account_profile(&clear).unwrap();
        source.publish_public_profile_documents().unwrap();
        copy_documents(&source, &receiver);
        copy_documents(&stale, &receiver);
        receiver.project_public_profile_documents().unwrap();
        assert_eq!(
            receiver.account_profile(&foreign.account_id()).unwrap(),
            Some(clear.clone())
        );
        assert_eq!(receiver.peer_avatar(&foreign.account_id()).unwrap(), None);
        let (device_ed25519, device_x25519) = receiver.identity.secret_bytes();
        drop(receiver);
        let reopened = {
            use crate::discovery::Roster;
            use std::sync::{Arc, Mutex};
            use tokio::sync::mpsc;
            let (dm, _) = mpsc::unbounded_channel();
            let (channel, _) = mpsc::unbounded_channel();
            let (file, _) = mpsc::unbounded_channel();
            Node::open_with_account(
                DeviceIdentity::from_secret_bytes(device_ed25519, device_x25519),
                Account::from_secret_bytes(owner.secret_bytes()),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &receiver_path.join("events.log"),
                &receiver_path.join("sent.log"),
                "pw",
            )
            .unwrap()
        };
        reopened.project_public_profile_documents().unwrap();
        reopened.project_public_profile_documents().unwrap();
        assert_eq!(
            reopened.account_profile(&foreign.account_id()).unwrap(),
            Some(clear)
        );
        for profile in others {
            assert_eq!(
                reopened.account_profile(&profile.account_id()).unwrap(),
                Some(profile)
            );
        }
    }

    #[test]
    fn paused_profile_projection_defers_import_and_own_publication_stays_separate() {
        let dir = tempfile::tempdir().unwrap();
        let owner = Account::generate();
        let foreign = Account::generate();
        let source = super::super::tests::node(&dir.path().join("source"), &owner);
        let receiver = super::super::tests::node(&dir.path().join("receiver"), &owner);
        source
            .receive_account_profile(&foreign_profile(&source.account, 10, Some(b"own")))
            .unwrap();
        assert!(
            source
                .account_profile(&source.account_id())
                .unwrap()
                .is_some()
        );
        let profile = foreign_profile(&foreign, 10, Some(b"contact"));
        source.receive_account_profile(&profile).unwrap();
        source.publish_public_profile_documents().unwrap();
        assert_eq!(
            source
                .account_documents(AccountDocumentDomain::PublicProfile, None, 64)
                .unwrap()
                .items
                .len(),
            1
        );
        copy_documents(&source, &receiver);
        receiver
            .set_device_sync_category(SyncCategory::Profile, false)
            .unwrap();
        receiver.project_public_profile_documents().unwrap();
        assert!(
            receiver
                .account_profile(&foreign.account_id())
                .unwrap()
                .is_none()
        );
        // All sources may now be offline; the category setter must project its
        // already retained signed snapshot without an explicit repair call.
        drop(source);
        receiver
            .set_device_sync_category(SyncCategory::Profile, true)
            .unwrap();
        assert_eq!(
            receiver.account_profile(&foreign.account_id()).unwrap(),
            Some(profile)
        );
    }
}
