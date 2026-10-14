use crate::eventlog::LogError;
use crate::identity::account::DeviceCertificate;
use crate::identity::device::PublicIdentity;
use crate::storage::record_log::EncryptedRecordLog;
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTACDV";
const MAX_LINKED_DEVICES: usize = 64;

pub(crate) fn file_path(dir: &Path, account_id: &str) -> std::path::PathBuf {
    dir.join(format!("account-devices-{account_id}.log"))
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AccountDevice {
    pub public: PublicIdentity,
    pub certificate: DeviceCertificate,
}

pub struct AccountDevices {
    file: EncryptedRecordLog<AccountDevice>,
    by_user: HashMap<String, AccountDevice>,
    own_account_id: String,
}

impl AccountDevices {
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
        let devices = AccountDevices::open(&path, "pw", &account.account_id()).unwrap();
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
        assert!(devices.insert(forged).is_err());
        assert!(devices.devices().is_empty());
    }
}
