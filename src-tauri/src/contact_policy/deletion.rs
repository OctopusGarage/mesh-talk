//! Contact and history removal is local. Verified identity bindings remain intact.
use super::*;

impl HiddenContactsState {
    pub(super) fn delete_unlocked(
        &self,
        owner: &str,
        account: &str,
        own_account: &str,
        online: bool,
        clear_history: impl FnOnce() -> Result<(), String>,
    ) -> Result<HiddenContactsSnapshot, String> {
        if !valid_account(account) || account == own_account {
            return Err("Cannot delete this contact".into());
        }
        if online {
            return Err("Contact is online. Only offline contacts can be deleted.".into());
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

    // Called under transaction: fresh online discovery clears a deletion, so a
    // rediscovered contact remains available even after it goes offline again.
    pub(crate) fn visible_accounts(
        &self,
        owner: &str,
        online: &[String],
        accounts: Vec<String>,
    ) -> Result<Vec<String>, String> {
        let mut snapshot = self.load(owner)?;
        let before = snapshot.deleted.len();
        snapshot.deleted.retain(|id| !online.contains(id));
        if snapshot.deleted.len() != before {
            self.save(&snapshot)?;
        }
        Ok(accounts
            .into_iter()
            .filter(|id| !snapshot.deleted.contains(id))
            .collect())
    }
}

#[tauri::command]
pub async fn delete_contact(
    owner: String,
    account: String,
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
        let guard = node.0.blocking_lock();
        policy.transaction_for_lease(&app, &lease, || {
            node.check_installation(&lease)
                .map_err(|error| error.to_string())?;
            let runtime = guard
                .as_ref()
                .ok_or("Deleting a contact requires a running node")?;
            if runtime.host_account_id() != Some(owner.as_str()) {
                return Err("Deleting a contact requires this account's running node".into());
            }
            let online = crate::chat_commands::online_account_ids(runtime.peers());
            policy.delete_unlocked(
                &owner,
                &account,
                runtime.account_id(),
                online.contains(&account),
                || {
                    runtime
                        .handle()
                        .clear_account_conversation(&account)
                        .map(|_| ())
                        .map_err(|error| error.to_string())
                },
            )
        })
    })
    .await
    .map_err(|_| "Delete contact task failed")?
}
