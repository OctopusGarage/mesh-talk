//! Encrypted, profile-bound grants. Raw producer credentials are never persisted here.
use crate::{Destination, GrantSpec, IntegrationError};
use mesh_talk_core::storage::record_log::EncryptedRecordLog;
use rand_core::{OsRng, RngCore};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

const MAX_GRANTS: usize = 256;
const MAX_DESTINATIONS: usize = 16;
const MAX_STORE_BYTES: usize = 2 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Grant {
    pub id: String,
    pub client_id: String,
    pub destinations: Vec<Destination>,
    pub describe: bool,
    pub expires_at_ms: u64,
    pub(crate) token_hash: [u8; 32],
}
#[derive(Serialize)]
pub struct IssuedGrant {
    pub grant: Grant,
    pub token: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AuditEntry {
    pub at_ms: u64,
    pub action: String,
    pub client_id: String,
    pub destination: Option<Destination>,
    pub operation_id: Option<String>,
    pub message_id: Option<String>,
}
#[derive(Serialize, Deserialize)]
struct Snapshot {
    version: u32,
    account_id: String,
    grants: Vec<Grant>,
    #[serde(default)]
    audit: Vec<AuditEntry>,
}
pub struct GrantRegistry {
    file: EncryptedRecordLog<Vec<u8>>,
    account_id: String,
    grants: Vec<Grant>,
    poisoned: bool,
    audit: Vec<AuditEntry>,
    rate: HashMap<String, (u64, u32)>,
}
pub fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
pub fn new_token() -> String {
    let mut bytes = [0; 32];
    OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}
pub(crate) fn token_hash(token: &str) -> [u8; 32] {
    Sha256::digest(token.as_bytes()).into()
}
pub(crate) fn equal_hash(a: &[u8; 32], b: &[u8; 32]) -> bool {
    a.iter().zip(b).fold(0u8, |diff, (a, b)| diff | (a ^ b)) == 0
}
fn storage_error() -> IntegrationError {
    IntegrationError::new(
        "STORAGE_FAILURE",
        "Integration permissions could not be persisted",
    )
}
impl GrantRegistry {
    pub fn open(path: &Path, password: &str, account_id: &str) -> Result<Self, IntegrationError> {
        if !crate::model::exact_hex(account_id, 32) {
            return Err(IntegrationError::new(
                "INVALID_INPUT",
                "Invalid cryptographic account fingerprint",
            ));
        }
        if path.exists()
            && std::fs::metadata(path).map_err(|_| storage_error())?.len() > MAX_STORE_BYTES as u64
        {
            return Err(storage_error());
        }
        let (file, mut snapshots) = EncryptedRecordLog::<Vec<u8>>::open(path, password, b"MTIGR1")
            .map_err(|_| storage_error())?;
        if snapshots.len() > 1 {
            return Err(storage_error());
        }
        // Public destinations use internally tagged JSON. Store JSON bytes inside
        // the encrypted framing rather than asking positional bincode to decode
        // serde's deserialize_any-based tagged representation.
        let snapshot = snapshots
            .pop()
            .map(|bytes| serde_json::from_slice::<Snapshot>(&bytes).map_err(|_| storage_error()))
            .transpose()?;
        let (grants, audit) = match snapshot {
            Some(snapshot)
                if snapshot.version == 1
                    && snapshot.account_id == account_id
                    && snapshot.grants.len() <= MAX_GRANTS =>
            {
                if snapshot.audit.len() > 256 {
                    return Err(storage_error());
                }
                (snapshot.grants, snapshot.audit)
            }
            Some(_) => return Err(storage_error()),
            None => (Vec::new(), Vec::new()),
        };
        let mut seen = std::collections::HashSet::new();
        for grant in &grants {
            if !seen.insert(grant.id.clone())
                || grant.client_id.is_empty()
                || grant.destinations.is_empty()
                || grant.destinations.len() > MAX_DESTINATIONS
                || grant
                    .destinations
                    .iter()
                    .any(|destination| destination.validate().is_err())
            {
                return Err(storage_error());
            }
        }
        Ok(Self {
            file,
            account_id: account_id.into(),
            grants,
            poisoned: false,
            audit,
            rate: HashMap::new(),
        })
    }
    fn persist(&mut self) -> Result<(), IntegrationError> {
        let snapshot = Snapshot {
            version: 1,
            account_id: self.account_id.clone(),
            grants: self.grants.clone(),
            audit: self.audit.clone(),
        };
        let encoded = serde_json::to_vec(&snapshot).map_err(|_| storage_error())?;
        // Include record/header overhead so every accepted snapshot can reopen.
        if encoded.len().saturating_add(128) > MAX_STORE_BYTES {
            return Err(IntegrationError::new(
                "CAPACITY_EXCEEDED",
                "Integration grant storage capacity reached",
            ));
        }
        if self
            .file
            .rewrite(&[encoded])
            .and_then(|_| self.file.sync())
            .is_err()
        {
            self.poisoned = true;
            return Err(storage_error());
        }
        Ok(())
    }
    pub fn create(&mut self, spec: GrantSpec) -> Result<IssuedGrant, IntegrationError> {
        if self.poisoned {
            return Err(storage_error());
        }
        if spec.client_id.is_empty()
            || spec.client_id.len() > 128
            || spec.client_id.chars().any(char::is_control)
            || spec.destinations.is_empty()
            || spec.destinations.len() > MAX_DESTINATIONS
            || spec.expires_at_ms <= now_ms()
        {
            return Err(IntegrationError::new(
                "INVALID_INPUT",
                "Provide a client ID, exact destinations and a future grant expiry",
            ));
        }
        for destination in &spec.destinations {
            destination.validate()?;
        }
        self.grants.retain(|grant| grant.expires_at_ms > now_ms());
        self.rate
            .retain(|id, _| self.grants.iter().any(|grant| &grant.id == id));
        if self.grants.len() >= MAX_GRANTS {
            return Err(IntegrationError::new(
                "CAPACITY_EXCEEDED",
                "Too many active integration grants",
            ));
        }
        let token = new_token();
        let grant = Grant {
            id: uuid::Uuid::new_v4().to_string(),
            client_id: spec.client_id,
            destinations: spec.destinations,
            describe: spec.describe,
            expires_at_ms: spec.expires_at_ms,
            token_hash: token_hash(&token),
        };
        self.grants.push(grant.clone());
        self.push_audit(AuditEntry {
            at_ms: now_ms(),
            action: "grant.create".into(),
            client_id: grant.client_id.clone(),
            destination: None,
            operation_id: None,
            message_id: None,
        });
        if let Err(error) = self.persist() {
            self.grants.pop();
            return Err(error);
        }
        Ok(IssuedGrant { grant, token })
    }
    pub fn revoke(&mut self, id: &str) -> Result<(), IntegrationError> {
        if self.poisoned {
            return Err(storage_error());
        }
        let client = self
            .grants
            .iter()
            .find(|grant| grant.id == id)
            .map(|grant| grant.client_id.clone());
        self.grants.retain(|grant| grant.id != id);
        if let Some(client_id) = client {
            self.push_audit(AuditEntry {
                at_ms: now_ms(),
                action: "grant.revoke".into(),
                client_id,
                destination: None,
                operation_id: None,
                message_id: None,
            });
        }
        self.rate.remove(id);
        self.persist()
    }
    pub fn list(&self) -> Result<Vec<Grant>, IntegrationError> {
        if self.poisoned {
            return Err(storage_error());
        }
        Ok(self
            .grants
            .iter()
            .filter(|grant| grant.expires_at_ms > now_ms())
            .cloned()
            .collect())
    }
    pub fn authenticate(
        &self,
        token: &str,
        destination: Option<&Destination>,
        describe: bool,
    ) -> Result<Grant, IntegrationError> {
        if self.poisoned {
            return Err(storage_error());
        }
        let hash = token_hash(token);
        let grant = self
            .grants
            .iter()
            .find(|grant| equal_hash(&hash, &grant.token_hash) && grant.expires_at_ms > now_ms())
            .ok_or_else(|| {
                IntegrationError::new(
                    "UNAUTHENTICATED",
                    "Integration credential expired, revoked or invalid",
                )
            })?;
        if (describe && !grant.describe)
            || destination.is_some_and(|destination| !grant.destinations.contains(destination))
        {
            return Err(IntegrationError::new(
                "SCOPE_DENIED",
                "This grant does not allow the requested operation or destination",
            ));
        }
        Ok(grant.clone())
    }
    fn push_audit(&mut self, entry: AuditEntry) {
        let cutoff = now_ms().saturating_sub(7 * 24 * 60 * 60 * 1000);
        self.audit.retain(|entry| entry.at_ms >= cutoff);
        self.audit.push(entry);
        if self.audit.len() > 256 {
            self.audit.remove(0);
        }
    }
    pub(crate) fn record_send(
        &mut self,
        client_id: &str,
        destination: &Destination,
        operation_id: String,
        message_id: String,
        duplicate: bool,
    ) -> Result<(), IntegrationError> {
        self.push_audit(AuditEntry {
            at_ms: now_ms(),
            action: if duplicate {
                "message.retry"
            } else {
                "message.accept"
            }
            .into(),
            client_id: client_id.into(),
            destination: Some(destination.clone()),
            operation_id: Some(operation_id),
            message_id: Some(message_id),
        });
        self.persist()
    }
    pub(crate) fn audit(&self) -> Result<Vec<AuditEntry>, IntegrationError> {
        if self.poisoned {
            return Err(storage_error());
        }
        let cutoff = now_ms().saturating_sub(7 * 24 * 60 * 60 * 1000);
        Ok(self
            .audit
            .iter()
            .filter(|entry| entry.at_ms >= cutoff)
            .cloned()
            .collect())
    }
    pub(crate) fn charge_send(&mut self, grant: &Grant) -> Result<(), IntegrationError> {
        let now = now_ms();
        let bucket = self.rate.entry(grant.id.clone()).or_insert((now, 5000));
        bucket.1 = bucket
            .1
            .saturating_add(now.saturating_sub(bucket.0).min(5000) as u32)
            .min(5000);
        bucket.0 = now;
        if bucket.1 < 1000 {
            let mut error = IntegrationError::new(
                "RATE_LIMITED",
                "Integration send rate exceeded; retry with the same key after one second",
            );
            error.retryable = true;
            return Err(error);
        }
        bucket.1 -= 1000;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Destination;
    #[test]
    fn grants_bind_client_to_exact_destination_and_never_imply_description() {
        let dir = tempfile::tempdir().unwrap();
        let account = "ab".repeat(16);
        let dest = Destination::Channel {
            id: "cd".repeat(32),
        };
        let mut registry = GrantRegistry::open(&dir.path().join("grants"), "pw", &account).unwrap();
        let issued = registry
            .create(GrantSpec {
                client_id: "build-monitor".into(),
                destinations: vec![dest.clone()],
                describe: false,
                expires_at_ms: now_ms() + 60000,
            })
            .unwrap();
        assert!(
            registry
                .authenticate(&issued.token, Some(&dest), false)
                .is_ok()
        );
        assert!(registry.authenticate(&issued.token, None, true).is_err());
        assert!(
            registry
                .authenticate(
                    &issued.token,
                    Some(&Destination::Account { id: account }),
                    false
                )
                .is_err()
        );
        assert!(
            registry
                .authenticate("wrong-token", Some(&dest), false)
                .is_err()
        );
    }
    #[test]
    fn revocation_is_durable_and_token_never_appears_in_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("grants");
        let account = "ab".repeat(16);
        let mut registry = GrantRegistry::open(&path, "pw", &account).unwrap();
        let issued = registry
            .create(GrantSpec {
                client_id: "agent".into(),
                destinations: vec![Destination::Channel {
                    id: "cd".repeat(32),
                }],
                describe: true,
                expires_at_ms: now_ms() + 60000,
            })
            .unwrap();
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(issued.token.len())
                .any(|part| part == issued.token.as_bytes())
        );
        registry.revoke(&issued.grant.id).unwrap();
        drop(registry);
        let registry = GrantRegistry::open(&path, "pw", &account).unwrap();
        assert_eq!(
            registry
                .authenticate(&issued.token, None, false)
                .unwrap_err()
                .code,
            "UNAUTHENTICATED"
        );
        assert!(GrantRegistry::open(&path, "pw", &"ef".repeat(16)).is_err());
    }
    #[test]
    fn expired_and_empty_scope_grants_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let mut registry =
            GrantRegistry::open(&dir.path().join("grants"), "pw", &"ab".repeat(16)).unwrap();
        assert!(
            registry
                .create(GrantSpec {
                    client_id: "agent".into(),
                    destinations: vec![],
                    describe: false,
                    expires_at_ms: now_ms() + 60000
                })
                .is_err()
        );
        assert!(
            registry
                .create(GrantSpec {
                    client_id: "agent".into(),
                    destinations: vec![Destination::Channel {
                        id: "cd".repeat(32)
                    }],
                    describe: false,
                    expires_at_ms: 1
                })
                .is_err()
        );
    }
}

