//! Remember only a local user's refresh grant, encrypted beside their credentials.
use super::*;
use serde::{Deserialize, Serialize};

const GRANT_FILE: &str = "meta/google_drive.enc";

#[derive(Serialize, Deserialize)]
struct SavedGrant {
    client_id: String,
    refresh_token: String,
}

#[derive(Clone)]
pub(super) struct Owner {
    app: AppState,
    node: NodeState,
    generation: u64,
}
impl Owner {
    pub(super) fn generation(&self) -> u64 {
        self.generation
    }
    pub(super) fn capture(app: &AppState, node: &NodeState) -> Option<Self> {
        app.session().capture().ok().map(|lease| Self {
            app: app.clone(),
            node: node.clone(),
            generation: lease.generation(),
        })
    }
    pub(super) async fn load(&self) -> Result<Option<String>, CommandError> {
        recovery::setup::with_storage(&self.app, &self.node, Some(self.generation), |storage| {
            if !storage.exists(GRANT_FILE) { return Ok(None); }
            let saved: SavedGrant = storage.read(GRANT_FILE,
                "Could not unlock the saved Google Drive connection. Disconnect it and connect again.")?;
            if saved.client_id != oauth::client_id() { return Ok(None); }
            if saved.refresh_token.is_empty() || saved.refresh_token.len() > 16 * 1024 {
                return Err(CommandError::Authentication("Saved Google Drive connection is invalid. Disconnect it and connect again.".into()));
            }
            Ok(Some(saved.refresh_token))
        }).await
    }
    pub(super) async fn save(
        &self,
        refresh_token: String,
        connection: Arc<Connection>,
    ) -> Result<(), CommandError> {
        recovery::setup::with_storage(
            &self.app,
            &self.node,
            Some(self.generation),
            move |storage| {
                let check = || {
                    if *connection.cancelled.borrow() {
                        Err(CommandError::Authorization(
                            "Google Drive connection was cancelled".into(),
                        ))
                    } else {
                        Ok(())
                    }
                };
                check()?;
                let saved = SavedGrant {
                    client_id: oauth::client_id().into(),
                    refresh_token,
                };
                // A late OAuth response cannot undo a forget, including cancellation
                // while the encrypted replacement is being prepared.
                storage.write_if(
                    GRANT_FILE,
                    &saved,
                    "Could not remember Google Drive securely. Try connecting again.",
                    check,
                )
            },
        )
        .await
    }
    pub(super) async fn forget(&self) -> Result<(), CommandError> {
        recovery::setup::with_storage(&self.app, &self.node, Some(self.generation), |storage| {
            storage.delete(GRANT_FILE, "Could not forget Google Drive. Try again.")
        })
        .await
    }
    pub(super) async fn forget_rejected(
        &self,
        rejected_token: String,
        connection: Arc<Connection>,
    ) -> Result<(), CommandError> {
        recovery::setup::with_storage(
            &self.app,
            &self.node,
            Some(self.generation),
            move |storage| {
                if *connection.cancelled.borrow() || !storage.exists(GRANT_FILE) {
                    return Ok(());
                }
                let saved: SavedGrant = storage.read(
                    GRANT_FILE,
                    "Could not unlock the saved Google Drive connection",
                )?;
                // An older failed refresh must not forget a more recent Google login.
                if saved.refresh_token == rejected_token
                    && saved.client_id == oauth::client_id()
                    && !*connection.cancelled.borrow()
                {
                    storage.delete(GRANT_FILE, "Could not forget expired Google Drive access")?;
                }
                Ok(())
            },
        )
        .await
    }
}
