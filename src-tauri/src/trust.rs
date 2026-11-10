//! Installation-local TOFU observations and manual device verification.
//! Original observation history never changes when the user re-verifies a device.
//! Only the current manually verified fingerprint authorizes DM history pulls.

use crate::chat_commands::NodeState;
use crate::commands::CommandError;
use crate::config_store;
use crate::runtime_admission::RuntimeAdmission;
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
    /// Installation-local time; legacy records have no recoverable observation date.
    #[serde(default)]
    pub first_seen_at_ms: Option<u64>,
    /// Separate from immutable first-observed history; absent in legacy records.
    #[serde(default)]
    pub verified_fingerprint: Option<String>,
    #[serde(default)]
    pub last_observed_fingerprint: Option<String>,
    #[serde(default)]
    pub device_changes: Vec<DeviceChange>,
    /// True once the user has confirmed the safety number out-of-band.
    #[serde(default)]
    pub verified: bool,
}

/// A change observed locally, not a claim about when the remote device was created.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeviceChange {
    pub previous_fingerprint: String,
    pub current_fingerprint: String,
    pub observed_at_ms: u64,
}

/// Current private state, independent of its versioned persistence envelope.
#[derive(Clone, Debug, Default)]
struct TrustFile {
    contacts: HashMap<String, TrustRecord>,
}

impl Serialize for TrustFile {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        #[derive(Serialize)]
        struct Versioned<'a> {
            schema: u32,
            records: &'a HashMap<String, TrustRecord>,
        }
        // Older clients interpret first_seen_fingerprint as the current manual
        // verification pin. Omitting their contacts field makes downgrades fail
        // closed instead of authorizing the immutable original observation.
        Versioned {
            schema: 2,
            records: &self.contacts,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TrustFile {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Versioned {
            schema: u32,
            records: HashMap<String, TrustRecord>,
        }
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Legacy {
            contacts: HashMap<String, TrustRecord>,
        }
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Stored {
            Versioned(Versioned),
            Legacy(Legacy),
        }
        let contacts = match Stored::deserialize(deserializer)? {
            Stored::Versioned(file) => {
                if file.schema != 2 {
                    return Err(serde::de::Error::custom(
                        "unsupported private trust storage schema",
                    ));
                }
                file.records
            }
            Stored::Legacy(file) => file.contacts,
        };
        Ok(Self { contacts })
    }
}

/// Serializes trust table read/modify/write operations within the process.
#[derive(Clone, Default)]
pub struct TrustState(Arc<Mutex<()>>);

/// Trust state returned to the UI for one contact, including the live change signal.
#[derive(Clone, Debug, Serialize)]
pub struct TrustInfo {
    pub account_id: String,
    /// Immutable original observation, even after a later device is verified.
    pub first_seen_fingerprint: String,
    pub first_seen_at_ms: Option<u64>,
    pub device_changes: Vec<DeviceChange>,
    pub verified: bool,
    /// Current fingerprint differs from the manual verification pin (or original TOFU pin).
    pub fingerprint_changed: bool,
    /// Whether we have any record for this contact yet (false = brand-new contact).
    pub known: bool,
}

/// The persistence file inside the host account's profile.
const TRUST_FILE: &str = "trust.json";

const MAX_DEVICE_CHANGES: usize = 32;

fn observed_now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}

/// Return whether any new observation must be persisted. Repeated reads do not
/// change timestamps or append duplicate device changes.
fn evaluate(
    file: &mut TrustFile,
    account_id: String,
    current_fingerprint: String,
) -> (TrustInfo, bool) {
    evaluate_at(file, account_id, current_fingerprint, observed_now_ms())
}

