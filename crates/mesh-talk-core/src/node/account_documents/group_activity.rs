//! Bounded optional history observations. These never install public group state.
use super::*;
use crate::{
    eventlog::ConversationId,
    node::{ChannelActivity, account_devices::AccountDevice},
};

const MAX_ENTRIES: usize = 20;
const SIGNING_DOMAIN: &[u8] = b"mesh-talk-private-group-activity-v1";

#[derive(Clone, Debug, Serialize, Deserialize)]
struct ActivityEntry {
    kind: String,
    actor: String,
    target: Option<String>,
    wall_clock: u64,
}

impl ActivityEntry {
    fn kind(&self) -> Option<&'static str> {
        match self.kind.as_str() {
            "created" => Some("created"),
            "joined" => Some("joined"),
            "removed" => Some("removed"),
            "renamed" => Some("renamed"),
            "transferred" => Some("transferred"),
            "left" => Some("left"),
            "accepted" => Some("accepted"),
            _ => None,
        }
    }

    fn valid(&self) -> bool {
        let target_matches_kind = match self.kind.as_str() {
            "created" | "renamed" => self.target.is_none(),
            "joined" | "removed" | "transferred" => self.target.is_some(),
            "left" | "accepted" => self.target.as_ref() == Some(&self.actor),
            _ => false,
        };
        target_matches_kind
            && hex(&self.actor, 32)
            && self.target.as_ref().is_none_or(|id| hex(id, 32))
            && self.wall_clock <= 8_640_000_000_000_000
    }

    fn into_activity(self) -> Option<ChannelActivity> {
        Some(ChannelActivity {
            kind: self.kind()?,
            actor: self.actor,
            target: self.target,
            wall_clock: self.wall_clock,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Observation {
    channel: String,
    source_device: String,
    inventory_hash: String,
    observer: AccountDevice,
    entries: Vec<ActivityEntry>,
    signature: String,
}

impl Observation {
    fn signing_message(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        let mut message = SIGNING_DOMAIN.to_vec();
        message.extend(serde_json::to_vec(&unsigned).expect("activity serializes"));
        message
    }
}

pub(super) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    let AccountDocumentKey::GroupActivity {
        channel,
        source_device,
    } = &meta.key
    else {
        return false;
    };
    if source_device != &meta.writer_device {
        return false;
    }
    if meta.tombstone {
        return payload.is_empty();
    }
    let Ok(value) = serde_json::from_slice::<Observation>(payload) else {
        return false;
    };
    value.channel == *channel
        && value.source_device == *source_device
        && hex(&value.inventory_hash, 64)
        && value.entries.len() <= MAX_ENTRIES
        && value.entries.iter().all(ActivityEntry::valid)
        && value.observer.public.user_id() == *source_device
        && value.observer.certificate.verify()
        && value.observer.certificate.device_ed25519_pub == value.observer.public.ed25519_pub
        && value.observer.certificate.account_ed25519_pub == meta.account_public
        && hex::decode(&value.signature)
            .ok()
            .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
            .is_some_and(|signature| {
                DeviceIdentity::verify(
                    &value.observer.public.ed25519_pub,
                    &value.signing_message(),
                    &signature,
                )
            })
}

impl Node {
    /// Publish after the inventory owner has committed its real local snapshot.
    /// References received from siblings are never activity publication sources.
    pub(in crate::node) fn publish_group_activity_for(&self, group: ConversationId) {
        if !self.device_sync_policy().enabled(SyncCategory::History) {
            return;
        }
        let entries = {
            let book = self.channels.lock().expect("channels mutex");
            let Some(state) = book.state(&group) else {
                return;
            };
            if !self.channel_identity_matches_account(state) {
                return;
            }
            book.activity(&group)
                .into_iter()
                .rev()
                .take(MAX_ENTRIES)
                .map(|entry| ActivityEntry {
                    kind: entry.kind.into(),
                    actor: entry.actor,
                    target: entry.target,
                    wall_clock: entry.wall_clock,
                })
                .collect::<Vec<_>>()
        };
        let channel = hex::encode(group.as_bytes());
        let source_device = self.user_id();
        let inventory_key = AccountDocumentKey::GroupInventory {
            channel: channel.clone(),
            source_device: source_device.clone(),
        };
        let key = AccountDocumentKey::GroupActivity {
            channel: channel.clone(),
            source_device: source_device.clone(),
        };
        let changed = {
            let mut store = self.account_documents.lock().expect("documents mutex");
            let Ok(Some(inventory)) = store.get(&inventory_key) else {
                return;
            };
            if !super::inventory::source_authenticated(inventory.payload()) {
                return;
            }
            let mut value = Observation {
                channel,
                source_device: source_device.clone(),
                inventory_hash: hex::encode(inventory.meta.content_hash),
                observer: AccountDevice {
                    public: self.identity.public(),
                    certificate: self.account.certify(&self.identity.public().ed25519_pub),
                },
                entries: entries.into_iter().rev().collect(),
                signature: String::new(),
            };
            value.signature = hex::encode(self.identity.sign(&value.signing_message()));
            let payload = serde_json::to_vec(&value).expect("activity serializes");
            let previous = store.meta(&key);
            let meta = if let Some(old) = previous
                .as_ref()
                .filter(|meta| !meta.tombstone && meta.content_hash == content_hash(&payload))
            {
                old.clone()
            } else {
                AccountDocumentMeta::new(
                    &self.account,
                    source_device,
                    key,
                    previous
                        .map_or(0, |meta| meta.revision)
                        .saturating_add(1)
                        .max(super::super::node::now_millis()),
                    &payload,
                    false,
                )
            };
            store.merge(meta, &payload)
        };
        match changed {
            Ok(true) => self.documents_changed(),
            Ok(false) => {}
            Err(error) => log::debug!("group activity publication deferred: {error}"),
        }
    }

    /// Optional history from precisely the source selected for descriptive facts.
    pub(in crate::node) fn shared_group_activity(
        &self,
        group: ConversationId,
    ) -> Vec<ChannelActivity> {
        if !self.device_sync_policy().enabled(SyncCategory::History) {
            return Vec::new();
        }
        let Some(reference) = self.channel_shared_reference(group) else {
            return Vec::new();
        };
        let store = self.account_documents.lock().expect("documents mutex");
        let inventory_key = AccountDocumentKey::GroupInventory {
            channel: reference.channel_id.clone(),
            source_device: reference.source_device.clone(),
        };
        let key = AccountDocumentKey::GroupActivity {
            channel: reference.channel_id,
            source_device: reference.source_device,
        };
        let Ok(Some(document)) = store.get(&key) else {
            return Vec::new();
        };
        if !valid_payload(&document.meta, document.payload()) {
            return Vec::new();
        }
        let Ok(value) = serde_json::from_slice::<Observation>(document.payload()) else {
            return Vec::new();
        };
        if store.meta(&inventory_key).is_none_or(|meta| {
            meta.tombstone || value.inventory_hash != hex::encode(meta.content_hash)
        }) {
            return Vec::new();
        }
        value
            .entries
            .into_iter()
            .filter_map(ActivityEntry::into_activity)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        eventlog::{Event, EventKind},
    };
    use std::sync::Arc;

    fn fixture() -> (
        tempfile::TempDir,
        Account,
        Arc<Node>,
        Arc<Node>,
        ConversationId,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = super::super::tests::node(&directory.path().join("source"), &account);
        let target = super::super::tests::node(&directory.path().join("target"), &account);
        let group = new_channel_id();
        let initial = Event::new(
            &source.identity,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Observed group".into(),
                members: vec![source.identity.public()],
                epoch: 0,
                owner: source.user_id(),
            }
            .encode(),
        );
        source.log.lock().unwrap().append(initial).unwrap();
        source.process_channel(group);
        source.publish_group_inventory_for(group);
        source.publish_group_activity_for(group);
        (directory, account, source, target, group)
    }

    fn key(source: &Node, group: ConversationId) -> AccountDocumentKey {
        AccountDocumentKey::GroupActivity {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        }
    }

    fn copy_document(source: &Node, target: &Node, key: &AccountDocumentKey) {
        let document = source.account_document(key).unwrap().unwrap();
        target
            .account_documents
            .lock()
            .unwrap()
            .merge(document.meta.clone(), document.payload())
            .unwrap();
    }

    fn copy_inventory(source: &Node, target: &Node, group: ConversationId) {
        copy_document(
            source,
            target,
            &AccountDocumentKey::GroupInventory {
                channel: hex::encode(group.as_bytes()),
                source_device: source.user_id(),
            },
        );
    }

    #[test]
    fn group_activity_reference_projection_preserves_physical_authority_and_optional_history() {
        let (_directory, _account, source, target, group) = fixture();
        copy_inventory(&source, &target, group);
        assert!(
            target.channel_activity(group).is_empty(),
            "legacy absence stays empty"
        );
        copy_document(&source, &target, &key(&source, group));
        assert_eq!(
            target.channel_activity(group),
            source.channel_activity(group)
        );
        assert_eq!(target.channel_activity(group)[0].kind, "created");
        assert!(target.channel_members(group).is_empty());
        assert!(
            target
                .log
                .lock()
                .unwrap()
                .events(&group)
                .unwrap()
                .is_empty()
        );
        assert!(!target.channel_can_send(group).unwrap());
        assert!(!target.channel_can_manage(group).unwrap());
        target.publish_group_activity_for(group);
        assert!(
            target
                .account_document(&key(&target, group))
                .unwrap()
                .is_none()
        );
        target
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        assert!(target.channel_activity(group).is_empty());
        target
            .set_device_sync_category(SyncCategory::History, true)
            .unwrap();
        assert_eq!(
            target.channel_activity(group),
            source.channel_activity(group)
        );
    }

    #[test]
    fn group_activity_rejects_tampered_source_key_rows_and_bounds() {
        let (_directory, account, source, target, group) = fixture();
        let document = source
            .account_document(&key(&source, group))
            .unwrap()
            .unwrap();
        let original: Observation = serde_json::from_slice(document.payload()).unwrap();
        for mutation in 0..9 {
            let mut value = original.clone();
            let mut document_key = document.meta.key.clone();
            let mut writer = source.user_id();
            match mutation {
                0 => value.entries[0].actor = target.user_id(),
                1 => {
                    value.entries = vec![value.entries[0].clone(); MAX_ENTRIES + 1];
                }
                2 => value.entries[0].kind = "message".into(),
                3 => value.entries[0].wall_clock = u64::MAX,
                4 => document_key = key(&target, group),
                5 => writer = target.user_id(),
                6 => value.entries[0].target = Some(target.user_id()),
                7 => {
                    value.entries[0].kind = "left".into();
                    value.entries[0].target = Some(target.user_id());
                }
                _ => {
                    value.entries[0].kind = "joined".into();
                    value.entries[0].target = None;
                }
            }
            // Even a sibling's valid account signature cannot impersonate the
            // source device or authorize unsupported display rows.
            if mutation >= 6 {
                // These invalid shapes carry a real source signature, proving
                // schema validation rejects them independently of authentication.
                value.signature = hex::encode(source.identity.sign(&value.signing_message()));
            }
            let payload = serde_json::to_vec(&value).unwrap();
            let meta = AccountDocumentMeta::new(
                &account,
                writer,
                document_key,
                document.meta.revision + 1,
                &payload,
                false,
            );
            assert!(!valid_payload(&meta, &payload));
            assert!(
                target
                    .account_documents
                    .lock()
                    .unwrap()
                    .merge(meta, &payload)
                    .is_err()
            );
        }
    }

    #[test]
    fn group_activity_inventory_changes_hide_stale_observations_and_publication_is_bounded() {
        let (_directory, _account, source, target, group) = fixture();
        copy_inventory(&source, &target, group);
        copy_document(&source, &target, &key(&source, group));
        let old = source
            .account_document(&key(&source, group))
            .unwrap()
            .unwrap();
        for index in 0..25 {
            source
                .append_event(
                    group,
                    EventKind::ChannelRename,
                    format!("Renamed {index}").into_bytes(),
                )
                .unwrap();
            source.process_channel(group);
        }
        source.publish_group_inventory_for(group);
        copy_inventory(&source, &target, group);
        assert!(target.channel_activity(group).is_empty());
        source.publish_group_activity_for(group);
        copy_document(&source, &target, &key(&source, group));
        let current = source
            .account_document(&key(&source, group))
            .unwrap()
            .unwrap();
        assert_ne!(current.meta.content_hash, old.meta.content_hash);
        assert!(current.payload().len() <= 8192);
        assert_eq!(target.channel_activity(group).len(), MAX_ENTRIES);
        assert!(
            target
                .channel_activity(group)
                .iter()
                .all(|entry| entry.kind == "renamed")
        );
        source.publish_group_activity_for(group);
        assert_eq!(
            source.account_document_meta(&key(&source, group)),
            Some(current.meta.clone())
        );
    }

    #[test]
    fn group_activity_physical_state_wins_over_borrowed_rows() {
        let (_directory, _account, source, target, group) = fixture();
        copy_inventory(&source, &target, group);
        copy_document(&source, &target, &key(&source, group));
        assert!(!target.channel_activity(group).is_empty());
        // Install a real local view through the ordinary public reducer.
        let event = Event::new(
            &target.identity,
            group,
            1,
            vec![],
            1,
            2,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Local actual group".into(),
                members: vec![target.identity.public()],
                epoch: 0,
                owner: target.user_id(),
            }
            .encode(),
        );
        target.log.lock().unwrap().append(event).unwrap();
        target.process_channel(group);
        assert!(target.channel_shared_reference(group).is_none());
        assert_eq!(target.channel_activity(group)[0].actor, target.user_id());
        assert_ne!(
            target.channel_activity(group),
            source.channel_activity(group)
        );
    }
    #[test]
    fn group_activity_uses_only_the_selected_inventory_source() {
        let (directory, account, source, target, group) = fixture();
        let second = super::super::tests::node(&directory.path().join("second"), &account);
        source
            .append_event(
                group,
                EventKind::MembershipChange,
                ChannelMeta {
                    name: "Observed group".into(),
                    members: vec![source.identity.public(), second.identity.public()],
                    epoch: 1,
                    owner: source.user_id(),
                }
                .encode(),
            )
            .unwrap();
        source.process_channel(group);
        let events: Vec<_> = source
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .into_iter()
            .collect();
        for event in events {
            second.log.lock().unwrap().append(event).unwrap();
        }
        second.process_channel(group);
        for node in [&source, &second] {
            node.publish_group_inventory_for(group);
            node.publish_group_activity_for(group);
            copy_inventory(node, &target, group);
        }
        let reference = target.channel_shared_reference(group).unwrap();
        let (selected, other) = if reference.source_device == source.user_id() {
            (&source, &second)
        } else {
            (&second, &source)
        };
        copy_document(other, &target, &key(other, group));
        assert!(target.channel_activity(group).is_empty());
        copy_document(selected, &target, &key(selected, group));
        assert_eq!(
            target.channel_activity(group),
            selected.channel_activity(group)
        );
    }

    #[test]
    fn group_activity_does_not_publish_when_history_is_disabled() {
        let (_directory, _account, source, _target, group) = fixture();
        let before = source.account_document_meta(&key(&source, group)).unwrap();
        source
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        source
            .append_event(
                group,
                EventKind::ChannelRename,
                b"Private new observation".to_vec(),
            )
            .unwrap();
        source.process_channel(group);
        source.publish_group_inventory_for(group);
        source.publish_group_activity_for(group);
        assert_eq!(
            source.account_document_meta(&key(&source, group)),
            Some(before)
        );
    }
}
