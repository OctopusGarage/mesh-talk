//! TOFU (trust-on-first-use) contact verification, layered on top of the existing
//! fingerprints — it adds NO new crypto and changes no key material. We persist, per
//! contact account, the fingerprint we first saw and whether the user has verified it
//! out-of-band (compared the safety number in person / over a trusted channel).
//!
//! Why key by `account_id`: the UI groups a multi-device contact under one account, so
//! that is the stable contact identity. The fingerprint we pin is the contact's current
//! device `user_id`. If a KNOWN account later presents a DIFFERENT device fingerprint
//! (a new/replaced device — or, in the worst case, an attacker who has taken over the
//! account label), we surface a `fingerprint_changed` flag so the UI can warn loudly
//! and the user can re-verify. The pinned value is the FIRST-seen fingerprint; we never
//! silently overwrite it.

use crate::chat_commands::NodeState;
use crate::commands::CommandError;
use crate::config_store;
use crate::state::AppState;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

/// What we persist about one contact account.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TrustRecord {
    /// The device fingerprint (`user_id`) we first saw presenting this account.
    pub first_seen_fingerprint: String,
    /// True once the user has confirmed the safety number out-of-band.
    #[serde(default)]
    pub verified: bool,
}

/// The on-disk shape: `account_id` -> record.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct TrustFile {
    #[serde(default)]
    contacts: HashMap<String, TrustRecord>,
}

/// Serializes trust table read/modify/write operations within the process.
#[derive(Clone, Default)]
pub struct TrustState(Arc<Mutex<()>>);

/// Trust state returned to the UI for one contact, including the live change signal.
#[derive(Clone, Debug, Serialize)]
pub struct TrustInfo {
    pub account_id: String,
    /// The fingerprint we pinned on first contact (the one the user should verify).
    pub first_seen_fingerprint: String,
    pub verified: bool,
    /// True if `current_fingerprint` differs from the pinned `first_seen_fingerprint`
    /// — a known contact's fingerprint changed; the UI must warn.
    pub fingerprint_changed: bool,
    /// Whether we have any record for this contact yet (false = brand-new contact).
    pub known: bool,
}

/// The persistence file inside the host account's profile.
const TRUST_FILE: &str = "trust.json";

/// Pure TOFU evaluation over the trust table. On first sight of `account_id`, pins
/// `current_fingerprint` as the first-seen value (mutating `file`) and returns
/// `known: false`. On a known account, reports `fingerprint_changed` against the pinned
/// value without ever overwriting it. Returns `(info, pinned_now)` where `pinned_now` is
/// true exactly when this call performed the first-sight pin (so the caller can persist).
fn evaluate(
    file: &mut TrustFile,
    account_id: String,
    current_fingerprint: String,
) -> (TrustInfo, bool) {
    match file.contacts.get(&account_id).cloned() {
        Some(rec) => (
            TrustInfo {
                account_id,
                fingerprint_changed: rec.first_seen_fingerprint != current_fingerprint,
                first_seen_fingerprint: rec.first_seen_fingerprint,
                verified: rec.verified,
                known: true,
            },
            false,
        ),
        None => {
            // First use: pin the current fingerprint.
            file.contacts.insert(
                account_id.clone(),
                TrustRecord {
                    first_seen_fingerprint: current_fingerprint.clone(),
                    verified: false,
                },
            );
            (
                TrustInfo {
                    account_id,
                    first_seen_fingerprint: current_fingerprint,
                    verified: false,
                    fingerprint_changed: false,
                    // Brand-new contact: no record existed before this call.
                    known: false,
                },
                true,
            )
        }
    }
}

/// Pure: (re-)pin `fingerprint` as the trusted first-seen value and mark verified.
fn set_verified(file: &mut TrustFile, account_id: String, fingerprint: String) {
    file.contacts.insert(
        account_id,
        TrustRecord {
            first_seen_fingerprint: fingerprint,
            verified: true,
        },
    );
}

fn verified_fingerprints(file: &TrustFile) -> Vec<String> {
    file.contacts
        .values()
        .filter(|record| record.verified)
        .map(|record| record.first_seen_fingerprint.clone())
        .collect()
}

pub(crate) fn verified_fingerprints_at(path: &Path) -> Vec<String> {
    verified_fingerprints(&config_store::read_account::<TrustFile>(path))
}

