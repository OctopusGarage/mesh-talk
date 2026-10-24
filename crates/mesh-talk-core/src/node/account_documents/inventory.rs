//! Account-private descriptive group inventory. This module never installs grants,
//! membership events, sender keys, or peer/account authorization bindings.
use super::*;
use crate::{
    eventlog::ConversationId, identity::device::PublicIdentity,
    node::account_devices::AccountDevice,
};
use std::collections::{BTreeMap, BTreeSet};
const MAX_MEMBERS: usize = 256;
const MAX_GROUP_NAME_BYTES: usize = 1024;
const PUBLICATION_SLICE: usize = 16;

/// Display facts only; an account label here is never an authorization proof.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupInventoryMember {
    pub public: PublicIdentity,
    pub account_id: Option<String>,
    pub name: Option<String>,
    pub pending: bool,
    pub post_office: bool,
}
/// A certified sibling's view of a group, without membership or key authority.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GroupInventoryReference {
    pub channel_id: String,
    pub source_device: String,
    pub name: String,
    pub member_count: usize,
    pub owner: String,
    pub owner_account_id: Option<String>,
    /// Status of the source device observation, never global account authority.
    pub dissolved: bool,
    pub left: bool,
    pub pending_invite: bool,
    pub legacy_membership: bool,
    pub members: Vec<GroupInventoryMember>,
    pub members_complete: bool,
    observer: AccountDevice,
}

/// Inventory labels are a bounded display projection, never a rename or a
/// replacement for the group's authoritative stored name.
fn inventory_display_name(name: &str, channel: ConversationId) -> String {
    let mut display = String::with_capacity(name.len().min(MAX_GROUP_NAME_BYTES));
    let mut truncated = false;
    for character in name.chars() {
        let character = if character.is_control() {
            ' '
        } else {
            character
        };
        if display.len() + character.len_utf8() > MAX_GROUP_NAME_BYTES {
            truncated = true;
            break;
        }
        display.push(character);
    }
    if display.trim().is_empty() {
        return format!("Group {}", &hex::encode(channel.as_bytes())[..12]);
    }
    if truncated {
        while display.len() + '…'.len_utf8() > MAX_GROUP_NAME_BYTES {
            display.pop();
        }
        display.push('…');
    }
    display
}

impl GroupInventoryReference {
    /// Member facts are already ordered with owner/observer first. Budget each
    /// serialized fact once so large canonical names cannot hide the whole group.
    fn bounded_payload(&mut self, limit: usize) -> Option<Vec<u8>> {
        let mut members = std::mem::take(&mut self.members);
        let was_complete = self.members_complete;
        // `false` is one JSON byte longer than `true`, making the envelope budget
        // conservative when the complete roster does fit after all.
        self.members_complete = false;
        let mut bytes = serde_json::to_vec(self).ok()?.len();
        if bytes > limit {
            return None;
        }
        let mut retained = 0;
        for member in &members {
            let length = serde_json::to_vec(member).ok()?.len();
            let additional = length + usize::from(retained > 0);
            if additional > limit - bytes {
                break;
            }
            bytes += additional;
            retained += 1;
        }
        self.members_complete = was_complete && retained == members.len();
        members.truncate(retained);
        self.members = members;
        let payload = serde_json::to_vec(self).ok()?;
        (payload.len() <= limit).then_some(payload)
    }
}