fn evaluate_at(
    file: &mut TrustFile,
    account_id: String,
    current_fingerprint: String,
    observed_at_ms: u64,
) -> (TrustInfo, bool) {
    let known = file.contacts.contains_key(&account_id);
    let rec = file
        .contacts
        .entry(account_id.clone())
        .or_insert_with(|| TrustRecord {
            first_seen_fingerprint: current_fingerprint.clone(),
            first_seen_at_ms: Some(observed_at_ms),
            verified_fingerprint: None,
            last_observed_fingerprint: Some(current_fingerprint.clone()),
            device_changes: Vec::new(),
            verified: false,
        });
    let previous = rec.last_observed_fingerprint.as_deref().unwrap_or_else(|| {
        rec.verified_fingerprint
            .as_deref()
            .unwrap_or(&rec.first_seen_fingerprint)
    });
    let changed = previous != current_fingerprint;
    if changed {
        rec.device_changes.push(DeviceChange {
            previous_fingerprint: previous.to_owned(),
            current_fingerprint: current_fingerprint.clone(),
            observed_at_ms,
        });
        if rec.device_changes.len() > MAX_DEVICE_CHANGES {
            rec.device_changes
                .drain(..rec.device_changes.len() - MAX_DEVICE_CHANGES);
        }
    }
    let needs_write = !known || changed || rec.last_observed_fingerprint.is_none();
    rec.last_observed_fingerprint = Some(current_fingerprint.clone());
    let pin = rec
        .verified_fingerprint
        .as_ref()
        .unwrap_or(&rec.first_seen_fingerprint);
    (
        TrustInfo {
            account_id,
            fingerprint_changed: pin != &current_fingerprint,
            first_seen_fingerprint: rec.first_seen_fingerprint.clone(),
            first_seen_at_ms: rec.first_seen_at_ms,
            device_changes: rec.device_changes.clone(),
            verified: rec.verified,
            known,
        },
        needs_write,
    )
}

/// Read an existing local overlay when no live device can be supplied. Missing
/// history remains unknown; this path never creates an observation or migration.
fn offline_trust_info(file: &TrustFile, account_id: String) -> TrustInfo {
    let Some(record) = file.contacts.get(&account_id) else {
        return TrustInfo {
            account_id,
            first_seen_fingerprint: String::new(),
            first_seen_at_ms: None,
            device_changes: Vec::new(),
            verified: false,
            fingerprint_changed: false,
            known: false,
        };
    };
    let pin = record
        .verified_fingerprint
        .as_ref()
        .unwrap_or(&record.first_seen_fingerprint);
    let last_observed = record.last_observed_fingerprint.as_ref().unwrap_or(pin);
    TrustInfo {
        account_id,
        first_seen_fingerprint: record.first_seen_fingerprint.clone(),
        first_seen_at_ms: record.first_seen_at_ms,
        device_changes: record.device_changes.clone(),
        verified: record.verified,
        fingerprint_changed: last_observed != pin,
        known: true,
    }
}

/// Re-verification changes the authorization pin, never original observation history.
fn set_verified(file: &mut TrustFile, account_id: String, fingerprint: String) {
    evaluate(file, account_id.clone(), fingerprint.clone());
    let record = file
        .contacts
        .get_mut(&account_id)
        .expect("observation inserted");
    record.verified = true;
    record.verified_fingerprint = Some(fingerprint);
}

fn verified_fingerprints(file: &TrustFile) -> Vec<String> {
    file.contacts
        .values()
        .filter(|record| record.verified)
        .map(|record| {
            record
                .verified_fingerprint
                .as_ref()
                .unwrap_or(&record.first_seen_fingerprint)
                .clone()
        })
        .collect()
}

pub(crate) fn verified_fingerprints_at(path: &Path) -> Vec<String> {
    verified_fingerprints(&config_store::read_account::<TrustFile>(path))
}

/// Observe a certified device/account binding, or read local history when the
/// current fingerprint is empty because no device is available.
#[tauri::command]
pub async fn get_trust(
    app: tauri::State<'_, AppState>,
    state: tauri::State<'_, TrustState>,
    node_state: tauri::State<'_, NodeState>,
    owner: String,
    account_id: String,
    current_fingerprint: String,
) -> Result<TrustInfo, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node_state, &owner)?;
    let state = state.inner().clone();
    let path = config_store::account_path(&crate::data_dir(), &owner, TRUST_FILE)
        .ok_or_else(|| CommandError::Authorization("invalid account owner".into()))?;
    admission
        .blocking(move |runtime, _| {
            if !current_fingerprint.is_empty() {
                require_certified_binding(runtime, &account_id, &current_fingerprint)?;
            }
            let _guard = state.0.lock().unwrap();
            let mut file = read_trust(&path)?;
            if current_fingerprint.is_empty() {
                return Ok(offline_trust_info(&file, account_id));
            }
            let (info, changed) = evaluate(&mut file, account_id, current_fingerprint);
            if changed {
                config_store::write_account(&path, &file)?;
            }
            Ok(info)
        })
        .await
}

fn require_certified_binding(
    runtime: &mesh_talk_core::node::NodeRuntime,
    account_id: &str,
    fingerprint: &str,
) -> Result<(), CommandError> {
    let binding = runtime
        .peer_public(fingerprint)
        .and_then(|public| runtime.account_id_for_device(&public));
    if binding.as_deref() != Some(account_id) {
        return Err(CommandError::Authorization(
            "device is not certified for this account".into(),
        ));
    }
    Ok(())
}