/// Fetch trust state for a contact account, given its CURRENT device fingerprint.
///
/// TOFU: if we have never seen this account, we pin `current_fingerprint` as the
/// first-seen value (and persist) so subsequent calls can detect a change. A brand-new
/// contact comes back `known: false, verified: false, fingerprint_changed: false`.
#[tauri::command]
pub fn get_trust(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, TrustState>,
    owner: String,
    account_id: String,
    current_fingerprint: String,
) -> Result<TrustInfo, CommandError> {
    config_store::with_account(&app, &owner, TRUST_FILE, |path| {
        let _guard = state.0.lock().unwrap();
        let mut file: TrustFile = config_store::read_account(path);
        let (info, pinned_now) = evaluate(&mut file, account_id, current_fingerprint);
        if pinned_now {
            config_store::write_account(path, &file)?;
        }
        Ok(info)
    })
}

/// Mark a contact verified. The user is asserting they compared the safety number
/// out-of-band; we also (re-)pin `fingerprint` as the trusted first-seen value, so
/// verifying after a legitimate fingerprint change clears the warning.
#[tauri::command]
pub async fn mark_verified(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, TrustState>,
    node_state: tauri::State<'_, NodeState>,
    owner: String,
    account_id: String,
    fingerprint: String,
) -> Result<(), CommandError> {
    let node = node_state.0.lock().await;
    let fingerprints = config_store::with_account(&app, &owner, TRUST_FILE, |path| {
        let _guard = state.0.lock().unwrap();
        let mut file: TrustFile = config_store::read_account(path);
        set_verified(&mut file, account_id, fingerprint);
        config_store::write_account(path, &file)?;
        Ok(verified_fingerprints(&file))
    })?;
    if let Some(runtime) = node
        .as_ref()
        .filter(|runtime| runtime.host_account_id() == Some(&owner))
    {
        runtime
            .handle()
            .replace_verified_dm_pull_peers(fingerprints);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sight_pins_fingerprint_and_is_unknown() {
        let mut file = TrustFile::default();
        let (info, pinned) = evaluate(&mut file, "acct".into(), "fp1".into());
        assert!(pinned);
        assert!(!info.known); // brand-new contact (documented behavior)
        assert!(!info.verified);
        assert!(!info.fingerprint_changed);
        assert_eq!(info.first_seen_fingerprint, "fp1");
        // The fingerprint is now pinned in the table.
        assert_eq!(
            file.contacts.get("acct").unwrap().first_seen_fingerprint,
            "fp1"
        );
    }

    #[test]
    fn second_sight_same_fingerprint_is_known_unchanged() {
        let mut file = TrustFile::default();
        evaluate(&mut file, "acct".into(), "fp1".into());
        let (info, pinned) = evaluate(&mut file, "acct".into(), "fp1".into());
        assert!(!pinned);
        assert!(info.known);
        assert!(!info.fingerprint_changed);
        assert_eq!(info.first_seen_fingerprint, "fp1");
    }

    #[test]
    fn second_sight_different_fingerprint_flags_change_keeps_pin() {
        let mut file = TrustFile::default();
        evaluate(&mut file, "acct".into(), "fp1".into());
        let (info, pinned) = evaluate(&mut file, "acct".into(), "fp2".into());
        assert!(!pinned);
        assert!(info.known);
        assert!(info.fingerprint_changed);
        // The pinned first-seen value is never silently overwritten.
        assert_eq!(info.first_seen_fingerprint, "fp1");
        assert_eq!(
            file.contacts.get("acct").unwrap().first_seen_fingerprint,
            "fp1"
        );
    }

    #[test]
    fn mark_verified_then_evaluate_reports_verified() {
        let mut file = TrustFile::default();
        set_verified(&mut file, "acct".into(), "fp1".into());
        let (info, pinned) = evaluate(&mut file, "acct".into(), "fp1".into());
        assert!(!pinned);
        assert!(info.known);
        assert!(info.verified);
    }

    #[test]
    fn verified_persists_across_reevaluate_same_fingerprint() {
        let mut file = TrustFile::default();
        set_verified(&mut file, "acct".into(), "fp1".into());
        evaluate(&mut file, "acct".into(), "fp1".into());
        let (info, _) = evaluate(&mut file, "acct".into(), "fp1".into());
        assert!(info.verified);
        assert!(!info.fingerprint_changed);
    }

    #[test]
    fn only_manually_verified_fingerprints_authorize_dm_pull() {
        let mut file = TrustFile::default();
        evaluate(&mut file, "known".into(), "known-fp".into());
        set_verified(&mut file, "verified".into(), "old-fp".into());
        evaluate(&mut file, "verified".into(), "new-fp".into());
        assert_eq!(verified_fingerprints(&file), vec!["old-fp"]);
    }
}
