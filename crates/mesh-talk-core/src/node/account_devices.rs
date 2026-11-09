//! Persistent, certificate-validated roster of devices linked to this account.

use crate::eventlog::LogError;
use crate::identity::account::DeviceCertificate;
use crate::identity::device::PublicIdentity;
use crate::storage::record_log::EncryptedRecordLog;
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTACDV";
const MAX_LINKED_DEVICES: usize = 64;

/// Return the encrypted roster path for an account in the node data directory.
pub(crate) fn file_path(dir: &Path, account_id: &str) -> std::path::PathBuf {
    dir.join(format!("account-devices-{account_id}.log"))
}

/// Public identity and account-issued certificate for one linked device.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountDevice {
    pub public: PublicIdentity,
    pub certificate: DeviceCertificate,
}

/// Encrypted, bounded roster of devices certified by the local account.
pub struct AccountDevices {
    file: EncryptedRecordLog<AccountDevice>,
    by_user: HashMap<String, AccountDevice>,
    own_account_id: String,
}

impl AccountDevices {
    /// Open the roster and reject corrupt or conflicting device certificates.
    pub fn open(path: &Path, password: &str, own_account_id: &str) -> Result<Self, LogError> {
        let (file, entries) = EncryptedRecordLog::<AccountDevice>::open(path, password, MAGIC)?;
        let mut by_user = HashMap::new();
        for entry in entries {
            Self::validate(&entry, own_account_id)?;
            let id = entry.public.user_id();
            if let Some(existing) = by_user.insert(id, entry.clone())
                && existing != entry
            {
                return Err(LogError::CorruptFile(
                    "conflicting linked-device certificate".into(),
                ));
            }
            if by_user.len() > MAX_LINKED_DEVICES {
                return Err(LogError::CorruptFile("too many linked devices".into()));
            }
        }
        Ok(Self {
            file,
            by_user,
            own_account_id: own_account_id.to_string(),
        })
    }

    fn validate(device: &AccountDevice, own_account_id: &str) -> Result<(), LogError> {
        if !device.certificate.verify()
            || device.certificate.device_ed25519_pub != device.public.ed25519_pub
            || device.certificate.account_id() != own_account_id
        {
            return Err(LogError::CorruptFile(
                "invalid linked-device certificate".into(),
            ));
        }
        Ok(())
    }

    /// Persist a newly certified device; return whether it was added.
    pub fn insert(&mut self, device: AccountDevice) -> Result<bool, LogError> {
        Self::validate(&device, &self.own_account_id)?;
        let id = device.public.user_id();
        if let Some(existing) = self.by_user.get(&id) {
            if existing != &device {
                return Err(LogError::CorruptFile(
                    "conflicting linked-device certificate".into(),
                ));
            }
            return Ok(false);
        }
        if self.by_user.len() >= MAX_LINKED_DEVICES {
            return Err(LogError::CorruptFile("too many linked devices".into()));
        }
        self.file.append(&device)?;
        self.by_user.insert(id, device);
        Ok(true)
    }

    /// Query the immutable binding already verified on open/insert. This is
    /// roster membership only; callers still own current synchronization policy.
    /// Check both public keys even though the lookup key uses the signing key.
    pub(crate) fn is_certified_device(
        &self,
        public: &PublicIdentity,
        expected_account: &str,
    ) -> bool {
        self.own_account_id == expected_account
            && self
                .by_user
                .get(&public.user_id())
                .is_some_and(|device| device.public == *public)
    }