pub(super) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    let AccountDocumentKey::GroupInventory {
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
    let Ok(value) = serde_json::from_slice::<GroupInventoryReference>(payload) else {
        return false;
    };
    let mut members = BTreeSet::new();
    let principals = value
        .members
        .iter()
        .filter(|member| !member.pending && (!value.legacy_membership || !member.post_office))
        .map(|member| {
            member
                .account_id
                .clone()
                .unwrap_or_else(|| member.public.user_id())
        })
        .collect::<BTreeSet<_>>();
    value.channel_id == *channel
        && value.source_device == *source_device
        && value.observer.public.user_id() == *source_device
        && value.observer.certificate.verify()
        && value.observer.certificate.device_ed25519_pub == value.observer.public.ed25519_pub
        && value.observer.certificate.account_ed25519_pub == meta.account_public
        && !value.name.is_empty()
        && value.name.len() <= MAX_GROUP_NAME_BYTES
        && !value.name.chars().any(char::is_control)
        && hex(&value.owner, 32)
        && value.owner_account_id.as_ref().is_none_or(|id| hex(id, 32))
        && value.legacy_membership == value.owner_account_id.is_none()
        && value.owner_account_id.as_ref().is_none_or(|owner_account| {
            value.members.iter().any(|member| {
                member.public.user_id() == value.owner
                    && member.account_id.as_ref() == Some(owner_account)
            })
        })
        && value.members.len() <= MAX_MEMBERS
        && value.member_count <= 4096
        && value.member_count >= principals.len()
        && (!value.members_complete || value.member_count == principals.len())
        && value.members.iter().all(|member| {
            members.insert(member.public.user_id())
                && member.account_id.as_ref().is_none_or(|id| hex(id, 32))
                && member.name.as_ref().is_none_or(|name| {
                    !name.is_empty() && name.len() <= 512 && !name.chars().any(char::is_control)
                })
        })
        && (value.left
            || value
                .members
                .iter()
                .any(|member| member.public == value.observer.public))
        && (value.dissolved
            || value
                .members
                .iter()
                .any(|member| member.public.user_id() == value.owner))
}

fn references_from_rows(
    store: &store::AccountDocumentStore,
    rows: Vec<AccountDocumentMeta>,
) -> BTreeMap<String, GroupInventoryReference> {
    let mut selected: BTreeMap<String, (AccountDocumentMeta, GroupInventoryReference)> =
        BTreeMap::new();
    for meta in rows.into_iter().filter(|meta| !meta.tombstone) {
        let Ok(Some(document)) = store.get(&meta.key) else {
            continue;
        };
        if !valid_payload(&meta, document.payload()) {
            continue;
        }
        let Ok(reference) = serde_json::from_slice::<GroupInventoryReference>(document.payload())
        else {
            continue;
        };
        if selected
            .get(&reference.channel_id)
            .is_none_or(|(old, _)| old.order() < meta.order())
        {
            selected.insert(reference.channel_id.clone(), (meta, reference));
        }
    }
    selected
        .into_iter()
        .map(|(id, (_, reference))| (id, reference))
        .collect()
}

impl Node {
    /// Returns display-only inventory only when this device has no real local group.
    pub fn channel_shared_reference(
        &self,
        channel: ConversationId,
    ) -> Option<GroupInventoryReference> {
        if self
            .channels
            .lock()
            .expect("channels mutex")
            .state(&channel)
            .is_some()
        {
            return None;
        }
        let channel = hex::encode(channel.as_bytes());
        let store = self.account_documents.lock().expect("documents mutex");
        references_from_rows(&store, store.inventory_channel_rows(&channel)).remove(&channel)
    }
    pub fn channel_legacy_membership(&self, channel: ConversationId) -> bool {
        let book = self.channels.lock().expect("channels mutex");
        if let Some(state) = book.state(&channel) {
            return state.owner_account_id().is_none();
        }
        drop(book);
        self.channel_shared_reference(channel)
            .is_some_and(|reference| reference.legacy_membership)
    }
    pub(in crate::node) fn group_inventory_references(
        &self,
    ) -> BTreeMap<String, GroupInventoryReference> {
        let store = self.account_documents.lock().expect("documents mutex");
        references_from_rows(&store, store.inventory_rows())
    }

