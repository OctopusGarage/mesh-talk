//! Device-local reclamation for offline accounts that never had durable history.
use super::*;

use crate::chat_commands::AccountInfo;
use std::{collections::BTreeMap, io::Read};

const EMPTY_CONTACT_LIFETIME_MS: u64 = 86_400_000;
const MAX_EXPIRY_CONTACTS: usize = 4096;
const MAX_EXPIRY_BYTES: u64 = 512 * 1024;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirstObservations {
    #[serde(default)]
    version: u8,
    contacts: BTreeMap<String, ObservedContact>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservedContact {
    /// None while online; otherwise the first local observation of this absence.
    #[serde(default, alias = "first_seen_ms")]
    offline_since_ms: Option<u64>,
    ever_history: bool,
    /// Read old journals without preserving their permanent expiry tombstones.
    #[serde(default, rename = "expired", skip_serializing)]
    _legacy_expired: bool,
}

type HistoryClassifier<'a> = dyn Fn(&[String]) -> Result<HashSet<String>, String> + 'a;

pub(crate) struct ContactExpiryInputs<'a> {
    pub ever_history: &'a HashSet<String>,
    pub online: &'a HashSet<String>,
    pub deleted: &'a HashSet<String>,
    pub history_for_absent: &'a HistoryClassifier<'a>,
}

impl HiddenContactsState {
    fn expiry_path(&self, owner: &str, own_account: &str) -> PathBuf {
        self.root
            .join("accounts")
            .join(owner)
            .join(format!("empty-contacts-{own_account}.json"))
    }