    /// Return all linked devices in stable device-ID order.
    pub fn devices(&self) -> Vec<AccountDevice> {
        let mut devices: Vec<_> = self.by_user.values().cloned().collect();
        devices.sort_by_key(|device| device.public.user_id());
        devices
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;
    use crate::identity::device::DeviceIdentity;

    fn certified(account: &Account, identity: &DeviceIdentity) -> AccountDevice {
        AccountDevice {
            public: identity.public(),
            certificate: account.certify(&identity.public().ed25519_pub),
        }
    }

    #[test]
    fn sibling_queries_reuse_admitted_certificates_and_preserve_full_device_identity() {
        use crate::discovery::Announce;
        use crate::identity::account::certificate_observations;
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let sibling = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&sibling, &account, "Sibling", 1234);
        let (node, _, _) = super::super::test_support::node_with_files(
            dir.path(),
            DeviceIdentity::generate(),
            account,
            &proof,
        );
        certificate_observations::reset();
        assert!(
            node.account_devices
                .lock()
                .unwrap()
                .insert(certified(&node.account, &sibling))
                .unwrap()
        );
        assert!(
            certificate_observations::count() > 0,
            "admission must still verify the actual signature"
        );
        certificate_observations::reset();
        let mut exchanged = sibling.public();
        exchanged.x25519_pub[0] ^= 1;
        let stranger = DeviceIdentity::generate().public();
        for _ in 0..64 {
            assert!(node.certified_sibling(&sibling.public()));
            assert!(node.certified_sibling(&node.identity.public()));
            assert!(!node.certified_sibling(&exchanged));
            assert!(!node.certified_sibling(&stranger));
        }
        assert_eq!(
            certificate_observations::count(),
            0,
            "unchanged sibling queries must reuse the roster's validated binding instead of repeating cryptography"
        );
        let rejected = DeviceIdentity::generate();
        let mut forged = certified(&node.account, &rejected);
        forged.certificate.signature[0] ^= 1;
        assert!(node.account_devices.lock().unwrap().insert(forged).is_err());
        assert!(certificate_observations::count() > 0);
        assert!(!node.certified_sibling(&rejected.public()));
    }

    #[test]
    fn linked_devices_survive_reopen_without_duplicates() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("account-devices.log");
        let account = Account::generate();
        let identity = DeviceIdentity::generate();
        let device = certified(&account, &identity);
        {
            let mut devices = AccountDevices::open(&path, "pw", &account.account_id()).unwrap();
            assert!(devices.insert(device.clone()).unwrap());
            assert!(!devices.insert(device.clone()).unwrap());
        }
        crate::identity::account::certificate_observations::reset();
        let devices = AccountDevices::open(&path, "pw", &account.account_id()).unwrap();
        assert!(crate::identity::account::certificate_observations::count() > 0);
        crate::identity::account::certificate_observations::reset();
        for _ in 0..64 {
            assert!(devices.is_certified_device(&identity.public(), &account.account_id()));
            assert!(
                !devices.is_certified_device(&identity.public(), &Account::generate().account_id())
            );
        }
        assert_eq!(
            crate::identity::account::certificate_observations::count(),
            0
        );
        assert_eq!(devices.devices(), vec![device]);
    }

    #[test]
    fn rejects_invalid_or_other_account_certificates() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let other_account = Account::generate();
        let identity = DeviceIdentity::generate();
        let mut devices = AccountDevices::open(
            &dir.path().join("account-devices.log"),
            "pw",
            &account.account_id(),
        )
        .unwrap();
        assert!(
            devices
                .insert(certified(&other_account, &identity))
                .is_err()
        );
        let mut forged = certified(&account, &identity);
        forged.certificate.signature[0] ^= 1;
        assert!(devices.insert(forged.clone()).is_err());
        assert!(devices.devices().is_empty());
        // The encrypted file is not trusted merely because it decrypts. A
        // forged record must still fail validation on a fresh roster owner.
        let poisoned = dir.path().join("poisoned.log");
        let (mut raw, _) =
            EncryptedRecordLog::<AccountDevice>::open(&poisoned, "pw", MAGIC).unwrap();
        raw.append(&forged).unwrap();
        drop(raw);
        assert!(AccountDevices::open(&poisoned, "pw", &account.account_id()).is_err());
    }
}
