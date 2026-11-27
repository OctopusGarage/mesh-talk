//! Account-scoped native policy observations. Secrets never enter status responses.
use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use serde::Serialize;
#[derive(Serialize)]
pub struct TotpStatus {
    pub enabled: bool,
    pub generation: u64,
    pub recovery_remaining: usize,
}
#[tauri::command]
pub async fn account_totp_status(
    owner: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<TotpStatus, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let app = app.inner().clone();
    let node = node.inner().clone();
    let auth = app.auth_operation.clone().lock_owned().await;
    tokio::task::spawn_blocking(move || {
        let _auth = auth;
        let guard = node.0.blocking_read();
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let info = app
            .session()
            .matching(admission.lease(), |v| v.clone())
            .map_err(CommandError::Authorization)?;
        // KDF and disk observation run without the session/lock authority guards.
        // Revalidate immediately before releasing the scoped observation.
        let state = app
            .auth_service()
            .identity_manager()
            .export_account_security(&info.user.name, &info.password)
            .map_err(|e| CommandError::Service(e.to_string()))?;
        admission.checked(runtime, || {
            Ok(TotpStatus {
                enabled: state.totp.is_some(),
                generation: state.generation,
                recovery_remaining: state.totp.as_ref().map_or(0, |v| {
                    v.recovery_hashes
                        .iter()
                        .filter(|h| !state.consumed_recovery.contains(h))
                        .count()
                }),
            })
        })
    })
    .await
    .map_err(|e| CommandError::Internal(e.to_string()))?
}

