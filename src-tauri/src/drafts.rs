//! Account-scoped drafts and explicit kept-source import of legacy local drafts.
use crate::{
    commands::CommandError,
    state::{AppState, SessionLease},
};
use mesh_talk_core::{
    node::{
        AccountDocumentDomain, AccountDocumentKey, DocumentConversation, DocumentVersion,
        MessageProtection, Node,
    },
    storage::record_log::EncryptedRecordLog,
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::Path};

type Texts = BTreeMap<String, String>;
type Protections = BTreeMap<String, MessageProtection>;
const PROTECTION_PREFIX: &str = "protection:";
const MAX_BYTES: usize = 4 * 1024 * 1024;

#[derive(Clone, Serialize, Deserialize)]
pub struct DraftChange {
    text: String,
    protection: Option<MessageProtection>,
    expected: Option<DocumentVersion>,
}
#[derive(Serialize, Deserialize)]
struct DraftPayload {
    text: String,
    protection: Option<MessageProtection>,
}

fn conversation(key: &str) -> Result<DocumentConversation, CommandError> {
    let (kind, id) = key
        .split_once(':')
        .ok_or_else(|| CommandError::Validation("invalid draft conversation".into()))?;
    let expected = match kind {
        "account" => 32,
        "channel" => 64,
        _ => 0,
    };
    if expected == 0
        || id.len() != expected
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    {
        return Err(CommandError::Validation(
            "draft conversation cannot be mapped to an account or channel".into(),
        ));
    }
    Ok(if kind == "account" {
        DocumentConversation::Account(id.into())
    } else {
        DocumentConversation::Channel(id.into())
    })
}
fn conversation_key(value: &DocumentConversation) -> String {
    match value {
        DocumentConversation::Account(id) => format!("account:{id}"),
        DocumentConversation::Channel(id) => format!("channel:{id}"),
    }
}

fn document_snapshot(node: &Node, writer: String) -> Result<DraftSnapshot, CommandError> {
    let mut snapshot = DraftSnapshot {
        session: writer,
        account_id: Some(node.account_id()),
        drafts: Texts::new(),
        protections: Protections::new(),
        versions: BTreeMap::new(),
        conflicts: Vec::new(),
    };
    let mut after = None;
    loop {
        let page = node
            .account_documents(AccountDocumentDomain::Draft, after.as_ref(), 64)
            .map_err(CommandError::from)?;
        for meta in page.items {
            let AccountDocumentKey::Draft { conversation } = &meta.key else {
                continue;
            };
            let key = conversation_key(conversation);
            snapshot.versions.insert(key.clone(), meta.version());
            if meta.tombstone {
                continue;
            }
            let Some(document) = node
                .account_document(&meta.key)
                .map_err(CommandError::from)?
            else {
                continue;
            };
            snapshot
                .versions
                .insert(key.clone(), document.meta.version());
            let payload: DraftPayload = serde_json::from_slice(document.payload())
                .map_err(|_| CommandError::Validation("invalid synchronized draft".into()))?;
            if !payload.text.is_empty() {
                snapshot.drafts.insert(key.clone(), payload.text);
            }
            if let Some(protection) = payload.protection {
                snapshot.protections.insert(key, protection);
            }
        }
        after = page.next;
        if after.is_none() {
            break;
        }
    }
    Ok(snapshot)
}