#[cfg(test)]
mod persistence_tests {
    use super::*;
    #[test]
    fn live_grants_survive_restart_with_typed_destinations() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("grants");
        let account = "ab".repeat(16);
        let mut registry = GrantRegistry::open(&path, "pw", &account).unwrap();
        let issued = registry
            .create(GrantSpec {
                client_id: "restart".into(),
                destinations: vec![Destination::Channel {
                    id: "cd".repeat(32),
                }],
                describe: true,
                expires_at_ms: now_ms() + 60000,
            })
            .unwrap();
        drop(registry);
        let registry = GrantRegistry::open(&path, "pw", &account).unwrap();
        assert_eq!(
            registry.authenticate(&issued.token, None, true).unwrap().id,
            issued.grant.id
        );
    }
}

#[cfg(test)]
mod bound_tests {
    use super::*;
    #[test]
    fn oversized_snapshots_fail_before_creating_an_unreadable_store() {
        let directory = tempfile::tempdir().unwrap();
        let mut registry =
            GrantRegistry::open(&directory.path().join("grants"), "pw", &"ab".repeat(16)).unwrap();
        let issued = registry
            .create(GrantSpec {
                client_id: "first".into(),
                destinations: vec![Destination::Channel {
                    id: "cd".repeat(32),
                }],
                describe: false,
                expires_at_ms: now_ms() + 60000,
            })
            .unwrap();
        let mut oversized = issued.grant.clone();
        oversized.client_id = "x".repeat(3 * 1024 * 1024);
        registry.grants.push(oversized);
        assert_eq!(registry.persist().unwrap_err().code, "CAPACITY_EXCEEDED");
    }
    #[test]
    fn creating_a_grant_retires_expired_rate_buckets() {
        let directory = tempfile::tempdir().unwrap();
        let mut registry =
            GrantRegistry::open(&directory.path().join("grants"), "pw", &"ab".repeat(16)).unwrap();
        registry.rate.insert("expired-grant".into(), (now_ms(), 0));
        registry
            .create(GrantSpec {
                client_id: "new".into(),
                destinations: vec![Destination::Channel {
                    id: "cd".repeat(32),
                }],
                describe: false,
                expires_at_ms: now_ms() + 60000,
            })
            .unwrap();
        assert!(registry.rate.is_empty());
    }
}