    /// Rebuild a bounded slice from actual local state, never from borrowed rows.
    pub(in crate::node) fn publish_group_inventory(&self, after: &mut Option<ConversationId>) {
        let own = self.user_id();
        let prior: BTreeSet<_> = self
            .account_documents
            .lock()
            .expect("documents mutex")
            .inventory_rows()
            .into_iter()
            .filter_map(|meta| match meta.key {
                AccountDocumentKey::GroupInventory {
                    channel,
                    source_device,
                } if source_device == own => Some(channel),
                _ => None,
            })
            .collect();
        let book = self.channels.lock().expect("channels mutex");
        let mut ids = book.channel_ids();
        ids.sort_by_key(|id| *id.as_bytes());
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| after.is_none_or(|previous| id.as_bytes() > previous.as_bytes()))
            .take(PUBLICATION_SLICE)
            .collect();
        let mut snapshots = Vec::new();
        for id in &ids {
            *after = Some(*id);
            let Some(state) = book.state(id) else {
                continue;
            };
            let channel_id = hex::encode(id.as_bytes());
            let eligible =
                self.channel_identity_matches_account(state) && state.is_accepted_member(&own);
            if !eligible && !prior.contains(&channel_id) {
                continue;
            }
            let mut members = state.members().to_vec();
            members.sort_by_key(|member| {
                (
                    member.user_id() != state.owner() && member.user_id() != own,
                    member.user_id(),
                )
            });
            let members = members
                .into_iter()
                .take(MAX_MEMBERS)
                .map(|public| {
                    let account_id = if state.owner_account_id().is_some() {
                        state
                            .member_account_id(&public.user_id())
                            .map(str::to_owned)
                    } else {
                        self.account_id_for_device(&public)
                    };
                    let name = account_id
                        .as_ref()
                        .and_then(|account| self.account_profile(account))
                        .map(|profile| profile.display_name)
                        .or_else(|| {
                            self.historical_author(&public.ed25519_pub)
                                .filter(|proof| {
                                    proof.public() == public
                                        && proof.verify()
                                        && (account_id.is_none()
                                            || proof.account_id() == account_id)
                                })
                                .map(|proof| proof.name)
                        })
                        .filter(|name| {
                            !name.is_empty()
                                && name.len() <= 512
                                && !name.chars().any(char::is_control)
                        });
                    GroupInventoryMember {
                        post_office: self.device_is_post_office(&public),
                        account_id,
                        name,
                        pending: !state.is_accepted_member(&public.user_id()),
                        public,
                    }
                })
                .collect();
            snapshots.push(GroupInventoryReference {
                channel_id,
                source_device: own.clone(),
                name: inventory_display_name(state.name(), *id),
                member_count: state.members().len(),
                owner: state.owner().into(),
                owner_account_id: state.owner_account_id().map(str::to_owned),
                dissolved: state.is_dissolved(),
                left: !self.channel_identity_matches_account(state),
                pending_invite: state.pending_invite(),
                legacy_membership: state.owner_account_id().is_none(),
                members,
                members_complete: state.members().len() <= MAX_MEMBERS,
                observer: AccountDevice {
                    public: self.identity.public(),
                    certificate: self.account.certify(&self.identity.public().ed25519_pub),
                },
            });
        }
        drop(book);
        if ids.len() < PUBLICATION_SLICE {
            *after = None;
        }
        // Absence in a restored/partial local book is not evidence of departure.
        // Keep the last durable source observation until a validated state changes it.
        for mut reference in snapshots {
            let id = hex::decode(&reference.channel_id)
                .ok()
                .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
                .map(ConversationId::new)
                .expect("locally encoded channel id");
            let (accounts, unbound) = self.channel_member_account_sets(id);
            reference.member_count = accounts.len() + unbound.len();
            let key = AccountDocumentKey::GroupInventory {
                channel: reference.channel_id.clone(),
                source_device: own.clone(),
            };
            let Some(payload) = reference.bounded_payload(key.limit()) else {
                continue;
            };
            let changed = {
                let mut store = self.account_documents.lock().expect("documents mutex");
                let previous = store.meta(&key);
                // Explicit source retractions are durable; periodic reconstruction
                // must not silently resurrect them.
                if previous.as_ref().is_some_and(|meta| meta.tombstone) {
                    continue;
                }
                let meta = if let Some(old) = previous
                    .as_ref()
                    .filter(|old| old.content_hash == content_hash(&payload))
                {
                    old.clone()
                } else {
                    let revision = previous
                        .map_or(0, |meta| meta.revision)
                        .saturating_add(1)
                        .max(super::super::node::now_millis());
                    AccountDocumentMeta::new(
                        &self.account,
                        own.clone(),
                        key,
                        revision,
                        &payload,
                        false,
                    )
                };
                store.merge(meta, &payload)
            };
            match changed {
                Ok(true) => self.documents_changed(),
                Ok(false) => {}
                Err(error) => log::debug!("group inventory publication deferred: {error}"),
            }
        }
    }
}
