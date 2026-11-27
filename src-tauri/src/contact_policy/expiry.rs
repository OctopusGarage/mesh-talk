//! Device-local expiry for discovered accounts that never had durable history.
use super::*;

use crate::chat_commands::AccountInfo;
use std::{collections::BTreeMap, io::Read};

const EMPTY_CONTACT_LIFETIME_MS: u64 = 86_400_000;
const MAX_EXPIRY_CONTACTS: usize = 4096;
const MAX_EXPIRY_BYTES: u64 = 512 * 1024;

#[derive(Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FirstObservations {
    contacts: BTreeMap<String, ObservedContact>,
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ObservedContact {
    first_seen_ms: u64,
    ever_history: bool,
    #[serde(default)]
    expired: bool,
}

impl HiddenContactsState {
    fn expiry_path(&self, owner: &str, own_account: &str) -> PathBuf {
        self.root
            .join("accounts")
            .join(owner)
            .join(format!("empty-contacts-{own_account}.json"))
    }

    /// Caller owns the existing policy/session transaction. No lock is acquired
    /// here: expiry must compose with explicit visibility without reversing order.
    pub(crate) fn expire_empty_accounts(
        &self,
        owner: &str,
        own_account: &str,
        rows: &mut Vec<AccountInfo>,
        ever_history: &HashSet<String>,
        now_ms: u64,
    ) -> Result<(), String> {
        authorize(Some(owner), owner)?;
        if !valid_account(own_account) {
            return Err("Invalid expiry account".into());
        }
        if rows.is_empty() {
            return Ok(());
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
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                FirstObservations::default()
            }
            Err(error) => return Err(error.to_string()),
        };
        if journal.contacts.len() > MAX_EXPIRY_CONTACTS
            || journal.contacts.keys().any(|id| !valid_account(id))
        {
            return Err("Invalid contact expiry journal".into());
        }
        let mut changed = false;
        for row in rows.iter() {
            if row.account_id == own_account || !valid_account(&row.account_id) {
                continue;
            }
            let history = ever_history.contains(&row.account_id);
            if let Some(observed) = journal.contacts.get_mut(&row.account_id) {
                if history && !observed.ever_history {
                    observed.ever_history = true;
                    changed = true;
                }
            } else if journal.contacts.len() < MAX_EXPIRY_CONTACTS {
                journal.contacts.insert(
                    row.account_id.clone(),
                    ObservedContact {
                        first_seen_ms: now_ms,
                        ever_history: history,
                        expired: false,
                    },
                );
                changed = true;
            }
            if let Some(observed) = journal.contacts.get_mut(&row.account_id)
                && !observed.ever_history
                && !observed.expired
                && now_ms
                    >= observed
                        .first_seen_ms
                        .saturating_add(EMPTY_CONTACT_LIFETIME_MS)
            {
                observed.expired = true;
                changed = true;
            }
        }
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
            serde_json::to_writer(&mut temporary, &journal).map_err(|e| e.to_string())?;
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
        // Persist before publishing any deadline or omission. Retain expired keys
        // so continuous discovery cannot recreate them. Saturation fails open for
        // new accounts; keys are not evicted because that would renew old clocks.
        rows.retain_mut(|row| {
            row.contact_expires_at_ms =
                if row.account_id == own_account || ever_history.contains(&row.account_id) {
                    None
                } else {
                    journal
                        .contacts
                        .get(&row.account_id)
                        .filter(|observed| !observed.ever_history)
                        .map(|observed| {
                            observed
                                .first_seen_ms
                                .saturating_add(EMPTY_CONTACT_LIFETIME_MS)
                        })
                };
            row.contact_expires_at_ms.is_none()
                || !journal
                    .contacts
                    .get(&row.account_id)
                    .is_some_and(|observed| observed.expired)
        });
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ME: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
    #[test]
    fn empty_contact_expiry_is_exact_and_survives_restart_and_rediscovery() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let mut rows = vec![row(A), row(A)];
        store
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 100)
            .unwrap();
        assert_eq!(rows[0].contact_expires_at_ms, Some(86_400_100));
        let journal = store.expiry_path("alice", ME);
        let before = std::fs::metadata(&journal).unwrap().modified().unwrap();
        let persisted: FirstObservations =
            serde_json::from_slice(&std::fs::read(&journal).unwrap()).unwrap();
        assert_eq!(
            persisted.contacts.len(),
            1,
            "two devices share one account clock"
        );
        rows[0].names = vec!["Renamed device".into()];
        store
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 200)
            .unwrap();
        assert_eq!(
            std::fs::metadata(&journal).unwrap().modified().unwrap(),
            before,
            "unchanged discovery must not rewrite the clock journal"
        );
        let reopened = HiddenContactsState::new(root.path().into());
        let mut rows = vec![row(A)];
        reopened
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 86_400_099)
            .unwrap();
        assert_eq!(rows.len(), 1);
        reopened
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 86_400_100)
            .unwrap();
        assert!(rows.is_empty());
        let mut rows = vec![row(A)];
        reopened
            .expire_empty_accounts(
                "alice",
                ME,
                &mut rows,
                &HashSet::from([A.into()]),
                99_000_000,
            )
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].contact_expires_at_ms, None);
        // A later cleared legacy transcript must not lose observed history.
        reopened
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 100_000_000)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].contact_expires_at_ms, None);
    }
    #[test]
    fn expiry_remains_consumed_after_clock_rollback_and_restart() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let mut rows = vec![row(A)];
        store
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 100)
            .unwrap();
        store
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 86_400_100)
            .unwrap();
        assert!(rows.is_empty());
        let reopened = HiddenContactsState::new(root.path().into());
        let mut rows = vec![row(A)];
        reopened
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 86_400_099)
            .unwrap();
        assert!(
            rows.is_empty(),
            "a consumed expiry must survive a backwards wall clock"
        );
        let mut rows = vec![row(A)];
        reopened
            .expire_empty_accounts(
                "alice",
                ME,
                &mut rows,
                &HashSet::from([A.into()]),
                86_400_099,
            )
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "a first committed conversation restores the account"
        );
        reopened
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 50)
            .unwrap();
        assert_eq!(
            rows.len(),
            1,
            "later clearing and clock rollback retain history protection"
        );
    }
    #[test]
    fn expiry_scopes_owner_identity_and_preserves_journal_on_failure() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let mut rows = vec![row(A), row(ME)];
        store
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 100)
            .unwrap();
        assert_eq!(rows[1].contact_expires_at_ms, None);
        let mut other = vec![row(A)];
        store
            .expire_empty_accounts("bob", ME, &mut other, &HashSet::new(), 200)
            .unwrap();
        assert_eq!(other[0].contact_expires_at_ms, Some(86_400_200));
        let mut adopted = vec![row(ME)];
        store
            .expire_empty_accounts("alice", A, &mut adopted, &HashSet::new(), 300)
            .unwrap();
        assert_eq!(adopted[0].contact_expires_at_ms, Some(86_400_300));
        let path = store.expiry_path("alice", ME);
        let bytes = std::fs::read(&path).unwrap();
        std::fs::write(&path, b"corrupt").unwrap();
        let mut rows = vec![row(A)];
        assert!(
            store
                .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 99_000_000)
                .is_err()
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(std::fs::read(&path).unwrap(), b"corrupt");
        let oversized = vec![b' '; 524_289];
        std::fs::write(&path, &oversized).unwrap();
        assert!(
            store
                .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 99_000_000)
                .is_err()
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(std::fs::metadata(&path).unwrap().len(), 524_289);
        std::fs::write(&path, bytes).unwrap();
        store
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 99_000_000)
            .unwrap();
        assert!(rows.is_empty());
    }
    #[test]
    fn expiry_saturation_is_fail_open_without_restarting_recorded_clocks() {
        let root = tempfile::tempdir().unwrap();
        let store = HiddenContactsState::new(root.path().into());
        let mut rows: Vec<_> = (0..4096).map(|i| row(&format!("{i:032x}"))).collect();
        store
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 1)
            .unwrap();
        assert!(
            std::fs::metadata(store.expiry_path("alice", ME))
                .unwrap()
                .len()
                <= 524_288
        );
        let mut rows = vec![row(A), row("00000000000000000000000000000000")];
        store
            .expire_empty_accounts("alice", ME, &mut rows, &HashSet::new(), 86_400_001)
            .unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].account_id, A);
        assert_eq!(rows[0].contact_expires_at_ms, None);
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
                        .is_some()
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
                    .first_seen_ms = 0;
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
                    !query()
                        .await
                        .unwrap()
                        .iter()
                        .any(|row| row.account_id == bob_account)
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
