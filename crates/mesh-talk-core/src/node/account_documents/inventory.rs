//! Account-private descriptive group inventory. This module never installs grants,
//! membership events, sender keys, or peer/account authorization bindings.
use super::*;
use crate::{
    discovery::Announce,
    eventlog::{ConversationId, Event, EventId, EventKind},
    identity::device::{DeviceIdentity, PublicIdentity},
    node::account_devices::AccountDevice,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashSet};
const MAX_MEMBERS: usize = 256;
const MAX_GROUP_NAME_BYTES: usize = 1024;
const PUBLICATION_SLICE: usize = 16;
const MAX_DIRECTORY_IDS: usize = 512;
const MAX_DIRECTORY_SOURCES: usize = 65;
const DIRECTORY_DOMAIN: &[u8] = b"mesh-talk-group-directory-v1";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct DirectoryAcknowledgement {
    source: String,
    revision: u64,
    hash: String,
}

/// A certified source's full-reducer observation. Identifiers describe the
/// evaluated public control set; they never claim to be a complete event DAG.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct GroupDirectoryObservation {
    version: u8,
    source: Announce,
    bindings: Vec<Announce>,
    certified: Vec<AccountDevice>,
    controls: Vec<String>,
    fingerprint: String,
    event_count: usize,
    complete: bool,
    frontier: Vec<String>,
    complete_frontier: bool,
    unavailable: bool,
    acknowledgements: Vec<DirectoryAcknowledgement>,
    history_policy: crate::channel::HistoryPolicy,
    signature: String,
}

fn directory_control(kind: EventKind) -> bool {
    super::super::account_group::wire::authorization_control(kind)
        || matches!(
            kind,
            EventKind::ChannelRename | EventKind::ChannelHistoryPolicy
        )
}

fn directory_fingerprint(controls: &[String]) -> String {
    let mut digest = Sha256::new();
    digest.update(DIRECTORY_DOMAIN);
    for control in controls {
        digest.update(control.as_bytes());
    }
    hex::encode(digest.finalize())
}

fn directory_view(events: &[&Event]) -> (Vec<String>, Vec<EventId>) {
    let relevant: HashSet<_> = events
        .iter()
        .filter(|event| directory_control(event.kind))
        .map(|event| event.id)
        .collect();
    let by_id: BTreeMap<_, _> = events.iter().map(|event| (event.id, *event)).collect();
    let mut ancestors = HashSet::new();
    let mut visited = HashSet::new();
    let mut pending: Vec<_> = events
        .iter()
        .filter(|event| relevant.contains(&event.id))
        .flat_map(|event| event.parents.iter().copied())
        .collect();
    while let Some(id) = pending.pop() {
        if relevant.contains(&id) {
            ancestors.insert(id);
        } else if visited.insert(id)
            && let Some(event) = by_id.get(&id)
        {
            pending.extend(event.parents.iter().copied());
        }
    }
    let mut controls: Vec<_> = relevant.iter().map(EventId::to_hex).collect();
    controls.sort();
    let mut frontier: Vec<_> = relevant.difference(&ancestors).copied().collect();
    frontier.sort();
    (controls, frontier)
}

pub(super) fn source_authenticated(payload: &[u8]) -> bool {
    serde_json::from_slice::<GroupInventoryReference>(payload)
        .is_ok_and(|reference| reference.directory.is_some())
}

