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
        recovery::setup::with_storage(&self.app, &self.node, Some(self.generation), |manager, username, password| {
            if !manager.file_exists(username, GRANT_FILE) { return Ok(None); }
            let saved: SavedGrant = manager.read_encrypted_file(username, GRANT_FILE, password)
                .map_err(|_| CommandError::Io("Could not unlock the saved Google Drive connection. Disconnect it and connect again.".into()))?;
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
            move |manager, username, password| {
                // Checking cancellation inside the serialized storage operation also
                // prevents a late OAuth response from undoing an explicit forget.
                if *connection.cancelled.borrow() {
                    return Err(CommandError::Authorization(
                        "Google Drive connection was cancelled".into(),
                    ));
                }
                let saved = SavedGrant {
                    client_id: oauth::client_id().into(),
                    refresh_token,
                };
                manager
                    .write_encrypted_file(username, GRANT_FILE, &saved, password)
                    .map_err(|_| {
                        CommandError::Io(
                            "Could not remember Google Drive securely. Try connecting again."
                                .into(),
                        )
                    })
            },
        )
        .await
    }
    pub(super) async fn forget(&self) -> Result<(), CommandError> {
        recovery::setup::with_storage(
            &self.app,
            &self.node,
            Some(self.generation),
            |manager, username, _| {
                if !manager.file_exists(username, GRANT_FILE) {
                    return Ok(());
                }
                manager.delete_file(username, GRANT_FILE).map_err(|_| {
                    CommandError::Io("Could not forget Google Drive. Try again.".into())
                })
            },
        )
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
            move |manager, username, password| {
                if *connection.cancelled.borrow() || !manager.file_exists(username, GRANT_FILE) {
                    return Ok(());
                }
                let saved: SavedGrant = manager
                    .read_encrypted_file(username, GRANT_FILE, password)
                    .map_err(|_| {
                        CommandError::Io(
                            "Could not unlock the saved Google Drive connection".into(),
                        )
                    })?;
                // An older failed refresh must not forget a more recent Google login.
                if saved.refresh_token == rejected_token && saved.client_id == oauth::client_id() {
                    manager.delete_file(username, GRANT_FILE).map_err(|_| {
                        CommandError::Io("Could not forget expired Google Drive access".into())
                    })?;
                }
                Ok(())
            },
        )
        .await
    }
}