/// A failed or corrupt read must not erase private history on the next observation.
fn read_trust(path: &Path) -> Result<TrustFile, CommandError> {
    match std::fs::read(path) {
        Ok(raw) => {
            serde_json::from_slice(&raw).map_err(|error| CommandError::Io(error.to_string()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(TrustFile::default()),
        Err(error) => Err(CommandError::Io(error.to_string())),
    }
}

/// Mark a contact verified. The user is asserting they compared the safety number
/// out-of-band; we update only the current verification pin, so
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
    let admission = RuntimeAdmission::capture(&app, &node_state, &owner)?;
    let state = state.inner().clone();
    let path = config_store::account_path(&crate::data_dir(), &owner, TRUST_FILE)
        .ok_or_else(|| CommandError::Authorization("invalid account owner".into()))?;
    admission
        .blocking(move |runtime, _| {
            require_certified_binding(runtime, &account_id, &fingerprint)?;
            let _guard = state.0.lock().unwrap();
            let mut file = read_trust(&path)?;
            set_verified(&mut file, account_id, fingerprint);
            config_store::write_account(&path, &file)?;
            runtime
                .handle()
                .replace_verified_dm_pull_peers(verified_fingerprints(&file));
            Ok(())
        })
        .await
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
        assert!(pinned);
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
    #[test]
    fn first_observation_has_a_local_timestamp() {
        let mut file = TrustFile::default();
        let (info, _) = evaluate(&mut file, "acct".into(), "fp1".into());
        let json = serde_json::to_value(info).unwrap();
        assert!(json["first_seen_at_ms"].as_u64().is_some());
    }

    #[test]
    fn changed_device_is_persisted_once_and_preserved_by_reverification() {
        let mut file = TrustFile::default();
        evaluate(&mut file, "acct".into(), "fp1".into());
        let (_, changed) = evaluate(&mut file, "acct".into(), "fp2".into());
        assert!(changed, "new evidence must be persisted");
        let (_, changed) = evaluate(&mut file, "acct".into(), "fp2".into());
        assert!(
            !changed,
            "identical observations should not rewrite history"
        );
        set_verified(&mut file, "acct".into(), "fp2".into());
        let (info, _) = evaluate(&mut file, "acct".into(), "fp2".into());
        assert_eq!(info.first_seen_fingerprint, "fp1");
        assert!(!info.fingerprint_changed);
        let json = serde_json::to_value(info).unwrap();
        assert_eq!(json["device_changes"].as_array().unwrap().len(), 1);
        assert_eq!(json["device_changes"][0]["previous_fingerprint"], "fp1");
        assert_eq!(verified_fingerprints(&file), vec!["fp2"]);
    }
    #[test]
    fn legacy_records_keep_unknown_first_observation_date_and_verification_authority() {
        let mut file: TrustFile = serde_json::from_str(
            r#"{"contacts":{"acct":{"first_seen_fingerprint":"legacy-fp","verified":true}}}"#,
        )
        .unwrap();
        let (info, migrated) = evaluate_at(&mut file, "acct".into(), "legacy-fp".into(), 100);
        assert!(migrated);
        assert_eq!(info.first_seen_at_ms, None);
        assert!(info.device_changes.is_empty());
        assert_eq!(verified_fingerprints(&file), vec!["legacy-fp"]);
        let (info, changed) = evaluate_at(&mut file, "acct".into(), "new-fp".into(), 200);
        assert!(changed);
        assert_eq!(info.first_seen_at_ms, None);
        assert_eq!(info.device_changes[0].observed_at_ms, 200);
        set_verified(&mut file, "acct".into(), "new-fp".into());
        let (info, _) = evaluate_at(&mut file, "acct".into(), "new-fp".into(), 300);
        assert_eq!(info.first_seen_at_ms, None);
        assert_eq!(info.first_seen_fingerprint, "legacy-fp");
        assert_eq!(info.device_changes.len(), 1);
        assert_eq!(verified_fingerprints(&file), vec!["new-fp"]);
        let json = serde_json::to_vec(&file).unwrap();
        let restored: TrustFile = serde_json::from_slice(&json).unwrap();
        assert_eq!(restored.contacts["acct"].first_seen_at_ms, None);
        assert_eq!(
            restored.contacts["acct"].device_changes,
            info.device_changes
        );
    }

    #[test]
    fn history_keeps_latest_bounded_changes_without_changing_first_observation() {
        let mut file = TrustFile::default();
        evaluate_at(&mut file, "acct".into(), "fp0".into(), 50);
        for index in 1..=40 {
            evaluate_at(&mut file, "acct".into(), format!("fp{index}"), 100 + index);
        }
        set_verified(&mut file, "acct".into(), "fp40".into());
        let (info, changed) = evaluate_at(&mut file, "acct".into(), "fp40".into(), 200);
        assert!(!changed);
        assert_eq!(info.first_seen_at_ms, Some(50));
        assert_eq!(info.first_seen_fingerprint, "fp0");
        assert_eq!(info.device_changes.len(), MAX_DEVICE_CHANGES);
        assert_eq!(info.device_changes[0].previous_fingerprint, "fp8");
        assert_eq!(info.device_changes[0].observed_at_ms, 109);
        assert_eq!(
            info.device_changes.last().unwrap().current_fingerprint,
            "fp40"
        );
        assert_eq!(verified_fingerprints(&file), vec!["fp40"]);
    }

    #[test]
    fn returning_to_an_earlier_fingerprint_is_another_observed_change() {
        let mut file = TrustFile::default();
        evaluate_at(&mut file, "acct".into(), "fp1".into(), 10);
        evaluate_at(&mut file, "acct".into(), "fp2".into(), 20);
        let (info, changed) = evaluate_at(&mut file, "acct".into(), "fp1".into(), 30);
        assert!(changed);
        assert!(!info.fingerprint_changed);
        assert_eq!(info.device_changes.len(), 2);
        assert_eq!(info.device_changes[1].previous_fingerprint, "fp2");
        assert_eq!(info.device_changes[1].current_fingerprint, "fp1");
        assert_eq!(info.device_changes[1].observed_at_ms, 30);
    }
    #[test]
    fn malformed_or_unreadable_history_is_not_treated_as_a_new_installation() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trust.json");
        assert!(read_trust(&path).unwrap().contacts.is_empty());
        let corrupt = b"{incomplete private history";
        std::fs::write(&path, corrupt).unwrap();
        assert!(read_trust(&path).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), corrupt);
        assert!(read_trust(root.path()).is_err());
    }

    #[test]
    fn legacy_history_migration_persists_changes_without_inventing_first_seen_date() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trust.json");
        std::fs::write(
            &path,
            r#"{"contacts":{"acct":{"first_seen_fingerprint":"old","verified":true}}}"#,
        )
        .unwrap();
        let mut file = read_trust(&path).unwrap();
        let (_, changed) = evaluate_at(&mut file, "acct".into(), "new".into(), 500);
        assert!(changed);
        config_store::write_account(&path, &file).unwrap();
        let mut restored = read_trust(&path).unwrap();
        let (info, changed) = evaluate_at(&mut restored, "acct".into(), "new".into(), 600);
        assert!(!changed);
        assert_eq!(info.first_seen_at_ms, None);
        assert_eq!(info.device_changes.len(), 1);
        assert_eq!(info.device_changes[0].observed_at_ms, 500);
        assert_eq!(verified_fingerprints(&restored), vec!["old"]);
    }
    #[test]
    fn offline_lookup_without_history_remains_unknown_and_does_not_create_observation() {
        let file = TrustFile::default();
        let info = offline_trust_info(&file, "offline".into());
        assert!(!info.known);
        assert!(!info.verified);
        assert!(!info.fingerprint_changed);
        assert_eq!(info.first_seen_at_ms, None);
        assert!(info.first_seen_fingerprint.is_empty());
        assert!(info.device_changes.is_empty());
        assert!(file.contacts.is_empty());
    }

    #[test]
    fn offline_lookup_preserves_observed_change_and_original_timestamp() {
        let mut file = TrustFile::default();
        evaluate_at(&mut file, "acct".into(), "old".into(), 100);
        set_verified(&mut file, "acct".into(), "old".into());
        evaluate_at(&mut file, "acct".into(), "new".into(), 200);
        let before = serde_json::to_vec(&file).unwrap();
        let info = offline_trust_info(&file, "acct".into());
        assert!(info.known);
        assert!(info.verified);
        assert!(info.fingerprint_changed);
        assert_eq!(info.first_seen_at_ms, Some(100));
        assert_eq!(info.first_seen_fingerprint, "old");
        assert_eq!(info.device_changes.len(), 1);
        assert_eq!(info.device_changes[0].observed_at_ms, 200);
        assert_eq!(serde_json::to_vec(&file).unwrap(), before);
    }

    #[test]
    fn offline_legacy_lookup_does_not_migrate_or_invent_timestamp() {
        let file: TrustFile = serde_json::from_str(
            r#"{"contacts":{"acct":{"first_seen_fingerprint":"legacy","verified":true}}}"#,
        )
        .unwrap();
        let before = serde_json::to_vec(&file).unwrap();
        let info = offline_trust_info(&file, "acct".into());
        assert!(info.known);
        assert!(info.verified);
        assert!(!info.fingerprint_changed);
        assert_eq!(info.first_seen_at_ms, None);
        assert_eq!(info.first_seen_fingerprint, "legacy");
        assert!(info.device_changes.is_empty());
        assert_eq!(serde_json::to_vec(&file).unwrap(), before);
        assert_eq!(file.contacts["acct"].last_observed_fingerprint, None);
    }
    #[test]
    fn versioned_reverification_does_not_restore_original_fingerprint_authority_in_legacy_reader() {
        // Mirror HEAD's persistence decoder: unknown fields are ignored, and a
        // missing contacts map defaults to empty. Its pin is first_seen_fingerprint.
        #[derive(Deserialize)]
        struct LegacyRecord {
            first_seen_fingerprint: String,
            #[serde(default)]
            verified: bool,
        }
        #[derive(Default, Deserialize)]
        struct LegacyFile {
            #[serde(default)]
            contacts: HashMap<String, LegacyRecord>,
        }
        let mut file = TrustFile::default();
        evaluate_at(&mut file, "acct".into(), "A".into(), 100);
        set_verified(&mut file, "acct".into(), "A".into());
        evaluate_at(&mut file, "acct".into(), "B".into(), 200);
        set_verified(&mut file, "acct".into(), "B".into());
        let json = serde_json::to_string(&file).unwrap();
        let legacy: LegacyFile = serde_json::from_str(&json).unwrap();
        assert!(
            legacy.contacts.is_empty(),
            "older readers must fail closed after downgrade"
        );
        let old_authority: Vec<_> = legacy
            .contacts
            .values()
            .filter(|record| record.verified)
            .map(|record| record.first_seen_fingerprint.clone())
            .collect();
        assert!(
            old_authority.is_empty(),
            "A must never regain verification authority"
        );
        let envelope: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(envelope["schema"], 2);
        assert!(envelope.get("contacts").is_none());
        assert!(envelope["records"].get("acct").is_some());
        let restored: TrustFile = serde_json::from_str(&json).unwrap();
        assert_eq!(verified_fingerprints(&restored), vec!["B"]);
        let info = offline_trust_info(&restored, "acct".into());
        assert_eq!(info.first_seen_fingerprint, "A");
        assert_eq!(info.first_seen_at_ms, Some(100));
        assert_eq!(
            info.device_changes,
            vec![DeviceChange {
                previous_fingerprint: "A".into(),
                current_fingerprint: "B".into(),
                observed_at_ms: 200,
            }]
        );
        assert!(info.verified);
        assert!(!info.fingerprint_changed);
    }

    #[test]
    fn legacy_verified_record_migrates_to_versioned_storage_with_same_authority_and_unknown_time() {
        let legacy =
            r#"{"contacts":{"acct":{"first_seen_fingerprint":"legacy-pin","verified":true}}}"#;
        let file: TrustFile = serde_json::from_str(legacy).unwrap();
        assert_eq!(verified_fingerprints(&file), vec!["legacy-pin"]);
        let encoded = serde_json::to_vec(&file).unwrap();
        let restored: TrustFile = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(verified_fingerprints(&restored), vec!["legacy-pin"]);
        let record = &restored.contacts["acct"];
        assert_eq!(record.first_seen_fingerprint, "legacy-pin");
        assert_eq!(record.first_seen_at_ms, None);
        assert_eq!(record.last_observed_fingerprint, None);
        assert!(record.device_changes.is_empty());
    }

    #[test]
    fn unsupported_malformed_or_mixed_storage_envelopes_are_errors_and_remain_untouched() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("trust.json");
        for raw in [
            r#"{"schema":3,"records":{}}"#,
            r#"{"schema":2}"#,
            r#"{"schema":2,"records":null}"#,
            r#"{"schema":2,"records":{},"contacts":{}}"#,
            r#"{"schema":2,"contacts":{}}"#,
            r#"{"records":{}}"#,
            r#"{"schema":2,"records":{},"unexpected":true}"#,
            r#"{"schema":2,"records":{"acct":{"verified":true}}}"#,
            r#"{"contacts":null}"#,
            r#"{}"#,
        ] {
            std::fs::write(&path, raw).unwrap();
            assert!(
                read_trust(&path).is_err(),
                "invalid envelope accepted: {raw}"
            );
            assert_eq!(std::fs::read_to_string(&path).unwrap(), raw);
        }
    }
}