fn save_documents(
    node: &Node,
    writer: String,
    changes: BTreeMap<String, DraftChange>,
) -> Result<DraftSnapshot, CommandError> {
    // Validate the whole request before its first per-conversation commit.
    let mut prepared = Vec::new();
    if changes.len() > 1000 {
        return Err(CommandError::Validation("draft batch exceeds limit".into()));
    }
    for (key, change) in changes {
        let document_key = AccountDocumentKey::Draft {
            conversation: conversation(&key)?,
        };
        let payload = serde_json::to_vec(&DraftPayload {
            text: change.text.clone(),
            protection: change.protection,
        })
        .map_err(|_| CommandError::Validation("invalid draft".into()))?;
        if payload.len() > 64 * 1024 {
            return Err(CommandError::Validation("draft exceeds 64 KiB".into()));
        }
        prepared.push((key, document_key, change, payload));
    }
    let mut conflicts = Vec::new();
    for (key, document_key, change, payload) in prepared {
        let current = node.account_document_meta(&document_key);
        if current.as_ref().map(|meta| meta.version()) != change.expected {
            conflicts.push(key);
            continue;
        }
        let result = if change.text.is_empty() && change.protection.is_none() {
            match current {
                Some(meta) if !meta.tombstone => node
                    .remove_account_document_if(
                        document_key.clone(),
                        Some(meta.version()),
                        |commit| commit(),
                    )
                    .map(|_| ()),
                Some(_) => Ok(()),
                None => node
                    .remove_account_document_if(document_key.clone(), None, |commit| commit())
                    .map(|_| ()),
            }
        } else {
            node.put_account_document_if(
                document_key.clone(),
                payload,
                change.expected.clone(),
                |commit| commit(),
            )
            .map(|_| ())
        };
        if let Err(error) = result {
            if node
                .account_document_meta(&document_key)
                .as_ref()
                .map(|meta| meta.version())
                != change.expected
            {
                conflicts.push(key);
            } else {
                return Err(CommandError::from(error));
            }
        }
    }
    let mut snapshot = document_snapshot(node, writer)?;
    snapshot.conflicts = conflicts;
    Ok(snapshot)
}

fn document_writer(
    app: &AppState,
    lease: &SessionLease,
    account: &str,
    expected_writer: Option<&str>,
) -> Result<String, CommandError> {
    let mut cache = app
        .drafts
        .lock()
        .map_err(|_| CommandError::Internal("draft lock unavailable".into()))?;
    if let Some(expected) = expected_writer {
        return cache
            .as_ref()
            .filter(|stored| {
                stored.lease == *lease && stored.account == account && stored.writer == expected
            })
            .map(|stored| stored.writer.clone())
            .ok_or_else(|| CommandError::Authorization("draft session not loaded".into()));
    }
    if cache
        .as_ref()
        .is_none_or(|stored| stored.lease != *lease || stored.account != account)
    {
        *cache = Some(DraftStorage {
            lease: lease.clone(),
            account: account.into(),
            writer: rand::random::<u128>().to_string(),
        });
    }
    Ok(cache.as_ref().unwrap().writer.clone())
}

#[tauri::command]
pub async fn load_conversation_drafts(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: String,
    expected_account: String,
) -> Result<DraftSnapshot, CommandError> {
    let admission = crate::runtime_admission::RuntimeAdmission::capture(&app, &state, &owner)?;
    admission
        .with_runtime(|runtime| {
            if runtime.account_id() != expected_account {
                return Err(CommandError::Authorization("draft account changed".into()));
            }
            let writer = document_writer(&app, admission.lease(), &expected_account, None)?;
            document_snapshot(&runtime.handle(), writer)
        })
        .await
}

#[tauri::command]
pub async fn save_conversation_drafts(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: String,
    session: String,
    expected_account: String,
    changes: BTreeMap<String, DraftChange>,
) -> Result<DraftSnapshot, CommandError> {
    let admission = crate::runtime_admission::RuntimeAdmission::capture(&app, &state, &owner)?;
    admission
        .with_runtime(|runtime| {
            if runtime.account_id() != expected_account {
                return Err(CommandError::Authorization("draft account changed".into()));
            }
            let writer =
                document_writer(&app, admission.lease(), &expected_account, Some(&session))?;
            save_documents(&runtime.handle(), writer, changes)
        })
        .await
}

#[derive(Serialize)]
pub struct LegacyDraftImport {
    imported: usize,
    unmapped: usize,
}

#[tauri::command]
pub async fn publish_legacy_conversation_drafts(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: String,
    expected_account: String,
) -> Result<LegacyDraftImport, CommandError> {
    let admission = crate::runtime_admission::RuntimeAdmission::capture(&app, &state, &owner)?;
    let root = crate::recovery::data_root(&state);
    admission
        .with_runtime(|runtime| {
            if runtime.account_id() != expected_account
                || !runtime.handle().device_sync_policy().history
            {
                return Err(CommandError::Authorization(
                    "draft source account or sharing policy changed".into(),
                ));
            }
            let path = crate::config_store::account_path(&root, &owner, "drafts.enc")
                .ok_or_else(|| CommandError::Validation("invalid draft owner".into()))?;
            let (texts, protections) = read_legacy_drafts(&path, runtime.restart_password())?;
            let mut known: std::collections::BTreeSet<_> =
                runtime.known_account_names()?.into_keys().collect();
            known.extend(
                runtime
                    .known_account_ids()
                    .map_err(|e| CommandError::Internal(e.to_string()))?,
            );
            known.insert(runtime.account_id().to_owned());
            seed_legacy_documents(&runtime.handle(), texts, protections, &known)
        })
        .await
}

