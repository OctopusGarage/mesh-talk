//! Original signed contact profiles, privately relayed through certified siblings.
//!
//! A source retains its own document head for each account. Document generations
//! order that source's snapshot; the original account signature and profile owner
//! determine which public presentation wins across sources. Private avatar
//! overrides and own-account profile publication retain their existing owners.
use super::*;
use crate::node::AccountProfile;

pub(super) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    !meta.tombstone
        && matches!(&meta.key,
            AccountDocumentKey::PublicProfile { account, source_device }
                if source_device == &meta.writer_device
                    && AccountProfile::decode(payload)
                        .is_some_and(|profile| profile.account_id() == *account))
}

impl Node {
    /// Republish the bounded canonical signed store without creating new public
    /// signatures, importing private overrides, or borrowing a sibling's head.
    pub(in crate::node) fn publish_public_profile_documents(&self) -> Result<(), NodeError> {
        let mut after = None;
        loop {
            let accounts = self
                .profiles
                .lock()
                .expect("profiles mutex")
                .names_page(after.as_deref())?;
            if accounts.is_empty() {
                break;
            }
            for (account, _) in accounts {
                after = Some(account.clone());
                if let Err(error) = self.publish_public_profile_document(&account) {
                    log::debug!("contact profile publication deferred: {error}");
                }
            }
        }
        Ok(())
    }

    /// Repair at most sixteen selected profiles per turn, without avatar snapshots.
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
        // names_page selects up to 128 headers without body or byte-budget
        // truncation. A seventeenth header therefore keeps this cursor open;
        // exactly sixteen remaining headers completes the pass.
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

    /// Refresh only the changed account after public-profile admission. Keeping
    /// selection under the profile lock prevents a queued stale snapshot from
    /// replacing a newer local publication.
    pub(in crate::node) fn publish_public_profile_document(
        &self,
        account: &str,
    ) -> Result<(), NodeError> {
        if account == self.account_id() {
            return Ok(());
        }
        let changed = {
            // Projection releases document locks before entering the profile owner.
            let profiles = self.profiles.lock().expect("profiles mutex not poisoned");
            let Some(profile) = profiles.get(account)? else {
                return Ok(());
            };
            let mut documents = self.account_documents.lock().expect("documents mutex");
            let key = AccountDocumentKey::PublicProfile {
                account: profile.account_id(),
                source_device: self.user_id(),
            };
            let payload = profile.encode();
            let previous = documents.meta(&key);
            if previous
                .as_ref()
                .is_some_and(|meta| !meta.tombstone && meta.content_hash == content_hash(&payload))
            {
                return Ok(());
            }
            let revision = previous
                .map_or(0, |meta| meta.revision)
                .checked_add(1)
                .ok_or_else(invalid)?
                .max(super::super::node::now_millis());
            let meta = AccountDocumentMeta::new(
                &self.account,
                self.user_id(),
                key,
                revision,
                &payload,
                false,
            );
            documents.merge(meta, &payload).map_err(NodeError::Log)?
        };
        if changed {
            self.documents_changed();
        }
        Ok(())
    }

    /// Repair public projections after import or reopen. Replaying an unchanged
    /// snapshot is idempotent; stale signed profiles cannot resurrect an avatar.
    pub(in crate::node) fn project_public_profile_documents(&self) -> Result<(), NodeError> {
        if !self.device_sync_policy().profile {
            return Ok(());
        }
        let mut after = None;
        loop {
            let page =
                self.account_documents(AccountDocumentDomain::PublicProfile, after.as_ref(), 64)?;
            for meta in &page.items {
                let document = match self.account_document(&meta.key) {
                    Ok(Some(document)) => document,
                    Ok(None) => continue,
                    Err(error) => {
                        // Existing document reconciliation requests incomplete
                        // blobs again. Other retained profiles remain usable.
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
            }
            after = page.next;
            if after.is_none() {
                return Ok(());
            }
        }
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