pub(super) fn local_publication(
    payload: &[u8],
    account: &[u8; 32],
) -> Option<(PublicIdentity, ConversationId, bool)> {
    let reference: GroupInventoryReference = serde_json::from_slice(payload).ok()?;
    let directory = reference.directory.as_ref()?;
    if !reference.valid_directory()
        || !reference.observer.certificate.verify()
        || reference.observer.certificate.account_ed25519_pub != *account
        || reference.observer.certificate.device_ed25519_pub
            != reference.observer.public.ed25519_pub
        || reference.source_device != reference.observer.public.user_id()
    {
        return None;
    }
    let group = ConversationId::new(hex::decode(&reference.channel_id).ok()?.try_into().ok()?);
    Some((
        reference.observer.public,
        group,
        directory.complete_frontier,
    ))
}

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
    /// Absent on retained version-four descriptive observations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    directory: Option<GroupDirectoryObservation>,
    /// Computed by this reader. A retained coherent snapshot may still require
    /// reconciliation with another source; it does not establish current truth.
    #[serde(skip)]
    pub directory_reconciling: bool,
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
    fn directory_message(&self) -> Vec<u8> {
        let mut unsigned = self.clone();
        if let Some(directory) = &mut unsigned.directory {
            directory.signature.clear();
        }
        let mut message = DIRECTORY_DOMAIN.to_vec();
        message.extend(serde_json::to_vec(&unsigned).expect("directory serializes"));
        message
    }

    fn sign_directory(&mut self, identity: &DeviceIdentity) {
        let signature = hex::encode(identity.sign(&self.directory_message()));
        self.directory
            .as_mut()
            .expect("directory present")
            .signature = signature;
    }

    /// A descriptive policy observation, never permission to disclose history.
    pub fn observed_history_policy(&self) -> Option<crate::channel::HistoryPolicy> {
        self.directory
            .as_ref()
            .filter(|directory| !directory.unavailable)
            .map(|directory| directory.history_policy)
    }

    fn valid_directory(&self) -> bool {
        let Some(directory) = &self.directory else {
            return true;
        };
        directory.version == 1
            && directory.source.verify()
            && directory.source.public() == self.observer.public
            && directory
                .source
                .account_cert
                .as_ref()
                .is_some_and(|certificate| {
                    certificate.account_ed25519_pub == self.observer.certificate.account_ed25519_pub
                })
            && directory.controls.len() <= MAX_DIRECTORY_IDS
            && directory.controls.windows(2).all(|pair| pair[0] < pair[1])
            && directory.controls.iter().all(|id| hex(id, 64))
            && hex(&directory.fingerprint, 64)
            && directory.event_count > 0
            && directory.event_count >= directory.controls.len()
            && !directory.frontier.is_empty()
            && directory.frontier.len() <= MAX_DIRECTORY_IDS
            && directory.frontier.iter().all(|id| hex(id, 64))
            && directory.frontier.windows(2).all(|pair| pair[0] < pair[1])
            && (!directory.complete
                || directory.event_count == directory.controls.len()
                    && directory.fingerprint == directory_fingerprint(&directory.controls))
            && (!directory.unavailable
                || directory.controls.is_empty()
                    && directory.bindings.is_empty()
                    && directory.certified.is_empty()
                    && directory.acknowledgements.is_empty()
                    && !directory.complete)
            && directory.bindings.len() <= MAX_MEMBERS
            && directory
                .bindings
                .windows(2)
                .all(|pair| pair[0].user_id < pair[1].user_id)
            && directory.bindings.iter().all(|proof| {
                proof.verify()
                    && self
                        .members
                        .iter()
                        .any(|member| member.public == proof.public())
            })
            && directory.certified.len() <= MAX_DIRECTORY_SOURCES
            && directory
                .certified
                .windows(2)
                .all(|pair| pair[0].public.user_id() < pair[1].public.user_id())
            && directory.certified.iter().all(|binding| {
                binding.certificate.verify()
                    && binding.certificate.device_ed25519_pub == binding.public.ed25519_pub
                    && binding.certificate.account_ed25519_pub
                        == self.observer.certificate.account_ed25519_pub
                    && self
                        .members
                        .iter()
                        .any(|member| member.public == binding.public)
            })
            && directory.acknowledgements.len() <= MAX_DIRECTORY_SOURCES
            && directory
                .acknowledgements
                .windows(2)
                .all(|pair| pair[0].source < pair[1].source)
            && directory.acknowledgements.iter().all(|ack| {
                hex(&ack.source, 32)
                    && ack.source != self.source_device
                    && ack.revision > 0
                    && hex(&ack.hash, 64)
            })
            && hex::decode(&directory.signature)
                .ok()
                .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
                .is_some_and(|signature| {
                    DeviceIdentity::verify(
                        &self.observer.public.ed25519_pub,
                        &self.directory_message(),
                        &signature,
                    )
                })
    }

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
            self.members = members;
            self.members_complete = was_complete;
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
    value.valid_directory()
        && value.channel_id == *channel
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
    node: &Node,
    store: &store::AccountDocumentStore,
    rows: Vec<AccountDocumentMeta>,
) -> BTreeMap<String, GroupInventoryReference> {
    let mut grouped: BTreeMap<String, Vec<(AccountDocumentMeta, GroupInventoryReference)>> =
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
        if reference
            .directory
            .as_ref()
            .is_some_and(|directory| !node.account_group_binding_current(&directory.source))
        {
            continue;
        }
        grouped
            .entry(reference.channel_id.clone())
            .or_default()
            .push((meta, reference));
    }
    grouped
        .into_iter()
        .filter_map(|(id, rows)| {
            let has_directory = rows
                .iter()
                .any(|(_, reference)| reference.directory.is_some());
            let dominates =
                |new: &(AccountDocumentMeta, GroupInventoryReference),
                 old: &(AccountDocumentMeta, GroupInventoryReference)| {
                    let Some(new_directory) = &new.1.directory else {
                        return false;
                    };
                    let Some(old_directory) = &old.1.directory else {
                        return true;
                    };
                    new_directory.acknowledgements.iter().any(|ack| {
                        ack.source == old.1.source_device
                            && ack.revision == old.0.revision
                            && ack.hash == hex::encode(old.0.content_hash)
                    }) || !new_directory.unavailable
                        && old_directory.complete
                        && new_directory.controls.len() > old_directory.controls.len()
                        && old_directory
                            .controls
                            .iter()
                            .all(|id| new_directory.controls.binary_search(id).is_ok())
                };
            let maximal: Vec<_> = rows
                .iter()
                .filter(|row| {
                    (!has_directory || row.1.directory.is_some())
                        && !rows.iter().any(|other| dominates(other, row))
                })
                .collect();
            let reconciling = maximal.iter().any(|(_, reference)| {
                reference
                    .directory
                    .as_ref()
                    .is_some_and(|directory| directory.unavailable)
            }) || maximal.iter().any(|(_, left)| {
                maximal
                    .iter()
                    .any(|(_, right)| match (&left.directory, &right.directory) {
                        (Some(left_directory), Some(right_directory)) => {
                            left_directory.fingerprint != right_directory.fingerprint
                                || left_directory.event_count != right_directory.event_count
                                || left.name != right.name
                                || left.owner != right.owner
                                || left.owner_account_id != right.owner_account_id
                                || left.member_count != right.member_count
                                || left_directory.history_policy != right_directory.history_policy
                        }
                        _ => false,
                    })
            });
            // A conflicting or resource-limited source never certifies a stale
            // positive. Keep one coherent observed display and mark uncertainty.
            let selected = maximal
                .iter()
                .copied()
                .filter(|(_, reference)| {
                    !reference
                        .directory
                        .as_ref()
                        .is_some_and(|directory| directory.unavailable)
                })
                .max_by(|left, right| {
                    if has_directory {
                        left.1.source_device.cmp(&right.1.source_device)
                    } else {
                        left.0.order().cmp(&right.0.order())
                    }
                })
                .or_else(|| maximal.first().copied())?;
            let mut reference = selected.1.clone();
            reference.directory_reconciling = reconciling;
            node.project_inventory_bindings(&mut reference);
            Some((id, reference))
        })
        .collect()
}