pub(crate) struct Pending {
    lease: crate::state::SessionLease,
    revision: u64,
    expires: std::time::Instant,
    pub(crate) enrollment: mesh_talk_core::identity::account_security::PendingEnrollment,
}
#[derive(Serialize)]
pub struct Setup {
    pub manual_seed: String,
    pub otpauth_uri: String,
    pub recovery_codes: Vec<String>,
}
fn base32(bytes: &[u8]) -> String {
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
    let mut out = String::new();
    let mut bits = 0u32;
    let mut count = 0;
    for byte in bytes {
        bits = (bits << 8) | u32::from(*byte);
        count += 8;
        while count >= 5 {
            count -= 5;
            out.push(alphabet[((bits >> count) & 31) as usize] as char);
        }
    }
    if count > 0 {
        out.push(alphabet[((bits << (5 - count)) & 31) as usize] as char);
    }
    out
}
#[derive(Serialize)]
pub struct Mutation {
    pub sync_pending: bool,
}
pub(crate) async fn mutate(
    owner: String,
    password: Option<String>,
    code: Option<String>,
    action: u8,
    app: AppState,
    node: NodeState,
) -> Result<serde_json::Value, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let admission = if action == 3 {
        admission
    } else {
        admission
            .verify_identity(password, "change account authentication")
            .await?
    };
    let auth = app.auth_operation.clone().lock_owned().await;
    tokio::task::spawn_blocking(move || {
        let _auth = auth;
        let guard = node.0.blocking_read();
        let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
        let revision = app.app_lock().authorization_revision()?;
        let info = app.session().matching(admission.lease(), |v| v.clone()).map_err(CommandError::Authorization)?;
        let manager = app.auth_service().identity_manager();
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| CommandError::Internal(e.to_string()))?.as_secs();
        let authorize = |commit: &mut dyn FnMut() -> Result<(), mesh_talk_core::identity::errors::IdentityError>| {
            admission.checked_at_lock_revision(runtime, revision, || Ok(commit()))
                .map_err(|e| mesh_talk_core::identity::errors::IdentityError::StorageError(e.to_string()))?
        };
        admission.checked_at_lock_revision(runtime, revision, || Ok(()))?;
        let result = (|| {
            let mut slot = app.totp_pending.lock().map_err(|_| CommandError::Internal("Enrollment state unavailable".into()))?;
            if slot.as_ref().is_some_and(|p| p.lease != *admission.lease() || p.revision != revision || p.expires <= std::time::Instant::now()) { *slot = None; }
            if action == 0 {
                *slot = None;
                drop(slot);
                #[cfg(test)]
                preparation_pause(&owner);
                let enrollment = manager.begin_account_totp(&info.user.name, &info.password, now).map_err(|e| CommandError::Service(e.to_string()))?;
                let manual_seed = base32(enrollment.secret.as_ref());
                let setup = Setup { otpauth_uri: format!("otpauth://totp/Mesh-Talk:{}?secret={manual_seed}&issuer=Mesh-Talk&algorithm=SHA1&digits=6&period=30", info.user.name.as_bytes().iter().map(|b| format!("%{b:02X}")).collect::<String>()), manual_seed, recovery_codes: enrollment.recovery_codes.to_vec() };
                admission.checked_at_lock_revision(runtime, revision, || { let mut slot = app.totp_pending.lock().map_err(|_| CommandError::Internal("Enrollment state unavailable".into()))?; *slot = Some(Pending { lease: admission.lease().clone(), revision, expires: std::time::Instant::now() + std::time::Duration::from_secs(600), enrollment }); Ok(()) })?;
                return serde_json::to_value(setup).map_err(|e| CommandError::Internal(e.to_string()));
            }
            if action == 4 { return Ok(serde_json::Value::Null); }
            if action == 3 { drop(slot); return admission.checked_at_lock_revision(runtime, revision, || { *app.totp_pending.lock().map_err(|_| CommandError::Internal("Enrollment state unavailable".into()))? = None; Ok(serde_json::Value::Null) }); }
            let proof = code.as_deref().ok_or_else(|| CommandError::Validation("Authentication code required".into()))?;
            if action == 1 {
                let pending = slot.as_ref().ok_or_else(|| CommandError::Validation("Enrollment expired".into()))?.enrollment.clone();
                drop(slot);
                #[cfg(test)]
                preparation_pause(&owner);
                manager.confirm_account_totp_if(&info.user.name, &info.password, pending, proof, now, authorize)
            } else {
                *slot = None;
                drop(slot);
                #[cfg(test)]
                preparation_pause(&owner);
                manager.replace_account_totp_if(&info.user.name, &info.password, proof, None, now, authorize)
            }.map_err(|e| CommandError::Service(e.to_string()))?;
            *app.totp_pending.lock().map_err(|_| CommandError::Internal("Enrollment state unavailable".into()))? = None;
            Ok(serde_json::Value::Null)
        })()?;
        if action == 1 || action == 2 || action == 4 {
            // Durable policy remains available for explicit retry if publication fails.
            let sync_pending = crate::shared_account_login::initialize_source(&app, &admission, runtime).is_err();
            return serde_json::to_value(Mutation { sync_pending }).map_err(|e| CommandError::Internal(e.to_string()));
        }
        Ok(result)
    }).await.map_err(|e| CommandError::Internal(e.to_string()))?
}
macro_rules! mutation_command {
    ($name:ident, $action:expr) => {
        #[tauri::command]
        pub async fn $name(
            owner: String,
            password: Option<String>,
            code: Option<String>,
            app: tauri::State<'_, AppState>,
            node: tauri::State<'_, NodeState>,
        ) -> Result<serde_json::Value, CommandError> {
            mutate(
                owner,
                password,
                code,
                $action,
                app.inner().clone(),
                node.inner().clone(),
            )
            .await
        }
    };
}
mutation_command!(account_totp_begin, 0);
mutation_command!(account_totp_confirm, 1);
mutation_command!(account_totp_disable, 2);
mutation_command!(account_totp_cancel, 3);

mutation_command!(account_totp_retry_sync, 4);

// Per-owner, one-shot instrumentation; parallel tests cannot stall another session.
#[cfg(test)]
type PreparationPause = (std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>);
#[cfg(test)]
pub(crate) static PREPARATION_PAUSES: std::sync::LazyLock<
    std::sync::Mutex<std::collections::HashMap<String, PreparationPause>>,
> = std::sync::LazyLock::new(Default::default);
#[cfg(test)]
fn preparation_pause(owner: &str) {
    let pause = PREPARATION_PAUSES.lock().unwrap().remove(owner);
    if let Some((entered, release)) = pause {
        entered.send(()).unwrap();
        release
            .recv_timeout(std::time::Duration::from_secs(20))
            .unwrap();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn setup_base32_matches_rfc_vector() {
        assert_eq!(
            base32(b"12345678901234567890"),
            "GEZDGNBVGY3TQOJQGEZDGNBVGY3TQOJQ"
        );
    }
    #[test]
    fn status_never_contains_secret_material() {
        let value = serde_json::to_value(TotpStatus {
            enabled: false,
            generation: 0,
            recovery_remaining: 0,
        })
        .unwrap();
        assert_eq!(value.as_object().unwrap().len(), 3);
        assert!(value.get("secret").is_none());
    }
}