fn read_legacy_drafts(path: &Path, password: &str) -> Result<(Texts, Protections), CommandError> {
    if !path
        .try_exists()
        .map_err(|error| CommandError::Io(error.to_string()))?
    {
        return Ok((Texts::new(), Protections::new()));
    }
    if path
        .metadata()
        .map_err(|error| CommandError::Io(error.to_string()))?
        .len()
        > (MAX_BYTES + 65536) as u64
    {
        return Err(CommandError::Validation(
            "legacy draft file exceeds limit".into(),
        ));
    }
    let (_, records) = EncryptedRecordLog::<Texts>::open(path, password, b"MTDR01")
        .map_err(|error| CommandError::Io(error.to_string()))?;
    decode(&records.into_iter().last().unwrap_or_default())
}

fn seed_legacy_documents(
    node: &Node,
    texts: Texts,
    protections: Protections,
    known_accounts: &std::collections::BTreeSet<String>,
) -> Result<LegacyDraftImport, CommandError> {
    let mut result = LegacyDraftImport {
        imported: 0,
        unmapped: 0,
    };
    let mut changes = BTreeMap::new();
    for key in texts
        .keys()
        .chain(protections.keys())
        .collect::<std::collections::BTreeSet<_>>()
    {
        let Ok(conversation) = conversation(key) else {
            result.unmapped += 1;
            continue;
        };
        if node
            .account_document_meta(&AccountDocumentKey::Draft {
                conversation: conversation.clone(),
            })
            .is_some()
        {
            continue;
        }
        let unambiguous = match &conversation {
            DocumentConversation::Account(id) => known_accounts.contains(id),
            DocumentConversation::Channel(id) => node
                .list_channels()?
                .iter()
                .any(|channel| hex::encode(channel.id.as_bytes()) == *id),
        };
        if !unambiguous {
            result.unmapped += 1;
            continue;
        }
        changes.insert(
            key.clone(),
            DraftChange {
                text: texts.get(key).cloned().unwrap_or_default(),
                protection: protections.get(key).copied(),
                expected: None,
            },
        );
    }
    let snapshot = save_documents(node, String::new(), changes.clone())?;
    result.imported = changes.len() - snapshot.conflicts.len();
    Ok(result)
}

pub(crate) struct DraftStorage {
    lease: SessionLease,
    writer: String,
    account: String,
}

#[derive(Serialize)]
pub struct DraftSnapshot {
    session: String,
    drafts: Texts,
    protections: Protections,
    account_id: Option<String>,
    versions: BTreeMap<String, DocumentVersion>,
    conflicts: Vec<String>,
}

fn valid_key(key: &str) -> bool {
    (key.starts_with("account:") || key.starts_with("channel:"))
        && key.len() <= 256
        && !key.ends_with(':')
}

fn protection(value: &str) -> Result<MessageProtection, CommandError> {
    match value {
        "click" => Ok(MessageProtection::Click),
        "authenticate" => Ok(MessageProtection::Authenticate),
        _ => Err(CommandError::Validation("invalid draft protection".into())),
    }
}

