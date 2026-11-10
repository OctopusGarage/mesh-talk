//! Mandatory account-group projections, signed by the actual source device.
use super::*;
use crate::{eventlog::ConversationId, node::account_group::wire::GroupControlHead};

pub(super) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    let AccountDocumentKey::GroupControl {
        channel,
        source_device,
    } = &meta.key
    else {
        return false;
    };
    if meta.tombstone {
        return false;
    }
    GroupControlHead::decode(payload, &meta.account_public).is_some_and(|head| {
        hex::encode(head.group.as_bytes()) == *channel
            && head.proof.public().user_id() == *source_device
            && meta.writer_device == *source_device
            && meta.revision == head.revision
    })
}

impl Node {
    pub(in crate::node) fn group_control_heads(
        &self,
        group: ConversationId,
    ) -> Vec<GroupControlHead> {
        let store = self.account_documents.lock().expect("documents mutex");
        store
            .group_control_rows(&hex::encode(group.as_bytes()))
            .into_iter()
            .filter_map(|meta| {
                let document = store.get(&meta.key).ok()??;
                let mut head = GroupControlHead::decode(
                    document.payload(),
                    &self.account.public().ed25519_pub,
                )?;
                if head.proof.public() == self.identity.public() {
                    head.access.local_frontier = store
                        .local_publication_frontier(&meta, &head.proof.public())
                        .unwrap_or_default();
                }
                Some(head)
            })
            .collect()
    }

    pub(in crate::node) fn group_control_has_unreadable_head(&self, group: ConversationId) -> bool {
        let store = self.account_documents.lock().expect("documents mutex");
        store
            .group_control_rows(&hex::encode(group.as_bytes()))
            .into_iter()
            .any(|meta| {
                store.get(&meta.key).ok().flatten().is_none_or(|document| {
                    GroupControlHead::decode(document.payload(), &self.account.public().ed25519_pub)
                        .is_none()
                })
            })
    }

    /// Publish only this device's real local projection. Borrowed source roles
    /// never become another device's claimed physical authority.
    pub(in crate::node) fn publish_account_group_access(&self, after: &mut Option<ConversationId>) {
        let mut groups = self.channels.lock().expect("channels mutex").channel_ids();
        groups.sort_by_key(|group| *group.as_bytes());
        let groups: Vec<_> = groups
            .into_iter()
            .filter(|group| after.is_none_or(|prior| group.as_bytes() > prior.as_bytes()))
            .take(16)
            .collect();
        self.publish_account_group_access_groups(&groups);
        *after = if groups.len() == 16 {
            groups.last().copied()
        } else {
            None
        };
    }

    /// An accepted effect refreshes its exact group before public fanout can wait.
    pub(in crate::node) fn publish_account_group_access_for(&self, group: ConversationId) {
        self.publish_account_group_access_groups(&[group]);
    }

    fn publish_account_group_access_groups(&self, groups: &[ConversationId]) {
        for group in groups {
            let key = AccountDocumentKey::GroupControl {
                channel: hex::encode(group.as_bytes()),
                source_device: self.user_id(),
            };
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            let (previous, old) = {
                let store = self.account_documents.lock().expect("documents mutex");
                let previous = store.meta(&key);
                let old = store.get(&key).ok().flatten().and_then(|document| {
                    let mut head = GroupControlHead::decode(
                        document.payload(),
                        &self.account.public().ed25519_pub,
                    )?;
                    if head.proof.public() == self.identity.public() {
                        head.access.local_frontier = store
                            .local_publication_frontier(&document.meta, &head.proof.public())
                            .unwrap_or_default();
                    }
                    Some(head)
                });
                (previous, old)
            };
            if previous.is_some() && old.is_none() {
                // Retain the durable source floor until its exact signed body is
                // repaired. An older public cache cannot guess its later role.
                continue;
            }
            if let Some(head) = &old {
                match self.account_group_observation_known(*group, head) {
                    Ok(true) => {}
                    Ok(false) => continue,
                    Err(error) => {
                        log::debug!("group access publication deferred: {error}");
                        continue;
                    }
                }
            }
            let access = match self
                .account_group_local_observation(*group, old.as_ref().map(|head| &head.access))
            {
                Ok(Some(access)) => access,
                Ok(None) => continue,
                Err(error) => {
                    log::debug!("group access publication deferred: {error}");
                    continue;
                }
            };
            if old.as_ref().is_some_and(|head| {
                head.access.same_canonical_observation(&access)
                    && head.access.acknowledgements == access.acknowledgements
            }) {
                continue;
            }
            let Some(revision) = previous
                .as_ref()
                .map_or(Some(1), |meta| meta.revision.checked_add(1))
            else {
                continue;
            };
            // Every checkpoint references durable public data. Do this without a
            // document-store lock so public-log readers never invert that order.
            if self.log.lock().expect("log mutex").sync().is_err() {
                continue;
            }
            let mut access = access;
            let proof = self.own_group_proof();
            let mut head = GroupControlHead::new(
                &self.identity,
                *group,
                revision,
                proof.clone(),
                access.clone(),
            );
            if access.controls.len() > crate::node::account_group::wire::MAX_PROVENANCE_CONTROLS
                || head.encode().len() > crate::node::account_group::wire::MAX_HEAD_BYTES
            {
                access.make_unavailable();
                head = GroupControlHead::new(
                    &self.identity,
                    *group,
                    revision,
                    proof.clone(),
                    access.clone(),
                );
            }
            let changed = {
                let mut store = self.account_documents.lock().expect("documents mutex");
                if store.meta(&key) != previous {
                    continue;
                }
                if !store.group_control_payload_fits(&key, head.encode().len()) {
                    access.make_unavailable();
                    head = GroupControlHead::new(
                        &self.identity,
                        *group,
                        revision,
                        proof.clone(),
                        access.clone(),
                    );
                }
                if access.unavailable
                    && old.as_ref().is_some_and(|old| {
                        old.access.unavailable
                            && old.access.source == access.source
                            && old.access.observation.public_state
                                == access.observation.public_state
                    })
                {
                    continue;
                }
                let payload = head.encode();
                let meta = AccountDocumentMeta::new(
                    &self.account,
                    self.user_id(),
                    key,
                    revision,
                    &payload,
                    false,
                );
                // Budget fallbacks are chosen BEFORE any index write. An I/O
                // error must not create two different heads at one revision.
                store.merge_local_group_control(meta, &payload, access.local_frontier.clone())
            };
            match changed {
                Ok(true) => self.documents_changed(),
                Ok(false) => {}
                Err(error) => log::debug!("account group access publication deferred: {error}"),
            }
        }
    }
}