impl Node {
    fn project_inventory_bindings(&self, reference: &mut GroupInventoryReference) {
        if !reference.legacy_membership {
            return;
        }
        let Some(directory) = &reference.directory else {
            return;
        };
        for member in &mut reference.members {
            // The local verified directory supersedes an older adopted binding.
            // A carried independently signed binding is only a display fallback.
            member.account_id = self
                .account_id_for_device(&member.public)
                .or_else(|| {
                    directory
                        .bindings
                        .iter()
                        .find(|proof| proof.public() == member.public)
                        .and_then(Announce::account_id)
                })
                .or_else(|| {
                    directory
                        .certified
                        .iter()
                        .any(|binding| binding.public == member.public)
                        .then(|| self.account_id())
                });
        }
        if reference.members_complete {
            reference.member_count = reference
                .members
                .iter()
                .filter(|member| !member.pending && !member.post_office)
                .map(|member| {
                    member
                        .account_id
                        .clone()
                        .unwrap_or_else(|| member.public.user_id())
                })
                .collect::<BTreeSet<_>>()
                .len();
        }
    }

    fn inventory_directory(
        &self,
        reference: &GroupInventoryReference,
        previous: Option<&GroupInventoryReference>,
        controls: &[String],
        frontier: &[EventId],
        history_policy: crate::channel::HistoryPolicy,
    ) -> GroupDirectoryObservation {
        let fingerprint = directory_fingerprint(controls);
        let mut bindings: Vec<_> = self
            .historical_peer_proofs()
            .into_iter()
            .chain(std::iter::once(self.own_group_proof()))
            .filter(|proof| {
                proof.verify()
                    && reference
                        .members
                        .iter()
                        .any(|member| member.public == proof.public())
            })
            .collect();
        bindings.sort_by(|left, right| left.user_id.cmp(&right.user_id));
        bindings.dedup_by(|left, right| left.user_id == right.user_id);
        bindings.truncate(MAX_MEMBERS);
        let mut certified = self
            .account_devices
            .lock()
            .expect("account devices mutex")
            .devices();
        certified.push(reference.observer.clone());
        certified.retain(|binding| {
            reference
                .members
                .iter()
                .any(|member| member.public == binding.public)
        });
        certified.sort_by_key(|binding| binding.public.user_id());
        certified.dedup_by(|left, right| left.public == right.public);
        // Retain exact acknowledgements of evaluated, incompatible heads. A
        // compatible source may have changed only its own acknowledgements;
        // acknowledging that version would make two unchanged sources publish
        // forever. This is the same retained floor used by GroupControl.
        let mut acknowledgements: BTreeMap<_, _> = previous
            .and_then(|reference| reference.directory.as_ref())
            .into_iter()
            .flat_map(|directory| &directory.acknowledgements)
            .map(|ack| (ack.source.clone(), ack.clone()))
            .collect();
        {
            let store = self.account_documents.lock().expect("documents mutex");
            for ack in store
                .inventory_channel_rows(&reference.channel_id)
                .into_iter()
                .filter(|meta| !meta.tombstone && meta.writer_device != self.user_id())
                .filter_map(|meta| {
                    let document = store.get(&meta.key).ok()??;
                    if !valid_payload(&meta, document.payload()) {
                        return None;
                    }
                    let other: GroupInventoryReference =
                        serde_json::from_slice(document.payload()).ok()?;
                    let directory = other.directory.as_ref()?;
                    if !self.account_group_binding_current(&directory.source)
                        || !(directory.fingerprint == fingerprint
                            && directory.event_count == controls.len()
                            || directory.complete_frontier
                                && directory
                                    .frontier
                                    .iter()
                                    .all(|id| controls.binary_search(id).is_ok()))
                    {
                        return None;
                    }
                    let compatible = !directory.unavailable
                        && directory.fingerprint == fingerprint
                        && directory.event_count == controls.len()
                        && other.name == reference.name
                        && other.owner == reference.owner
                        && other.owner_account_id == reference.owner_account_id
                        && other.member_count == reference.member_count
                        && directory.history_policy == history_policy;
                    if compatible {
                        return None;
                    }
                    Some(DirectoryAcknowledgement {
                        source: other.source_device,
                        revision: meta.revision,
                        hash: hex::encode(meta.content_hash),
                    })
                })
                .take(MAX_DIRECTORY_SOURCES)
            {
                acknowledgements.insert(ack.source.clone(), ack);
            }
        }
        let acknowledgements = acknowledgements
            .into_values()
            .take(MAX_DIRECTORY_SOURCES)
            .collect();
        let mut directory = GroupDirectoryObservation {
            version: 1,
            source: self.own_group_proof(),
            bindings,
            certified,
            event_count: controls.len(),
            complete: controls.len() <= MAX_DIRECTORY_IDS,
            unavailable: false,
            fingerprint,
            controls: controls.iter().take(MAX_DIRECTORY_IDS).cloned().collect(),
            frontier: frontier
                .iter()
                .take(MAX_DIRECTORY_IDS)
                .map(EventId::to_hex)
                .collect(),
            complete_frontier: frontier.len() <= MAX_DIRECTORY_IDS,
            acknowledgements,
            history_policy,
            signature: "0".repeat(128),
        };
        directory
            .acknowledgements
            .sort_by(|left, right| left.source.cmp(&right.source));
        directory
    }

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
        references_from_rows(self, &store, store.inventory_channel_rows(&channel)).remove(&channel)
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
        references_from_rows(self, &store, store.inventory_rows())
    }

    /// Publish only physical local groups, with canonical facts and their witness
    /// derived together from the existing complete public reducer.
    pub(in crate::node) fn publish_group_inventory(&self, after: &mut Option<ConversationId>) {
        let mut ids = self.channels.lock().expect("channels mutex").channel_ids();
        ids.sort_by_key(|id| *id.as_bytes());
        let ids: Vec<_> = ids
            .into_iter()
            .filter(|id| after.is_none_or(|previous| id.as_bytes() > previous.as_bytes()))
            .take(PUBLICATION_SLICE)
            .collect();
        self.publish_group_inventory_groups(&ids);
        *after = if ids.len() == PUBLICATION_SLICE {
            ids.last().copied()
        } else {
            None
        };
    }

    /// Canonical metadata for an accepted effect shares the normal signed owner.
    pub(in crate::node) fn publish_group_inventory_for(&self, group: ConversationId) {
        self.publish_group_inventory_groups(&[group]);
    }

    fn publish_group_inventory_groups(&self, ids: &[ConversationId]) {
        let own = self.user_id();
        for id in ids {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            let channel_id = hex::encode(id.as_bytes());
            let key = AccountDocumentKey::GroupInventory {
                channel: channel_id.clone(),
                source_device: own.clone(),
            };
            let (previous, old, receipt) = {
                let store = self.account_documents.lock().expect("documents mutex");
                let previous = store.meta(&key);
                let old = store.get(&key).ok().flatten().and_then(|document| {
                    serde_json::from_slice::<GroupInventoryReference>(document.payload()).ok()
                });
                let receipt = previous.as_ref().and_then(|meta| {
                    store.local_publication_frontier(meta, &self.identity.public())
                });
                (previous, old, receipt)
            };
            if previous.as_ref().is_some_and(|meta| meta.tombstone) {
                continue;
            }
            let (state, controls, frontier, history_policy) = {
                let log = self.log.lock().expect("log mutex");
                let events = log.events(id);
                let Some(state) =
                    super::super::channel::ChannelBook::project_membership(*id, &events).state
                else {
                    continue;
                };
                let (controls, frontier) = directory_view(&events);
                (
                    state,
                    controls,
                    frontier,
                    super::super::join_history::policy(&events),
                )
            };
            let fingerprint = directory_fingerprint(&controls);
            if old
                .as_ref()
                .and_then(|reference| reference.directory.as_ref())
                .is_some_and(|directory| {
                    !(directory.fingerprint == fingerprint
                        && directory.event_count == controls.len()
                        || directory.complete_frontier
                            && directory
                                .frontier
                                .iter()
                                .all(|id| controls.binary_search(id).is_ok())
                        || receipt.as_ref().is_some_and(|frontier| {
                            frontier
                                .iter()
                                .all(|id| controls.binary_search(&id.to_hex()).is_ok())
                        }))
                })
            {
                // A restored public prefix cannot rewrite this source's later
                // signed floor. Its exact receipt remains in the owning index.
                continue;
            }
            let eligible =
                self.channel_identity_matches_account(&state) && state.is_accepted_member(&own);
            if !eligible && previous.is_none() {
                continue;
            }
            let accepted: Vec<_> = state
                .members()
                .iter()
                .filter(|member| state.is_accepted_member(&member.user_id()))
                .cloned()
                .collect();
            let member_count = if state.owner_account_id().is_some() {
                accepted
                    .iter()
                    .filter_map(|member| state.member_account_id(&member.user_id()))
                    .collect::<BTreeSet<_>>()
                    .len()
            } else {
                let (accounts, unbound) = self.project_channel_member_accounts(&accepted);
                accounts.len() + unbound.len()
            };
            let mut members = state.members().to_vec();
            members.sort_by_key(|member| {
                (
                    member.user_id() != state.owner() && member.user_id() != own,
                    member.user_id(),
                )
            });
            let mut reference = GroupInventoryReference {
                channel_id,
                source_device: own.clone(),
                name: inventory_display_name(state.name(), *id),
                member_count,
                owner: state.owner().into(),
                owner_account_id: state.owner_account_id().map(str::to_owned),
                dissolved: state.is_dissolved(),
                left: !self.channel_identity_matches_account(&state),
                pending_invite: state.pending_invite(),
                legacy_membership: state.owner_account_id().is_none(),
                members: members
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
                                                || proof.account_id().as_ref()
                                                    == account_id.as_ref())
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
                            pending: !state.is_accepted_member(&public.user_id()),
                            public,
                            account_id,
                            name,
                        }
                    })
                    .collect(),
                members_complete: state.members().len() <= MAX_MEMBERS,
                observer: AccountDevice {
                    public: self.identity.public(),
                    certificate: self.account.certify(&self.identity.public().ed25519_pub),
                },
                directory: None,
                directory_reconciling: false,
            };
            reference.directory = Some(self.inventory_directory(
                &reference,
                old.as_ref(),
                &controls,
                &frontier,
                history_policy,
            ));
            if reference.bounded_payload(key.limit()).is_none() {
                let directory = reference.directory.as_mut().expect("directory present");
                directory.unavailable = true;
                directory.complete = false;
                directory.controls.clear();
                directory.bindings.clear();
                directory.certified.clear();
                directory.acknowledgements.clear();
                if reference.bounded_payload(key.limit()).is_none() {
                    continue;
                }
            }
            let directory = reference.directory.as_mut().expect("directory present");
            directory.bindings.retain(|proof| {
                reference
                    .members
                    .iter()
                    .any(|member| member.public == proof.public())
            });
            directory.certified.retain(|binding| {
                reference
                    .members
                    .iter()
                    .any(|member| member.public == binding.public)
            });
            reference.sign_directory(&self.identity);
            let payload = serde_json::to_vec(&reference).expect("directory serializes");
            // Missing local blobs can heal the exact signed body, but cannot
            // discard a later unreadable source floor using a guessed snapshot.
            if previous
                .as_ref()
                .is_some_and(|meta| old.is_none() && meta.content_hash != content_hash(&payload))
            {
                continue;
            }
            if self.log.lock().expect("log mutex").sync().is_err() {
                continue;
            }
            let changed = {
                let mut store = self.account_documents.lock().expect("documents mutex");
                if store.meta(&key) != previous {
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
                store.merge_local_group_inventory(meta, &payload, frontier)
            };
            match changed {
                Ok(true) => self.documents_changed(),
                Ok(false) => {}
                Err(error) => log::debug!("group inventory publication deferred: {error}"),
            }
            self.publish_group_activity_for(*id);
        }
    }
}