// Keep MTDR01's bincode map-of-strings layout. Reserved metadata entries let legacy
// text-only snapshots decode unchanged, and text/policy share one atomic rewrite.
fn validate(texts: &Texts) -> Result<(), CommandError> {
    let bytes: usize = texts.iter().map(|(key, text)| key.len() + text.len()).sum();
    let mut conversations = std::collections::BTreeSet::new();
    if bytes > MAX_BYTES {
        return Err(CommandError::Validation(
            "draft storage limit exceeded".into(),
        ));
    }
    for (key, text) in texts {
        let conversation = if let Some(key) = key.strip_prefix(PROTECTION_PREFIX) {
            protection(text)?;
            key
        } else {
            if text.is_empty() {
                return Err(CommandError::Validation("empty draft text".into()));
            }
            key
        };
        if !valid_key(conversation) {
            return Err(CommandError::Validation(
                "invalid draft conversation".into(),
            ));
        }
        conversations.insert(conversation);
    }
    if conversations.len() > 1000 {
        return Err(CommandError::Validation(
            "draft storage limit exceeded".into(),
        ));
    }
    Ok(())
}

fn decode(texts: &Texts) -> Result<(Texts, Protections), CommandError> {
    validate(texts)?;
    let mut drafts = Texts::new();
    let mut protections = Protections::new();
    for (key, value) in texts {
        if let Some(key) = key.strip_prefix(PROTECTION_PREFIX) {
            protections.insert(key.into(), protection(value)?);
        } else {
            drafts.insert(key.clone(), value.clone());
        }
    }
    Ok((drafts, protections))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{auth_service::AuthService, user::User};
    use mesh_talk_core::{identity::manager::IdentityManager, storage::file_manager::FileManager};
    use std::sync::Arc;

    fn document_node(
        root: &Path,
        device: &mesh_talk_core::identity::device::DeviceIdentity,
        account: &mesh_talk_core::identity::account::Account,
    ) -> Arc<Node> {
        let (tx, _) = tokio::sync::mpsc::unbounded_channel();
        let (ct, _) = tokio::sync::mpsc::unbounded_channel();
        let (ft, _) = tokio::sync::mpsc::unbounded_channel();
        let (ed, x) = device.secret_bytes();
        Node::open_with_account(
            mesh_talk_core::identity::device::DeviceIdentity::from_secret_bytes(ed, x),
            mesh_talk_core::identity::account::Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(std::sync::Mutex::new(
                mesh_talk_core::discovery::Roster::default(),
            )),
            tx,
            ct,
            ft,
            &root.join("messages.log"),
            &root.join("sent.log"),
            "pw",
        )
        .unwrap()
    }

    #[test]
    fn account_document_draft_changes_preserve_concurrent_conversations_and_tombstones_after_restart()
     {
        use mesh_talk_core::identity::{account::Account, device::DeviceIdentity};
        let root = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let account = Account::generate();
        let node = document_node(root.path(), &device, &account);
        let a = format!("account:{}", "a".repeat(32));
        let b = format!("channel:{}", "b".repeat(64));
        let c = format!("account:{}", "c".repeat(32));
        let change = |text: &str, expected| DraftChange {
            text: text.into(),
            protection: None,
            expected,
        };
        let saved = save_documents(
            &node,
            "writer".into(),
            BTreeMap::from([
                (a.clone(), change("local A", None)),
                (b.clone(), change("local B", None)),
            ]),
        )
        .unwrap();
        save_documents(
            &node,
            "remote".into(),
            BTreeMap::from([
                (
                    a.clone(),
                    change("remote A", Some(saved.versions[&a].clone())),
                ),
                (c.clone(), change("remote C", None)),
            ]),
        )
        .unwrap();
        let result = save_documents(
            &node,
            "writer".into(),
            BTreeMap::from([
                (
                    a.clone(),
                    change("dirty A", Some(saved.versions[&a].clone())),
                ),
                (b.clone(), change("", Some(saved.versions[&b].clone()))),
            ]),
        )
        .unwrap();
        assert_eq!(result.conflicts, vec![a.clone()]);
        assert_eq!(result.drafts.get(&a).unwrap(), "remote A");
        assert_eq!(result.drafts.get(&c).unwrap(), "remote C");
        assert!(!result.drafts.contains_key(&b));
        drop(node);
        let node = document_node(root.path(), &device, &account);
        let reopened = document_snapshot(&node, "reopened".into()).unwrap();
        assert_eq!(reopened.drafts, result.drafts);
        assert!(
            node.account_document_meta(&AccountDocumentKey::Draft {
                conversation: conversation(&b).unwrap()
            })
            .unwrap()
            .tombstone
        );
    }

    #[test]
    fn legacy_drafts_require_explicit_source_seed_and_cannot_overwrite_canonical_clear_or_guess_identifiers()
     {
        use mesh_talk_core::identity::{account::Account, device::DeviceIdentity};
        let root = tempfile::tempdir().unwrap();
        let node = document_node(
            root.path(),
            &DeviceIdentity::generate(),
            &Account::generate(),
        );
        let key = format!("account:{}", "a".repeat(32));
        let clear = format!("channel:{}", "b".repeat(64));
        let legacy = Texts::from([
            (key.clone(), "legacy source draft".into()),
            (format!("protection:{key}"), "authenticate".into()),
            (clear.clone(), "never resurrect".into()),
            ("account:ambiguous-device".into(), "unmapped".into()),
            (
                format!("account:{}", "d".repeat(32)),
                "unknown account or device".into(),
            ),
        ]);
        let path = root.path().join("drafts.enc");
        let (mut file, _) = EncryptedRecordLog::<Texts>::open(&path, "pw", b"MTDR01").unwrap();
        file.rewrite(std::slice::from_ref(&legacy)).unwrap();
        let original = std::fs::read(&path).unwrap();
        let (texts, protections) = read_legacy_drafts(&path, "pw").unwrap();
        assert!(
            document_snapshot(&node, "ordinary join startup".into())
                .unwrap()
                .drafts
                .is_empty()
        );
        let initial = save_documents(
            &node,
            "writer".into(),
            BTreeMap::from([(
                clear.clone(),
                DraftChange {
                    text: "before clear".into(),
                    protection: None,
                    expected: None,
                },
            )]),
        )
        .unwrap();
        save_documents(
            &node,
            "writer".into(),
            BTreeMap::from([(
                clear.clone(),
                DraftChange {
                    text: String::new(),
                    protection: None,
                    expected: Some(initial.versions[&clear].clone()),
                },
            )]),
        )
        .unwrap();
        let report = seed_legacy_documents(
            &node,
            texts,
            protections,
            &std::collections::BTreeSet::from(["a".repeat(32)]),
        )
        .unwrap();
        assert_eq!((report.imported, report.unmapped), (1, 2));
        let snapshot = document_snapshot(&node, "source".into()).unwrap();
        assert_eq!(snapshot.drafts.get(&key).unwrap(), "legacy source draft");
        assert_eq!(
            snapshot.protections.get(&key),
            Some(&MessageProtection::Authenticate)
        );
        assert!(!snapshot.drafts.contains_key(&clear));
        assert_eq!(
            std::fs::read(path).unwrap(),
            original,
            "explicit import preserves the legacy encrypted file"
        );
    }

    #[test]
    fn draft_writer_is_bound_to_session_and_cryptographic_account() {
        let root = tempfile::tempdir().unwrap();
        let app = fixture(root.path());
        let lease = app.session().capture().unwrap();
        let writer = document_writer(&app, &lease, "account-one", None).unwrap();
        assert!(document_writer(&app, &lease, "account-two", Some(&writer)).is_err());
        login(&app);
        let next = app.session().capture().unwrap();
        let next_writer = document_writer(&app, &next, "account-one", None).unwrap();
        assert_ne!(writer, next_writer);
        assert!(document_writer(&app, &next, "account-one", Some(&writer)).is_err());
    }

    fn fixture(root: &Path) -> AppState {
        let app = AppState::new(AuthService::new(Arc::new(IdentityManager::new(
            FileManager::new(root.to_owned()),
        ))));
        login(&app);
        app
    }
    fn login(app: &AppState) {
        app.session().set(
            "token".into(),
            User {
                user_id: "alice".into(),
                name: "alice".into(),
                account_login_name: None,
                display_name: "Alice".into(),
                address: "fixture".into(),
                created_at: 0,
                last_seen: 0,
                is_online: true,
            },
            "password".into(),
        );
    }
    #[test]
    fn invalid_and_oversized_snapshots_are_rejected() {
        assert!(validate(&Texts::from([("wrong".into(), "text".into())])).is_err());
        assert!(validate(&Texts::from([("account:a".into(), "x".repeat(MAX_BYTES))])).is_err());
        assert!(validate(&Texts::from([("account:a".into(), "".into())])).is_err());
    }
}
