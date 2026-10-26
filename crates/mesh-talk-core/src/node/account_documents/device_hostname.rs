//! Private device labels, never public discovery or account profile data.
use super::*;
use std::collections::BTreeMap;

pub(super) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    matches!(&meta.key, AccountDocumentKey::DeviceHostname { device_id } if device_id == &meta.writer_device)
        && if meta.tombstone {
            payload.is_empty()
        } else {
            std::str::from_utf8(payload).is_ok_and(valid_hostname)
        }
}
fn valid_hostname(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 255 && !value.chars().any(char::is_control)
}
impl Node {
    /// The host supplies its OS hostname. Publish only to certified account siblings.
    /// Passing None retracts a previously published name; no public profile changes.
    pub fn set_device_hostname(&self, hostname: Option<&str>) -> Result<(), NodeError> {
        if hostname.is_some_and(|value| !valid_hostname(value)) {
            return Err(invalid());
        }
        let payload = hostname.unwrap_or_default().as_bytes();
        let key = AccountDocumentKey::DeviceHostname {
            device_id: self.user_id(),
        };
        let changed = {
            let mut store = self.account_documents.lock().expect("documents mutex");
            let old = store.meta(&key);
            if old.is_none() && hostname.is_none() {
                return Ok(());
            }
            let removed = hostname.is_none();
            let hash = if removed {
                [0; 32]
            } else {
                content_hash(payload)
            };
            let meta = match old {
                Some(meta) if meta.tombstone == removed && meta.content_hash == hash => meta,
                old => AccountDocumentMeta::new(
                    &self.account,
                    self.user_id(),
                    key,
                    old.map_or(0, |meta| meta.revision)
                        .saturating_add(1)
                        .max(super::super::node::now_millis()),
                    payload,
                    removed,
                ),
            };
            store.merge(meta, payload).map_err(NodeError::Log)?
        };
        if changed {
            self.documents_changed();
        }
        Ok(())
    }

    /// Limit display to the local device and the persistent certified roster.
    pub fn device_hostnames(&self) -> Result<BTreeMap<String, String>, NodeError> {
        let mut devices = self
            .linked_devices()
            .into_iter()
            .map(|d| d.public.user_id())
            .collect::<Vec<_>>();
        devices.push(self.user_id());
        let store = self.account_documents.lock().expect("documents mutex");
        let mut names = BTreeMap::new();
        for device_id in devices {
            if let Some(document) = store
                .get(&AccountDocumentKey::DeviceHostname {
                    device_id: device_id.clone(),
                })
                .map_err(NodeError::Log)?
            {
                let name = std::str::from_utf8(document.payload()).map_err(|_| invalid())?;
                names.insert(device_id, name.to_owned());
            }
        }
        Ok(names)
    }
}
