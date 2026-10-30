//! Contact and history removal is local. Verified identity bindings remain intact.
use super::*;

impl HiddenContactsState {
    #[cfg(test)]
    pub(super) fn delete_unlocked(
        &self,
        owner: &str,
        account: &str,
        own_account: &str,
        _online: bool,
        clear_history: impl FnOnce() -> Result<(), String>,
    ) -> Result<HiddenContactsSnapshot, String> {
        if !valid_account(account) || account == own_account {
            return Err("Cannot delete this contact".into());
        }
        let mut snapshot = self.load(owner)?;
        snapshot.contacts.retain(|c| c.account_id != account);
        if !snapshot.deleted.iter().any(|id| id == account) {
            if snapshot.contacts.len() + snapshot.deleted.len() >= MAX_CONTACTS {
                return Err("Too many contact policies".into());
            }
            snapshot.deleted.push(account.into());
            snapshot.deleted.sort();
        }
        // Validate policy and presence before irreversible local history erasure.
        // These stores have separate atomic commits. On failure, report an error
        // and allow retry; never report contact deletion while history remains.
        clear_history()?;
        self.save(&snapshot)?;
        Ok(snapshot)
    }

    // Discovery is evidence of presence, never an explicit visibility restore.
    #[cfg(test)]
    pub(crate) fn visible_accounts(
        &self,
        owner: &str,
        _online: &[String],
        accounts: Vec<String>,
    ) -> Result<Vec<String>, String> {
        let snapshot = self.load(owner)?;
        Ok(accounts
            .into_iter()
            .filter(|id| !snapshot.deleted.contains(id))
            .collect())
    }

    pub(crate) fn personal_snapshot(
        &self,
        owner: &str,
        node: &mesh_talk_core::node::Node,
    ) -> Result<HiddenContactsSnapshot, String> {
        let mut snapshot = self.load(owner)?;
        let accounts: std::collections::BTreeSet<_> = node
            .known_account_ids()
            .map_err(|error| error.to_string())?
            .into_iter()
            .chain(node.personal_account_ids())
            .chain(snapshot.deleted.iter().cloned())
            .chain(snapshot.contacts.iter().map(|row| row.account_id.clone()))
            .collect();
        for account in accounts {
            let personal = node.conversation_personal_state(
                &mesh_talk_core::node::DocumentConversation::Account(account.clone()),
            );
            if !personal.known {
                continue;
            }
            snapshot.deleted.retain(|id| id != &account);
            if personal.hidden && personal.local_cleanup_complete {
                snapshot.contacts.retain(|row| row.account_id != account);
                snapshot.deleted.push(account);
            }
        }
        snapshot.deleted.sort();
        snapshot.deleted.dedup();
        Ok(snapshot)
    }
}

#[tauri::command]
pub async fn delete_contact(
    owner: String,
    expected_account: String,
    account: String,
    clear_history: Option<bool>,
    app: tauri::State<'_, crate::state::AppState>,
    node: tauri::State<'_, crate::chat_commands::NodeState>,
    policy: tauri::State<'_, HiddenContactsState>,
) -> Result<HiddenContactsSnapshot, String> {
    let admission = crate::runtime_admission::RuntimeAdmission::capture(&app, &node, &owner)
        .map_err(|error| error.to_string())?;
    let lease = admission.lease().clone();
    let node = node.inner().clone();
    #[cfg(test)]
    let captured = app.owner_command_captured.clone();
    let app = app.inner().clone();
    let policy = policy.inner().clone();
    tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        if let Some(entered) = captured.lock().unwrap().as_ref() {
            let _ = entered.send("contact-runtime-wait");
        }
        let guard = node.0.blocking_read();
        policy.transaction_for_lease(&app, &lease, || {
            node.check_installation(&lease)
                .map_err(|error| error.to_string())?;
            let runtime = guard
                .as_ref()
                .ok_or("Deleting a contact requires a running node")?;
            if runtime.host_account_id() != Some(owner.as_str()) {
                return Err("Deleting a contact requires this account's running node".into());
            }
            if runtime.account_id() != expected_account {
                return Err("Contact deletion belongs to another account".into());
            }
            if !valid_account(&account) || account == runtime.account_id() {
                return Err("Cannot delete this contact".into());
            }
            let handle = runtime.handle();
            // The exact installation/session guard already encloses the entire
            // synchronous producer. Do not reacquire that session in its callback.
            handle
                .delete_account_contact_if(
                    &expected_account,
                    &account,
                    clear_history.unwrap_or(true),
                    |commit| commit(),
                )
                .map_err(|error| error.to_string())?;
            // New deletions are account-scoped core controls. Retire the old
            // local projection so it cannot override a later genuine reopen.
            policy.update_unlocked(&owner, &account, false, "", Some(runtime.account_id()))?;
            policy.personal_snapshot(&owner, &handle)
        })
    })
    .await
    .map_err(|_| "Delete contact task failed")?
}