    /// The existing policy/session transaction owns observation and commit. No
    /// worker, polling loop or lock is added; the caller's roster refresh drives it.
    pub(crate) fn expire_empty_accounts(
        &self,
        owner: &str,
        own_account: &str,
        rows: &mut Vec<AccountInfo>,
        now_ms: u64,
        inputs: ContactExpiryInputs<'_>,
    ) -> Result<(), String> {
        authorize(Some(owner), owner)?;
        if !valid_account(own_account) {
            return Err("Invalid expiry account".into());
        }
        let path = self.expiry_path(owner, own_account);
        let mut journal = match std::fs::File::open(&path) {
            Ok(file) => {
                let mut bytes = Vec::new();
                file.take(MAX_EXPIRY_BYTES + 1)
                    .read_to_end(&mut bytes)
                    .map_err(|e| e.to_string())?;
                if bytes.len() as u64 > MAX_EXPIRY_BYTES {
                    return Err("Contact expiry journal too large".into());
                }
                serde_json::from_slice::<FirstObservations>(&bytes).map_err(|e| e.to_string())?
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => FirstObservations {
                version: 1,
                ..Default::default()
            },
            Err(error) => return Err(error.to_string()),
        };
        if journal.version > 1
            || journal.contacts.len() > MAX_EXPIRY_CONTACTS
            || journal.contacts.keys().any(|id| !valid_account(id))
        {
            return Err("Invalid contact expiry journal".into());
        }
        // Recheck absent pending contacts before reclamation too: a first
        // message may have committed and been cleared since the last roster read.
        let listed: HashSet<_> = rows.iter().map(|row| row.account_id.as_str()).collect();
        let absent: Vec<_> = journal
            .contacts
            .iter()
            .filter(|(account, observed)| {
                !observed.ever_history
                    && !listed.contains(account.as_str())
                    && !inputs.deleted.contains(*account)
            })
            .map(|(account, _)| account.clone())
            .collect();
        let mut ever_history = inputs.ever_history.clone();
        if !absent.is_empty() {
            ever_history.extend((inputs.history_for_absent)(&absent)?);
        }
        // Old first-observation clocks cannot establish when a contact went
        // offline. Start conservatively at this observation; online cancels it.
        let mut changed = journal.version == 0;
        if journal.version == 0 {
            journal.version = 1;
            for (account, observed) in &mut journal.contacts {
                observed.offline_since_ms = (!inputs.online.contains(account)).then_some(now_ms);
            }
        }
        for row in rows.iter() {
            if row.account_id == own_account
                || !valid_account(&row.account_id)
                || inputs.deleted.contains(&row.account_id)
                || ever_history.contains(&row.account_id)
                || !inputs.online.contains(&row.account_id)
                || journal.contacts.contains_key(&row.account_id)
                || journal.contacts.len() >= MAX_EXPIRY_CONTACTS
            {
                continue;
            }
            journal.contacts.insert(
                row.account_id.clone(),
                ObservedContact {
                    offline_since_ms: None,
                    ever_history: false,
                    _legacy_expired: false,
                },
            );
            changed = true;
        }
        // Include absent roster entries: losing all discovery routes must start
        // the offline grace period, not strand metadata forever.
        journal.contacts.retain(|account, observed| {
            if account == own_account || inputs.deleted.contains(account) {
                changed = true;
                return false;
            }
            if ever_history.contains(account) && !observed.ever_history {
                observed.ever_history = true;
                changed = true;
            }
            if observed.ever_history || inputs.online.contains(account) {
                if observed.offline_since_ms.take().is_some() {
                    changed = true;
                }
            } else {
                let since = *observed.offline_since_ms.get_or_insert_with(|| {
                    changed = true;
                    now_ms
                });
                if now_ms >= since.saturating_add(EMPTY_CONTACT_LIFETIME_MS) {
                    changed = true;
                    return false;
                }
            }
            true
        });
        if changed {
            let parent = path.parent().ok_or("Invalid expiry journal path")?;
            std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            let mut temporary =
                tempfile::NamedTempFile::new_in(parent).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                temporary
                    .as_file()
                    .set_permissions(std::fs::Permissions::from_mode(0o600))
                    .map_err(|e| e.to_string())?;
            }
            let bytes = serde_json::to_vec(&journal).map_err(|e| e.to_string())?;
            if bytes.len() as u64 > MAX_EXPIRY_BYTES {
                return Err("Contact expiry journal too large".into());
            }
            std::io::Write::write_all(&mut temporary, &bytes).map_err(|e| e.to_string())?;
            temporary.flush().map_err(|e| e.to_string())?;
            temporary.as_file().sync_all().map_err(|e| e.to_string())?;
            temporary.persist(&path).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            if std::fs::File::open(parent)
                .and_then(|directory| directory.sync_all())
                .is_err()
            {
                log::warn!("Contact expiry committed; directory durability sync unavailable");
            }
        }
        // Commit reclamation before publishing it. No expired IDs remain on
        // disk; stale offline routes cannot re-enrol them without new Online.
        rows.retain_mut(|row| {
            row.contact_expires_at_ms = None;
            if inputs.deleted.contains(&row.account_id) {
                return false;
            }
            if row.account_id == own_account
                || ever_history.contains(&row.account_id)
                || inputs.online.contains(&row.account_id)
            {
                return true;
            }
            let Some(observed) = journal.contacts.get(&row.account_id) else {
                return false;
            };
            row.contact_expires_at_ms = observed
                .offline_since_ms
                .map(|since| since.saturating_add(EMPTY_CONTACT_LIFETIME_MS));
            true
        });
        let existing: HashSet<_> = rows.iter().map(|row| row.account_id.clone()).collect();
        for (account, observed) in &journal.contacts {
            if !existing.contains(account) {
                rows.push(AccountInfo {
                    account_id: account.clone(),
                    device_count: 0,
                    names: Vec::new(),
                    contact_expires_at_ms: observed
                        .offline_since_ms
                        .map(|since| since.saturating_add(EMPTY_CONTACT_LIFETIME_MS)),
                });
            }
        }
        rows.sort_by(|left, right| left.account_id.cmp(&right.account_id));
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ME: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn observe(
        store: &HiddenContactsState,
        rows: &mut Vec<AccountInfo>,
        now: u64,
        online: &[&str],
        history: &[&str],
        deleted: &[&str],
    ) -> Result<(), String> {
        store.expire_empty_accounts(
            "alice",
            ME,
            rows,
            now,
            ContactExpiryInputs {
                online: &online.iter().map(|id| (*id).to_owned()).collect(),
                ever_history: &history.iter().map(|id| (*id).to_owned()).collect(),
                deleted: &deleted.iter().map(|id| (*id).to_owned()).collect(),
                history_for_absent: &|_| Ok(history.iter().map(|id| (*id).to_owned()).collect()),
            },
        )
    }
    fn saved(store: &HiddenContactsState) -> FirstObservations {
        serde_json::from_slice(&std::fs::read(store.expiry_path("alice", ME)).unwrap()).unwrap()
    }

    #[test]
    fn offline_contact_restarts_grace_period_and_reclaims_metadata_without_tombstones() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let mut rows = vec![row(A), row(A)];
        observe(&store, &mut rows, 100, &[A], &[], &[]).unwrap();
        assert!(rows.iter().all(|row| row.contact_expires_at_ms.is_none()));
        assert_eq!(
            saved(&store).contacts.len(),
            1,
            "devices share an account clock"
        );
        let path = store.expiry_path("alice", ME);
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        observe(&store, &mut rows, 100_000_000, &[A], &[], &[]).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified,
            "unchanged online observations must not write"
        );
        rows.clear(); // The entire peer disappeared, not merely one of its routes.
        observe(&store, &mut rows, 100_000_100, &[], &[], &[]).unwrap();
        assert_eq!(rows[0].contact_expires_at_ms, Some(186_400_100));
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        observe(&store, &mut rows, 100_000_200, &[], &[], &[]).unwrap();
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        // Reconnect cancels the first deadline; the next disconnect gets 24h.
        observe(&store, &mut rows, 186_400_099, &[A], &[], &[]).unwrap();
        assert_eq!(rows[0].contact_expires_at_ms, None);
        observe(&store, &mut rows, 200_000_000, &[], &[], &[]).unwrap();
        assert_eq!(rows[0].contact_expires_at_ms, Some(286_400_000));
        let reopened = HiddenContactsState::new(root.path().into());
        observe(&reopened, &mut rows, 286_399_999, &[], &[], &[]).unwrap();
        assert_eq!(rows.len(), 1);
        observe(&reopened, &mut rows, 286_400_000, &[], &[], &[]).unwrap();
        assert!(rows.is_empty());
        assert!(
            saved(&reopened).contacts.is_empty(),
            "delete metadata, not merely the visible row"
        );
        assert!(std::fs::metadata(&path).unwrap().len() < 64);
        let modified = std::fs::metadata(&path).unwrap().modified().unwrap();
        let mut stale = vec![row(A)];
        observe(&reopened, &mut stale, 286_400_001, &[], &[], &[]).unwrap();
        assert!(
            stale.is_empty(),
            "stale offline routes cannot recreate deleted contacts"
        );
        assert_eq!(
            std::fs::metadata(&path).unwrap().modified().unwrap(),
            modified
        );
        let mut returned = vec![row(A)];
        observe(&reopened, &mut returned, 300_000_000, &[A], &[], &[]).unwrap();
        assert_eq!(
            returned.len(),
            1,
            "new online discovery can recreate a contact"
        );
        assert_eq!(returned[0].contact_expires_at_ms, None);
    }

    #[test]
    fn ever_chatted_contacts_survive_offline_expiry_and_later_history_clear() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let mut rows = vec![row(A)];
        observe(&store, &mut rows, 100, &[A], &[], &[]).unwrap();
        observe(&store, &mut rows, 200, &[], &[A], &[]).unwrap();
        assert!(saved(&store).contacts[A].ever_history);
        rows.clear();
        let reopened = HiddenContactsState::new(root.path().into());
        observe(&reopened, &mut rows, 100_000_000, &[], &[], &[]).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].contact_expires_at_ms, None);
        // A durable indexed contact does not require an expiry record at all.
        let other = "cccccccccccccccccccccccccccccccc";
        rows.push(row(other));
        observe(&reopened, &mut rows, 200_000_000, &[], &[other], &[]).unwrap();
        assert!(rows.iter().any(|row| row.account_id == other));
        assert!(!saved(&store).contacts.contains_key(other));
    }

    #[test]
    fn absent_contact_history_is_rechecked_before_reclamation_and_failure_preserves_storage() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let mut rows = vec![row(A)];
        observe(&store, &mut rows, 1, &[A], &[], &[]).unwrap();
        rows.clear();
        observe(&store, &mut rows, 2, &[], &[], &[]).unwrap();
        rows.clear();
        let path = store.expiry_path("alice", ME);
        let before = std::fs::read(&path).unwrap();
        let none = HashSet::new();
        assert!(
            store
                .expire_empty_accounts(
                    "alice",
                    ME,
                    &mut rows,
                    86_400_002,
                    ContactExpiryInputs {
                        online: &none,
                        deleted: &none,
                        ever_history: &none,
                        history_for_absent: &|accounts| {
                            assert_eq!(accounts, &[A.to_owned()]);
                            Err("unavailable index".into())
                        },
                    }
                )
                .is_err()
        );
        assert_eq!(
            std::fs::read(&path).unwrap(),
            before,
            "uncertainty must never delete data"
        );
        store
            .expire_empty_accounts(
                "alice",
                ME,
                &mut rows,
                86_400_002,
                ContactExpiryInputs {
                    online: &none,
                    deleted: &none,
                    ever_history: &none,
                    history_for_absent: &|accounts| {
                        assert_eq!(accounts, &[A.to_owned()]);
                        Ok(HashSet::from([A.to_owned()]))
                    },
                },
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].contact_expires_at_ms, None);
        assert!(saved(&store).contacts[A].ever_history);
    }

    #[test]
    fn legacy_expired_records_migrate_conservatively_and_explicit_deletion_wins() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let path = store.expiry_path("alice", ME);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let legacy = serde_json::json!({"contacts": { A: {"first_seen_ms":0,"ever_history":false,"expired":true} }});
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        let mut rows = vec![row(A)];
        observe(&store, &mut rows, 100_000_000, &[A], &[], &[]).unwrap();
        assert_eq!(rows[0].contact_expires_at_ms, None);
        assert!(
            !String::from_utf8(std::fs::read(&path).unwrap())
                .unwrap()
                .contains("expired")
        );
        rows.clear();
        observe(&store, &mut rows, 200_000_000, &[], &[], &[]).unwrap();
        assert_eq!(rows[0].contact_expires_at_ms, Some(286_400_000));
        observe(&store, &mut rows, 200_000_100, &[A], &[], &[A]).unwrap();
        assert!(
            rows.is_empty(),
            "online presence must not undo explicit deletion"
        );
        assert!(saved(&store).contacts.is_empty());
    }

    #[test]
    fn corrupt_oversized_and_failed_journals_preserve_visible_rows_and_original_bytes() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let mut rows = vec![row(A)];
        observe(&store, &mut rows, 1, &[A], &[], &[]).unwrap();
        let path = store.expiry_path("alice", ME);
        for bytes in [
            b"corrupt".to_vec(),
            vec![b' '; MAX_EXPIRY_BYTES as usize + 1],
        ] {
            std::fs::write(&path, &bytes).unwrap();
            assert!(observe(&store, &mut rows, 100_000_000, &[], &[], &[]).is_err());
            assert_eq!(rows.len(), 1);
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
        }
        let blocked = root.path().join("blocked");
        std::fs::write(&blocked, b"original").unwrap();
        let blocked = HiddenContactsState::new(blocked);
        assert!(observe(&blocked, &mut rows, 1, &[A], &[], &[]).is_err());
        assert_eq!(rows.len(), 1);
    }

    #[test]
    fn offline_reclamation_releases_capacity_without_rewriting_unchanged_online_contacts() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let ids: Vec<_> = (0..MAX_EXPIRY_CONTACTS)
            .map(|i| format!("{i:032x}"))
            .collect();
        let mut rows: Vec<_> = ids.iter().map(|id| row(id)).collect();
        let online: Vec<_> = ids.iter().map(String::as_str).collect();
        observe(&store, &mut rows, 1, &online, &[], &[]).unwrap();
        assert_eq!(saved(&store).contacts.len(), MAX_EXPIRY_CONTACTS);
        assert!(
            std::fs::metadata(store.expiry_path("alice", ME))
                .unwrap()
                .len()
                <= MAX_EXPIRY_BYTES
        );
        rows.clear();
        observe(&store, &mut rows, 2, &[], &[], &[]).unwrap();
        assert_eq!(rows.len(), MAX_EXPIRY_CONTACTS);
        observe(&store, &mut rows, 86_400_002, &[], &[], &[]).unwrap();
        assert!(rows.is_empty());
        assert!(saved(&store).contacts.is_empty());
        let mut rows = vec![row(A)];
        observe(&store, &mut rows, 86_400_003, &[A], &[], &[]).unwrap();
        assert!(saved(&store).contacts.contains_key(A));
    }

    #[test]
    fn native_empty_contact_expiry_preserves_transport_and_incoming_history() {
        use mesh_talk_core::node::NodeRuntime;
        use tauri::Manager;
        let root = tempfile::tempdir().unwrap();
        let start = |owner: &'static str| {
            tauri::async_runtime::block_on(NodeRuntime::start(
                root.path(),
                owner,
                owner,
                "fixture-password",
                0,
                |_| {},
                |_| {},
                |_| {},
                |_| {},
                |_| {},
            ))
            .unwrap()
        };
        let alice = start("alice");
        let bob = start("bob");
        let alice_account = alice.account_id().to_owned();
        let bob_account = bob.account_id().to_owned();
        let alice_handle = alice.handle();
        let bob_handle = bob.handle();
        let alice_port = alice.discovery_port();
        let bob_port = bob.discovery_port();
        let auth = crate::services::auth_service::AuthService::new(Arc::new(
            mesh_talk_core::identity::manager::IdentityManager::new(
                mesh_talk_core::storage::file_manager::FileManager::new(root.path().into()),
            ),
        ));
        let state = crate::state::AppState::new(auth);
        state.session().set(
            "token".into(),
            crate::services::user::User {
                user_id: "alice".into(),
                account_login_name: None,
                name: "alice".into(),
                display_name: "Alice".into(),
                address: String::new(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "fixture-password".into(),
        );
        let nodes = crate::chat_commands::NodeState::empty();
        *nodes.0.blocking_write() = Some(alice);
        let policy = HiddenContactsState::new(root.path().into());
        let app = tauri::test::mock_builder()
            .manage(state)
            .manage(nodes.clone())
            .manage(policy.clone())
            .build(tauri::test::mock_context(tauri::test::noop_assets()))
            .unwrap();
        // Catch the entire business body, retire real producers, then propagate
        // its original failure before any app/store/temp-directory destruction.
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tauri::async_runtime::block_on(async {
                // Provision real signed discovery just as the other native
                // contact-policy fixture does. This expiry test does not use
                // the shared GUI reactor's UDP-send timing as its setup oracle.
                let announce = |sender: &NodeRuntime, port| {
                    let proof = sender
                        .handle()
                        .signed_announce(sender.display_name(), sender.listen_tcp_port());
                    assert!(proof.verify());
                    let bytes = mesh_talk_core::discovery::announce::encode(&proof);
                    std::net::UdpSocket::bind("127.0.0.1:0")
                        .unwrap()
                        .send_to(&bytes, (std::net::Ipv4Addr::LOCALHOST, port))
                        .unwrap();
                };
                announce(&bob, alice_port);
                {
                    let guard = nodes.0.read().await;
                    announce(guard.as_ref().unwrap(), bob_port);
                }
                let query =
                    || crate::chat_commands::list_accounts(app.state(), app.state(), app.state());
                let rows = tokio::time::timeout(std::time::Duration::from_secs(15), async {
                    loop {
                        let rows = query().await.unwrap();
                        if rows.iter().any(|row| row.account_id == bob_account)
                            && bob_handle
                                .cached_peer_snapshot()
                                .unwrap()
                                .iter()
                                .any(|peer| {
                                    peer.account_id.as_deref() == Some(alice_account.as_str())
                                })
                        {
                            break rows;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
                assert!(
                    rows.iter()
                        .find(|row| row.account_id == bob_account)
                        .unwrap()
                        .contact_expires_at_ms
                        .is_none()
                );
                assert!(
                    !alice_handle
                        .accounts_with_ever_history(std::slice::from_ref(&bob_account))
                        .unwrap()
                        .contains(&bob_account)
                );
                assert!(
                    alice_handle.known_account_ids().unwrap().is_empty(),
                    "authenticated discovery is not history"
                );
                let path = policy.expiry_path("alice", &alice_account);
                let mut journal: FirstObservations =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                journal
                    .contacts
                    .get_mut(&bob_account)
                    .unwrap()
                    .offline_since_ms = Some(0);
                let expired = serde_json::to_vec(&journal).unwrap();
                std::fs::write(&path, b"unreadable journal").unwrap();
                let uncertain = query().await.unwrap();
                assert_eq!(
                    uncertain
                        .iter()
                        .find(|row| row.account_id == bob_account)
                        .unwrap()
                        .contact_expires_at_ms,
                    None,
                    "journal failure must keep discovered contacts visible"
                );
                assert_eq!(std::fs::read(&path).unwrap(), b"unreadable journal");
                std::fs::write(&path, expired).unwrap();
                assert!(
                    query()
                        .await
                        .unwrap()
                        .iter()
                        .any(|row| row.account_id == bob_account),
                    "online discovery must override automatic expiry"
                );
                let persisted: FirstObservations =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                assert_eq!(persisted.contacts[&bob_account].offline_since_ms, None);
                let before = std::fs::read(&path).unwrap();
                assert!(
                    query()
                        .await
                        .unwrap()
                        .iter()
                        .any(|row| row.account_id == bob_account)
                );
                assert_eq!(
                    std::fs::read(&path).unwrap(),
                    before,
                    "unchanged online reads must not write the journal"
                );
                assert!(
                    alice_handle
                        .cached_peer_snapshot()
                        .unwrap()
                        .iter()
                        .any(|peer| peer.account_id.as_deref() == Some(bob_account.as_str())),
                    "presentation expiry preserves certified routing"
                );
                bob_handle
                    .send_to_account(&alice_account, b"history after automatic expiry", None)
                    .await
                    .unwrap();
                tokio::time::timeout(std::time::Duration::from_secs(15), async {
                    loop {
                        if alice_handle
                            .accounts_with_ever_history(std::slice::from_ref(&bob_account))
                            .unwrap()
                            .contains(&bob_account)
                        {
                            break;
                        }
                        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                    }
                })
                .await
                .unwrap();
                let rows = query().await.unwrap();
                assert_eq!(
                    rows.iter()
                        .find(|row| row.account_id == bob_account)
                        .unwrap()
                        .contact_expires_at_ms,
                    None
                );
                alice_handle
                    .clear_account_conversation(&bob_account)
                    .unwrap();
                assert!(
                    query()
                        .await
                        .unwrap()
                        .iter()
                        .any(|row| row.account_id == bob_account),
                    "clearing history cannot restart expiry"
                );
            });
        }));
        tauri::async_runtime::block_on(async {
            let alice = nodes.0.write().await.take().unwrap();
            alice.stop().await;
            bob.stop().await;
        });
        drop((alice_handle, bob_handle));
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
    }
    fn row(account: &str) -> crate::chat_commands::AccountInfo {
        crate::chat_commands::AccountInfo {
            account_id: account.into(),
            device_count: 1,
            names: vec!["Alice".into()],
            contact_expires_at_ms: None,
        }
    }
}
