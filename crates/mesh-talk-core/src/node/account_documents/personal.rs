use super::*;
use crate::node::account_group::operations::journal::OperationReference;
use crate::node::account_group::{
    AccountGroupOperation, AccountGroupOperationId, AccountGroupRole, MembershipLifetime,
};
use crate::{discovery::Announce, eventlog::ConversationId, identity::device::PublicIdentity};

#[derive(Clone, Debug, Serialize)]
pub struct ConversationPersonalState {
    pub known: bool,
    pub hidden: bool,
    pub clear_history: bool,
    pub local_cleanup_complete: bool,
    pub floor: Option<DocumentVersion>,
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct PersonalControl {
    version: u8,
    pub(super) conversation: DocumentConversation,
    action_id: String,
    author: Announce,
    action: PersonalAction,
    device_signature: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum PersonalAction {
    LeaveChoice {
        reference: Box<OperationReference>,
        delete_history: bool,
    },
    Delete {
        clear_history: bool,
        closed: Vec<MembershipLifetime>,
        leave: Option<AccountGroupOperationId>,
    },
    Reopen {
        deletion: DocumentVersion,
    },
    Applied {
        floor: DocumentVersion,
        device: PublicIdentity,
    },
}
impl PersonalControl {
    fn message(&self) -> Vec<u8> {
        let mut copy = self.clone();
        copy.device_signature.clear();
        let mut bytes = b"mesh-talk-personal-conversation-control-v1".to_vec();
        bytes.extend(serde_json::to_vec(&copy).expect("personal control"));
        bytes
    }
    pub(super) fn affects_archive(&self) -> bool {
        matches!(
            self.action,
            PersonalAction::Delete {
                clear_history: true,
                ..
            } | PersonalAction::Applied { .. }
        )
    }
}
#[derive(Clone, Eq, PartialEq, Ord, PartialOrd)]
pub(super) struct AppliedKey {
    conversation: DocumentConversation,
    revision: u64,
    writer: String,
    hash: [u8; 32],
    ed: [u8; 32],
    x: [u8; 32],
}
impl AppliedKey {
    pub(super) fn new(
        conversation: &DocumentConversation,
        floor: &DocumentVersion,
        public: &PublicIdentity,
    ) -> Self {
        Self {
            conversation: conversation.clone(),
            revision: floor.revision,
            writer: floor.writer_device.clone(),
            hash: floor.content_hash,
            ed: public.ed25519_pub,
            x: public.x25519_pub,
        }
    }
}
#[derive(Clone)]
pub(in crate::node) struct PersonalHistoryCut {
    floors: std::collections::BTreeMap<DocumentConversation, DocumentVersion>,
    blocked: std::collections::BTreeSet<DocumentConversation>,
}
impl PersonalHistoryCut {
    pub(in crate::node) fn allows(&self, node: &Node, conversation: &DocumentConversation) -> bool {
        !self.blocked.contains(conversation)
            && self.floors.get(conversation).cloned()
                == node
                    .personal_floor(conversation)
                    .map(|(meta, _)| meta.version())
    }
}
pub(super) fn decode(meta: &AccountDocumentMeta, bytes: &[u8]) -> Option<PersonalControl> {
    let AccountDocumentKey::ConversationPersonal {
        conversation,
        action_id,
    } = &meta.key
    else {
        return None;
    };
    if meta.tombstone || bytes.len() > 64 * 1024 {
        return None;
    }
    let value: PersonalControl = serde_json::from_slice(bytes).ok()?;
    if value.version != 1
        || &value.conversation != conversation
        || &value.action_id != action_id
        || !value.author.verify()
        || value.author.post_office
        || value.author.public().user_id() != meta.writer_device
        || value.author.account_cert.as_ref()?.account_ed25519_pub != meta.account_public
    {
        return None;
    }
    let signature: [u8; 64] = value.device_signature.as_slice().try_into().ok()?;
    if !DeviceIdentity::verify(&value.author.ed25519_pub, &value.message(), &signature) {
        return None;
    }
    let valid = match &value.action {
        PersonalAction::LeaveChoice { reference, .. } => {
            reference.valid(&meta.account_public)
                && reference.kind
                    == crate::node::account_group::AccountGroupOperationKind::LeaveAccount
                && reference.origin.public() == value.author.public()
                && *conversation
                    == DocumentConversation::Channel(hex::encode(reference.group.as_bytes()))
        }
        PersonalAction::Delete { closed, leave, .. } => {
            closed.len() <= 65
                && match conversation {
                    DocumentConversation::Account(_) => closed.is_empty() && leave.is_none(),
                    DocumentConversation::Channel(_) => !closed.is_empty(),
                }
        }
        PersonalAction::Applied { device, floor } => {
            *device == value.author.public() && floor.revision > 0
        }
        PersonalAction::Reopen { deletion } => {
            matches!(conversation, DocumentConversation::Account(_)) && deletion.revision > 0
        }
    };
    valid.then_some(value)
}
fn action_id(label: &[u8], bytes: &[u8]) -> String {
    let mut hash = Sha256::new();
    hash.update(b"mesh-talk-conversation-personal-v1");
    hash.update(label);
    hash.update(bytes);
    hex::encode(hash.finalize())
}
fn channel(group: ConversationId) -> DocumentConversation {
    DocumentConversation::Channel(hex::encode(group.as_bytes()))
}

impl super::store::AccountDocumentStore {
    pub(super) fn index_personal_control(
        &mut self,
        meta: AccountDocumentMeta,
        control: PersonalControl,
    ) {
        self.personal_pending.insert(control.conversation.clone());
        if let PersonalAction::Delete {
            clear_history: true,
            ..
        } = &control.action
        {
            let replace = self
                .personal_floors
                .get(&control.conversation)
                .is_none_or(|(old, _)| old.order() < meta.order());
            if replace {
                self.personal_floors.insert(
                    control.conversation.clone(),
                    (meta.clone(), control.clone()),
                );
            }
        }
        if let PersonalAction::Applied { floor, device } = &control.action {
            self.personal_applied
                .insert(AppliedKey::new(&control.conversation, floor, device));
        }
        let affects = control.affects_archive();
        self.personal.insert(meta.key.clone(), (meta, control));
        if affects {
            let mut hash = Sha256::new();
            hash.update(b"mesh-talk-personal-archive-cut-v1");
            for (meta, control) in self
                .personal
                .values()
                .filter(|(_, control)| control.affects_archive())
            {
                hash.update(meta.content_hash);
                hash.update(meta.revision.to_be_bytes());
                hash.update(meta.writer_device.as_bytes());
                let _ = control;
            }
            self.personal_archive_digest = hash.finalize().into();
        }
    }
}
impl Node {
    fn check_personal_account(&self, expected: &str) -> Result<(), NodeError> {
        if self.account_id() != expected
            || self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            || self.runtime_work.is_closed()
        {
            return Err(NodeError::Authorization("account runtime changed".into()));
        }
        Ok(())
    }
    fn personal_controls(
        &self,
        conversation: &DocumentConversation,
    ) -> Vec<(AccountDocumentMeta, PersonalControl)> {
        let start = AccountDocumentKey::ConversationPersonal {
            conversation: conversation.clone(),
            action_id: String::new(),
        };
        self.account_documents
            .lock()
            .expect("documents mutex")
            .personal
            .range(start..)
            .take_while(|(_, (_, control))| &control.conversation == conversation)
            .map(|(_, value)| value.clone())
            .collect()
    }
    fn write_personal(
        &self,
        conversation: DocumentConversation,
        id: String,
        action: PersonalAction,
    ) -> Result<AccountDocumentMeta, NodeError> {
        let _floor_commit = matches!(
            action,
            PersonalAction::Delete {
                clear_history: true,
                ..
            }
        )
        .then(|| {
            self.account_sync_commit
                .lock()
                .expect("account sync commit mutex")
        });
        let key = AccountDocumentKey::ConversationPersonal {
            conversation: conversation.clone(),
            action_id: id.clone(),
        };
        let mut control = PersonalControl {
            version: 1,
            conversation,
            action_id: id,
            author: self.own_group_proof(),
            action,
            device_signature: Vec::new(),
        };
        control.device_signature = self.identity.sign(&control.message()).to_vec();
        let bytes = serde_json::to_vec(&control)
            .map_err(|_| NodeError::InvalidInput("invalid personal control".into()))?;
        let mut store = self.account_documents.lock().expect("documents mutex");
        if let Some((meta, old)) = store.personal.get(&key) {
            if serde_json::to_vec(&old.action).ok() != serde_json::to_vec(&control.action).ok() {
                return Err(NodeError::InvalidInput(
                    "personal action ID belongs to another choice".into(),
                ));
            }
            return Ok(meta.clone());
        }
        let revision = store
            .personal
            .values()
            .map(|(meta, _)| meta.revision)
            .max()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or_else(|| NodeError::InvalidInput("personal revision exhausted".into()))?;
        let meta =
            AccountDocumentMeta::new(&self.account, self.user_id(), key, revision, &bytes, false);
        store.merge(meta.clone(), &bytes).map_err(NodeError::Log)?;
        drop(store);
        self.documents_changed();
        Ok(meta)
    }
    fn personal_floor(
        &self,
        conversation: &DocumentConversation,
    ) -> Option<(AccountDocumentMeta, PersonalControl)> {
        self.account_documents
            .lock()
            .expect("documents mutex")
            .personal_floors
            .get(conversation)
            .cloned()
    }
    pub(in crate::node) fn personal_source_applied(
        &self,
        conversation: &DocumentConversation,
        public: &PublicIdentity,
    ) -> bool {
        let store = self.account_documents.lock().expect("documents mutex");
        store
            .personal_floors
            .get(conversation)
            .is_none_or(|(meta, _)| {
                store.personal_applied.contains(&AppliedKey::new(
                    conversation,
                    &meta.version(),
                    public,
                ))
            })
    }
    pub(in crate::node) fn capture_personal_history_cut(
        &self,
        source: &PublicIdentity,
    ) -> PersonalHistoryCut {
        let store = self.account_documents.lock().expect("documents mutex");
        let mut floors = std::collections::BTreeMap::new();
        let mut blocked = std::collections::BTreeSet::new();
        for (conversation, (meta, _)) in &store.personal_floors {
            let version = meta.version();
            let allowed = store.personal_applied.contains(&AppliedKey::new(
                conversation,
                &version,
                &self.identity.public(),
            )) && store.personal_applied.contains(&AppliedKey::new(
                conversation,
                &version,
                source,
            ));
            if !allowed {
                blocked.insert(conversation.clone());
            }
            floors.insert(conversation.clone(), version);
        }
        PersonalHistoryCut { floors, blocked }
    }
    pub(in crate::node) fn personal_history_source_allowed(
        &self,
        conversation: &DocumentConversation,
        source: Option<&PublicIdentity>,
    ) -> bool {
        if self.personal_floor(conversation).is_none() {
            return true;
        }
        self.personal_source_applied(conversation, &self.identity.public())
            && source.is_some_and(|source| {
                self.certified_sibling(source) && self.personal_source_applied(conversation, source)
            })
    }
    /// A late old floor never authorizes old rows in a fresh membership. Only
    /// signed causal descent from a currently authorized disjoint lifetime does.
    pub(in crate::node) fn personal_channel_record_current(
        &self,
        group: ConversationId,
        event: crate::eventlog::EventId,
    ) -> bool {
        let Some((_, control)) = self.personal_floor(&channel(group)) else {
            return false;
        };
        let PersonalAction::Delete { closed, .. } = control.action else {
            return false;
        };
        let Ok(access) = self.account_group_access_with_closed(group) else {
            return false;
        };
        let anchors: std::collections::HashSet<_> = access
            .sources
            .iter()
            .filter(|source| {
                matches!(
                    source.role,
                    AccountGroupRole::Owner | AccountGroupRole::Member | AccountGroupRole::Invited
                ) && !closed.contains(&source.lifetime)
            })
            .map(|source| {
                source
                    .lifetime
                    .acceptance
                    .unwrap_or(source.lifetime.membership)
            })
            .collect();
        if anchors.is_empty() {
            return false;
        }
        let log = self.log.lock().expect("log mutex");
        let mut pending = vec![event];
        let mut visited = std::collections::HashSet::new();
        while let Some(id) = pending.pop() {
            if !visited.insert(id) {
                continue;
            }
            let row = match log.get(&id) {
                Ok(Some(row)) if row.conversation_id == group => row,
                Ok(_) => continue,
                Err(error) => {
                    log::debug!("personal channel ancestry unavailable: {error}");
                    return false;
                }
            };
            if anchors.contains(&id) {
                return true;
            }
            pending.extend(row.parents.iter().copied());
        }
        false
    }
    pub(in crate::node) fn personal_floor_digest(&self) -> [u8; 32] {
        self.account_documents
            .lock()
            .expect("documents mutex")
            .personal_archive_digest
    }
    pub fn personal_account_ids(&self) -> Vec<String> {
        let store = self.account_documents.lock().expect("documents mutex");
        let mut ids: Vec<_> = store
            .personal
            .values()
            .filter_map(|(_, c)| match &c.conversation {
                DocumentConversation::Account(id) => Some(id.clone()),
                _ => None,
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
    pub fn conversation_personal_state(
        &self,
        conversation: &DocumentConversation,
    ) -> ConversationPersonalState {
        let controls = self.personal_controls(conversation);
        let deleted = controls
            .iter()
            .filter(|(_, c)| matches!(c.action, PersonalAction::Delete { .. }))
            .max_by(|(a, _), (b, _)| a.order().cmp(&b.order()));
        let floor = self.personal_floor(conversation).map(|(m, _)| m.version());
        let complete =
            floor.is_none() || self.personal_source_applied(conversation, &self.identity.public());
        let Some((meta, control)) = deleted else {
            return ConversationPersonalState {
                known: false,
                hidden: false,
                clear_history: false,
                local_cleanup_complete: complete,
                floor,
            };
        };
        let restored = controls.iter().any(|(m, c)| {
            m.order() > meta.order()
                && matches!(&c.action,PersonalAction::Reopen{deletion} if *deletion==meta.version())
        });
        let group_current = match conversation {
            DocumentConversation::Account(_) => true,
            DocumentConversation::Channel(id) => hex::decode(id)
                .ok()
                .and_then(|b| <[u8; 32]>::try_from(b).ok())
                .is_some_and(|id| {
                    let Ok(access) = self.account_group_access_with_closed(ConversationId::new(id))
                    else {
                        return false;
                    };
                    let PersonalAction::Delete { closed, .. } = &control.action else {
                        return false;
                    };
                    matches!(
                        access.role,
                        AccountGroupRole::Left | AccountGroupRole::Dissolved
                    ) && !access.sources.is_empty()
                        && access
                            .sources
                            .iter()
                            .all(|source| closed.contains(&source.lifetime))
                }),
        };
        ConversationPersonalState {
            known: true,
            hidden: !restored && group_current,
            clear_history: matches!(
                control.action,
                PersonalAction::Delete {
                    clear_history: true,
                    ..
                }
            ),
            local_cleanup_complete: complete,
            floor,
        }
    }
    pub fn account_group_leave_cleanup(&self, id: AccountGroupOperationId) -> Option<bool> {
        self.account_documents
            .lock()
            .expect("documents mutex")
            .personal
            .values()
            .find_map(|(_, c)| match &c.action {
                PersonalAction::LeaveChoice {
                    reference,
                    delete_history,
                } if reference.id == id => Some(*delete_history),
                _ => None,
            })
    }
    pub fn account_group_leave_cleanup_complete(
        &self,
        id: AccountGroupOperationId,
    ) -> Option<bool> {
        let requested = self.account_group_leave_cleanup(id)?;
        if !requested {
            return Some(true);
        }
        let controls: Vec<_> = self
            .account_documents
            .lock()
            .expect("documents mutex")
            .personal
            .values()
            .cloned()
            .collect();
        let original = controls
            .iter()
            .find_map(|(_, control)| match &control.action {
                PersonalAction::LeaveChoice { reference, .. } if reference.id == id => {
                    Some(reference)
                }
                _ => None,
            });
        Some(original.is_some_and(|reference| controls.iter().any(|(meta,control)| {
            control.conversation==channel(reference.group)
                && matches!(&control.action,PersonalAction::Delete{leave:Some(operation),closed,..}
                    if *operation==id && closed.contains(&reference.executor.lifetime))
                && controls.iter().any(|(_,ack)|matches!(&ack.action,PersonalAction::Applied{floor,device}
                    if *floor==meta.version() && *device==self.identity.public()))
        })))
    }
    pub fn submit_account_group_leave_if(
        &self,
        expected: &str,
        id: AccountGroupOperationId,
        group: ConversationId,
        delete_history: Option<bool>,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<crate::node::AccountGroupOperationStatus, NodeError> {
        self.check_personal_account(expected)?;
        let prior = self.account_group_leave_cleanup(id);
        if prior.is_none() && delete_history.is_none() {
            return self.submit_account_group_operation_if(
                expected,
                id,
                group,
                AccountGroupOperation::LeaveAccount,
                authorize,
            );
        }
        if prior.is_none()
            && delete_history == Some(true)
            && self.account_group_operation_status(expected, id)?.is_some()
        {
            return Err(NodeError::InvalidInput(
                "an accepted operation cannot acquire a new cleanup choice".into(),
            ));
        }
        if prior.zip(delete_history).is_some_and(|(a, b)| a != b) {
            return Err(NodeError::InvalidInput(
                "operation ID belongs to another cleanup choice".into(),
            ));
        }
        let choice = delete_history.or(prior).unwrap_or(false);
        self.submit_account_group_operation_with_prepared_if(
            expected,
            id,
            group,
            AccountGroupOperation::LeaveAccount,
            |reference| {
                let existing = self
                    .personal_controls(&channel(group))
                    .into_iter()
                    .find_map(|(_, control)| match control.action {
                        PersonalAction::LeaveChoice {
                            reference: retained,
                            delete_history,
                        } if retained.id == id => Some((retained, delete_history)),
                        _ => None,
                    });
                if let Some((retained, old_choice)) = existing {
                    if old_choice != choice
                        || retained.group != group
                        || retained.digest != reference.digest
                        || retained.origin.public() != self.identity.public()
                    {
                        return Err(NodeError::InvalidInput(
                            "operation ID belongs to another cleanup intent".into(),
                        ));
                    }
                    *reference = *retained;
                    return Ok(());
                }
                self.write_personal(
                    channel(group),
                    hex::encode(id.as_bytes()),
                    PersonalAction::LeaveChoice {
                        reference: Box::new(reference.clone()),
                        delete_history: choice,
                    },
                )?;
                Ok(())
            },
            authorize,
        )
    }
    fn persist_group_delete(
        &self,
        group: ConversationId,
        leave: Option<AccountGroupOperationId>,
    ) -> Result<(), NodeError> {
        let access = self.account_group_access_with_closed(group)?;
        if !matches!(
            access.role,
            AccountGroupRole::Left | AccountGroupRole::Dissolved
        ) || access.sources.is_empty()
        {
            return Err(NodeError::Authorization("group is not left".into()));
        }
        let id = leave.map_or_else(
            || {
                self.new_account_group_operation_id(&self.account_id())
                    .map(|id| hex::encode(id.as_bytes()))
            },
            |id| Ok(action_id(b"leave-delete", id.as_bytes())),
        )?;
        let closed = access
            .sources
            .into_iter()
            .map(|source| source.lifetime)
            .collect();
        self.write_personal(
            channel(group),
            id,
            PersonalAction::Delete {
                clear_history: true,
                closed,
                leave,
            },
        )?;
        Ok(())
    }
    pub fn delete_left_group_if(
        &self,
        expected: &str,
        group: ConversationId,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let mut committed = false;
        authorize(&mut || {
            self.check_personal_account(expected)?;
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("membership commit mutex");
            if !self.conversation_personal_state(&channel(group)).hidden {
                self.persist_group_delete(group, None)?;
            }
            committed = true;
            Ok(())
        })?;
        if !committed {
            return Err(NodeError::Authorization(
                "authorization did not execute deletion".into(),
            ));
        }
        self.retry_personal_cleanup_for(&channel(group))?;
        if !self
            .conversation_personal_state(&channel(group))
            .local_cleanup_complete
        {
            return Err(NodeError::Channel(
                "personal cleanup remains pending".into(),
            ));
        }
        Ok(())
    }
    pub fn delete_account_contact_if(
        &self,
        expected: &str,
        peer: &str,
        clear_history: bool,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        if !hex(peer, 32) || peer == self.account_id() {
            return Err(NodeError::InvalidInput("invalid contact account".into()));
        }
        let mut committed = false;
        authorize(&mut || {
            self.check_personal_account(expected)?;
            let state =
                self.conversation_personal_state(&DocumentConversation::Account(peer.into()));
            if state.known && state.hidden && state.clear_history == clear_history {
                committed = true;
                return Ok(());
            }
            let id = self.new_account_group_operation_id(expected)?;
            self.write_personal(
                DocumentConversation::Account(peer.into()),
                hex::encode(id.as_bytes()),
                PersonalAction::Delete {
                    clear_history,
                    closed: vec![],
                    leave: None,
                },
            )?;
            committed = true;
            Ok(())
        })?;
        if !committed {
            return Err(NodeError::Authorization(
                "authorization did not execute deletion".into(),
            ));
        }
        self.retry_personal_cleanup_for(&DocumentConversation::Account(peer.into()))
    }
    pub fn reopen_account_conversation_if(
        &self,
        expected: &str,
        peer: &str,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        self.check_personal_account(expected)?;
        if !hex(peer, 32) || peer == self.account_id() {
            return Err(NodeError::InvalidInput("invalid contact account".into()));
        }
        let mut permit = None;
        authorize(&mut || {
            self.check_personal_account(expected)?;
            permit = Some(
                self.runtime_work
                    .admit()
                    .ok_or_else(|| NodeError::Authorization("runtime is stopping".into()))?,
            );
            Ok(())
        })?;
        let _permit = permit.ok_or_else(|| {
            NodeError::Authorization("authorization did not execute reopen".into())
        })?;
        self.check_personal_account(expected)?;
        let conversation = DocumentConversation::Account(peer.into());
        self.retry_personal_cleanup_for(&conversation)?;
        self.reopen_personal_after_live(peer)
    }
    pub(in crate::node) fn personal_cleanup_next(
        &self,
        after: Option<&DocumentConversation>,
    ) -> Option<DocumentConversation> {
        use std::ops::Bound::{Excluded, Unbounded};
        let store = self.account_documents.lock().expect("documents mutex");
        after
            .and_then(|after| {
                store
                    .personal_pending
                    .range((Excluded(after.clone()), Unbounded))
                    .next()
            })
            .or_else(|| store.personal_pending.first())
            .cloned()
    }
    pub(in crate::node) fn retry_conversation_personal_cleanup(&self) -> Result<(), NodeError> {
        if let Some(conversation) = self.personal_cleanup_next(None) {
            self.retry_personal_cleanup_for(&conversation)?;
        }
        Ok(())
    }
    pub(in crate::node) fn retry_personal_cleanup_for(
        &self,
        target: &DocumentConversation,
    ) -> Result<(), NodeError> {
        self.check_personal_account(&self.account_id())?;
        let controls = self.personal_controls(target);
        let choices:Vec<_>=controls.iter().filter_map(|(_,control)|match &control.action {
            PersonalAction::LeaveChoice{reference,delete_history:true}
                if !controls.iter().any(|(_,control)|matches!(control.action,PersonalAction::Delete{leave:Some(id),..} if id==reference.id))
                =>Some(reference.clone()),_=>None}).filter(|reference|self.committed_personal_leave(reference)).take(1).collect();
        for reference in choices {
            if self.account_group_leave_cleanup_complete(reference.id) == Some(true) {
                continue;
            }
            if self.committed_personal_leave(&reference) {
                let _membership = self
                    .channel_membership_commit
                    .lock()
                    .expect("membership commit mutex");
                let access = match self.account_group_access_with_closed(reference.group) {
                    Ok(access) => access,
                    Err(NodeError::Log(crate::eventlog::LogError::StaleCursor)) => {
                        // The leave is committed, but its canonical metadata may
                        // still be preparing. Keep the cleanup ticket and let the
                        // existing recovery owner finish before testing the exact
                        // closed lifetime; never erase from a partial projection.
                        self.channel_checkpoints.queue_runtime(reference.group)?;
                        self.channel_recovery_notify.notify_one();
                        return Ok(());
                    }
                    Err(error) => return Err(error),
                };
                if access.role == AccountGroupRole::Left
                    && access
                        .sources
                        .iter()
                        .any(|s| s.lifetime == reference.executor.lifetime)
                {
                    self.persist_group_delete(reference.group, Some(reference.id))?;
                }
            }
        }
        if let Some((meta, control)) = self.personal_floor(target) {
            self.apply_personal_floor(target, &meta, &control, None)?;
        }
        self.account_documents
            .lock()
            .expect("documents mutex")
            .personal_pending
            .remove(target);
        Ok(())
    }
    fn apply_personal_floor(
        &self,
        conversation: &DocumentConversation,
        meta: &AccountDocumentMeta,
        control: &PersonalControl,
        delivery: Option<&mut crate::node::delivery_store::DeliveryStore>,
    ) -> Result<bool, NodeError> {
        self.check_personal_account(&self.account_id())?;
        if self.personal_source_applied(conversation, &self.identity.public()) {
            return Ok(true);
        }
        let _membership = matches!(conversation, DocumentConversation::Channel(_)).then(|| {
            self.channel_membership_commit
                .lock()
                .expect("membership commit mutex")
        });
        match &conversation {
            DocumentConversation::Account(peer) => {
                if let Some(store) = delivery {
                    self.clear_account_conversation_with_store(peer, store)?;
                } else {
                    self.clear_account_conversation(peer)?;
                }
            }
            DocumentConversation::Channel(id) => {
                let bytes: [u8; 32] = hex::decode(id)
                    .ok()
                    .and_then(|b| b.try_into().ok())
                    .ok_or_else(|| NodeError::InvalidInput("invalid channel".into()))?;
                let access = self.account_group_access_with_closed(ConversationId::new(bytes))?;
                let PersonalAction::Delete { closed, .. } = &control.action else {
                    return Ok(false);
                };
                let fresh: Vec<_> = access
                    .sources
                    .iter()
                    .filter(|source| {
                        matches!(
                            source.role,
                            AccountGroupRole::Owner
                                | AccountGroupRole::Member
                                | AccountGroupRole::Invited
                        ) && !closed.contains(&source.lifetime)
                    })
                    .map(|source| source.lifetime.clone())
                    .collect();
                if fresh.is_empty() {
                    if !matches!(
                        access.role,
                        AccountGroupRole::Left | AccountGroupRole::Dissolved
                    ) || access.sources.is_empty()
                        || !access
                            .sources
                            .iter()
                            .all(|source| closed.contains(&source.lifetime))
                    {
                        return Ok(false);
                    }
                    self.clear_conversation(ConversationId::new(bytes))?;
                } else {
                    self.clear_channel_conversation_preserving_lifetimes(
                        ConversationId::new(bytes),
                        &fresh,
                    )?;
                }
            }
        }
        {
            let mut store = self.account_documents.lock().expect("documents mutex");
            let key = AccountDocumentKey::Draft {
                conversation: conversation.clone(),
            };
            if let Some(draft) = store.meta(&key).filter(|meta| !meta.tombstone) {
                let preserve = if let (
                    DocumentConversation::Channel(id),
                    PersonalAction::Delete { closed, .. },
                ) = (conversation, &control.action)
                {
                    let group = hex::decode(id)
                        .ok()
                        .and_then(|b| <[u8; 32]>::try_from(b).ok())
                        .map(ConversationId::new);
                    // The durable local receipt is evidence of this exact draft's
                    // authorization scope, never a comparison of unrelated clocks.
                    let receipt = store.local_draft_lifetime(&draft, &self.identity.public());
                    drop(store);
                    let preserve = group.zip(receipt).is_some_and(|(group, lifetime)| {
                        !closed.contains(&lifetime)
                            && self
                                .account_group_access_with_closed(group)
                                .is_ok_and(|access| {
                                    access.sources.iter().any(|source| {
                                        source.lifetime == lifetime
                                            && matches!(
                                                source.role,
                                                AccountGroupRole::Owner
                                                    | AccountGroupRole::Member
                                                    | AccountGroupRole::Invited
                                            )
                                    })
                                })
                    });
                    store = self.account_documents.lock().expect("documents mutex");
                    preserve
                } else {
                    false
                };
                if !preserve {
                    store.put(
                        &self.account,
                        self.user_id(),
                        key,
                        &[],
                        Some(&draft.version()),
                        true,
                    )?;
                }
            }
        }
        if self
            .personal_floor(conversation)
            .is_none_or(|(current, _)| current.version() != meta.version())
        {
            return Err(NodeError::Channel("personal erasure floor changed".into()));
        }
        let mut marker = serde_json::to_vec(&meta.version()).expect("floor version");
        marker.extend(self.identity.public().ed25519_pub);
        marker.extend(self.identity.public().x25519_pub);
        self.write_personal(
            conversation.clone(),
            action_id(b"applied", &marker),
            PersonalAction::Applied {
                floor: meta.version(),
                device: self.identity.public(),
            },
        )?;

        Ok(true)
    }
    pub(in crate::node) fn prepare_live_personal_dm(
        &self,
        peer: &str,
        delivery: &mut crate::node::delivery_store::DeliveryStore,
    ) -> Result<(), NodeError> {
        let conversation = DocumentConversation::Account(peer.into());
        if let Some((meta, control)) = self.personal_floor(&conversation) {
            self.apply_personal_floor(&conversation, &meta, &control, Some(delivery))?;
        }
        Ok(())
    }
    pub(in crate::node) fn reopen_personal_after_live(&self, peer: &str) -> Result<(), NodeError> {
        self.check_personal_account(&self.account_id())?;
        let conversation = DocumentConversation::Account(peer.into());
        let state = self.conversation_personal_state(&conversation);
        if !state.known || !state.hidden {
            return Ok(());
        }
        if !state.local_cleanup_complete {
            return Err(NodeError::Channel(
                "personal cleanup remains pending".into(),
            ));
        }
        if let Some((meta, _)) = self
            .personal_controls(&conversation)
            .into_iter()
            .filter(|(_, c)| matches!(c.action, PersonalAction::Delete { .. }))
            .max_by(|(a, _), (b, _)| a.order().cmp(&b.order()))
        {
            let id = self.new_account_group_operation_id(&self.account_id())?;
            self.write_personal(
                conversation,
                hex::encode(id.as_bytes()),
                PersonalAction::Reopen {
                    deletion: meta.version(),
                },
            )?;
        }
        Ok(())
    }
}

// Explicit account-personal conversation deletion, independent of membership.

#[cfg(test)]
mod tests {
    use super::super::*;
    use super::PersonalAction;
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        eventlog::{Event, EventId, EventKind},
        node::account_group::{AccountGroupOperationPhase, AccountGroupRole},
    };

    #[test]
    fn unauthorized_reopen_cannot_apply_a_pending_personal_floor() {
        for mode in 0..4 {
            let dir = tempfile::tempdir().unwrap();
            let account = Account::generate();
            let node = super::super::tests::node(dir.path(), &account);
            let peer = "a".repeat(32);
            let conversation = DocumentConversation::Account(peer.clone());
            let conv =
                crate::node::conversation::account_conversation_id(&account.account_id(), &peer);
            node.received
                .lock()
                .unwrap()
                .record(
                    conv,
                    "b".repeat(32),
                    1,
                    b"pending cleanup row",
                    crate::eventlog::EventId::new([6; 32]),
                )
                .unwrap();
            node.write_personal(
                conversation.clone(),
                "1".repeat(64),
                PersonalAction::Delete {
                    clear_history: true,
                    closed: vec![],
                    leave: None,
                },
            )
            .unwrap();
            if mode == 3 {
                node.runtime_work.close();
            }
            let expected = if mode == 0 {
                "c".repeat(32)
            } else {
                account.account_id()
            };
            let result =
                node.reopen_account_conversation_if(&expected, &peer, |commit| match mode {
                    1 => Err(NodeError::Authorization("denied".into())),
                    2 => Ok(()),
                    _ => commit(),
                });
            assert!(
                result.is_err(),
                "wrong Root, denied, missing commit or retired runtime cannot reopen"
            );
            assert!(
                !node
                    .conversation_personal_state(&conversation)
                    .local_cleanup_complete,
                "rejected authorization must not write a cleanup acknowledgment"
            );
            assert_eq!(
                node.received.lock().unwrap().entries(&conv).len(),
                1,
                "rejected authorization must not erase pending plaintext"
            );
        }
    }

    #[tokio::test]
    async fn original_leave_choice_is_immutable_and_cancelled_intent_never_erases() {
        for choice in [None, Some(false), Some(true)] {
            let dir = tempfile::tempdir().unwrap();
            let account = Account::generate();
            let node = super::super::tests::node(dir.path(), &account);
            let owner = DeviceIdentity::generate();
            let group = new_channel_id();
            let genesis = Event::new(
                &owner,
                group,
                1,
                vec![],
                1,
                1,
                EventKind::MembershipChange,
                ChannelMeta {
                    name: "Choice".into(),
                    members: vec![owner.public(), node.identity.public()],
                    epoch: 0,
                    owner: owner.public().user_id(),
                }
                .encode(),
            );
            node.log.lock().unwrap().append_durable(genesis).unwrap();
            node.process_channel(group);
            super::super::tests::settle_group_metadata(&node, group);
            node.received
                .lock()
                .unwrap()
                .record(
                    group,
                    owner.public().user_id(),
                    1,
                    b"retained",
                    EventId::new([7; 32]),
                )
                .unwrap();
            let id = node
                .new_account_group_operation_id(&account.account_id())
                .unwrap();
            node.submit_account_group_leave_if(
                &account.account_id(),
                id,
                group,
                choice,
                |commit| commit(),
            )
            .unwrap();
            assert_eq!(
                node.account_group_leave_cleanup(id),
                choice,
                "missing legacy choice remains absent"
            );
            let changed = if choice == Some(true) {
                Some(false)
            } else {
                Some(true)
            };
            assert!(
                node.submit_account_group_leave_if(
                    &account.account_id(),
                    id,
                    group,
                    changed,
                    |commit| commit()
                )
                .is_err(),
                "an existing operation cannot gain or change destructive consent"
            );
            node.cancel_account_group_operation(&account.account_id(), id)
                .unwrap();
            for _ in 0..8 {
                node.maintain_account_group_operation_effects();
                node.retry_conversation_personal_cleanup().unwrap();
            }
            assert_eq!(node.received.lock().unwrap().entries(&group).len(), 1);
            let state = node.conversation_personal_state(&super::channel(group));
            assert!(
                !state.hidden && state.floor.is_none(),
                "cancelled intent is not a deletion"
            );
        }
    }

    #[tokio::test]
    async fn late_personal_group_floor_preserves_fresh_messages_and_atomic_draft_receipt() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let node = super::super::tests::node(dir.path(), &account);
        let owner = DeviceIdentity::generate();
        let group = new_channel_id();
        let meta = |epoch| {
            ChannelMeta {
                name: "Fresh scope".into(),
                members: vec![owner.public(), node.identity.public()],
                epoch,
                owner: owner.public().user_id(),
            }
            .encode()
        };
        let genesis = Event::new(
            &owner,
            group,
            1,
            vec![],
            1,
            10,
            EventKind::MembershipChange,
            meta(0),
        );
        let old = Event::new(
            &node.identity,
            group,
            1,
            vec![genesis.id],
            2,
            10,
            EventKind::Message,
            vec![1],
        );
        let leave = Event::new(
            &node.identity,
            group,
            2,
            vec![old.id],
            3,
            10,
            EventKind::ChannelLeave,
            vec![],
        );
        for row in [&genesis, &old, &leave] {
            node.log
                .lock()
                .unwrap()
                .append_durable(row.clone())
                .unwrap();
        }
        node.process_channel(group);
        super::super::tests::settle_group_metadata(&node, group);
        node.persist_group_delete(group, None).unwrap();
        let rejoin = Event::new(
            &owner,
            group,
            2,
            vec![leave.id],
            4,
            10,
            EventKind::MembershipChange,
            meta(2),
        );
        let accept = Event::new(
            &node.identity,
            group,
            3,
            vec![rejoin.id],
            5,
            10,
            EventKind::ChannelAccept,
            vec![],
        );
        let fresh = Event::new(
            &node.identity,
            group,
            4,
            vec![accept.id],
            6,
            10,
            EventKind::Message,
            vec![2],
        );
        for row in [&rejoin, &accept, &fresh] {
            node.log
                .lock()
                .unwrap()
                .append_durable(row.clone())
                .unwrap();
        }
        node.process_channel(group);
        super::super::tests::settle_group_metadata(&node, group);
        let conversation = super::channel(group);
        let draft = AccountDocumentKey::Draft {
            conversation: conversation.clone(),
        };
        let written = node
            .put_account_document_if(draft.clone(), b"fresh draft".to_vec(), None, |commit| {
                commit()
            })
            .unwrap();
        for row in [&old, &fresh] {
            node.received
                .lock()
                .unwrap()
                .record(
                    group,
                    node.user_id(),
                    row.wall_clock,
                    if row.id == old.id { b"old" } else { b"fresh" },
                    row.id,
                )
                .unwrap();
        }
        assert!(node.personal_channel_record_current(group, fresh.id));
        assert!(!node.personal_channel_record_current(group, old.id));
        node.retry_personal_cleanup_for(&conversation).unwrap();
        let state = node.conversation_personal_state(&conversation);
        assert!(!state.hidden && state.local_cleanup_complete);
        let rows = node.received.lock().unwrap().entries(&group).to_vec();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].event_id, fresh.id);
        assert_eq!(
            node.account_document(&draft).unwrap().unwrap().payload(),
            b"fresh draft"
        );
        let identity = node.identity.secret_bytes();
        drop(node);
        let reopened = super::super::tests::node_with_identity(
            dir.path(),
            &account,
            DeviceIdentity::from_secret_bytes(identity.0, identity.1),
        );
        assert_eq!(
            reopened
                .account_document(&draft)
                .unwrap()
                .unwrap()
                .meta
                .version(),
            written.version()
        );
        assert!(
            reopened
                .conversation_personal_state(&conversation)
                .local_cleanup_complete
        );
    }

    #[test]
    fn personal_applied_requires_the_exact_full_device_identity() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let node = super::super::tests::node(&dir.path().join("own"), &account);
        let secrets = node.identity.secret_bytes();
        let other_x = DeviceIdentity::generate().secret_bytes().1;
        let other = super::super::tests::node_with_identity(
            &dir.path().join("other"),
            &account,
            DeviceIdentity::from_secret_bytes(secrets.0, other_x),
        );
        let conversation = DocumentConversation::Account("a".repeat(32));
        let floor = node
            .write_personal(
                conversation.clone(),
                "1".repeat(64),
                PersonalAction::Delete {
                    clear_history: true,
                    closed: vec![],
                    leave: None,
                },
            )
            .unwrap();
        let ack = other
            .write_personal(
                conversation.clone(),
                "2".repeat(64),
                PersonalAction::Applied {
                    floor: floor.version(),
                    device: other.identity.public(),
                },
            )
            .unwrap();
        let key = ack.key.clone();
        let payload = other.account_document(&key).unwrap().unwrap();
        node.account_documents
            .lock()
            .unwrap()
            .merge(ack, payload.payload())
            .unwrap();
        assert!(
            !node.personal_source_applied(&conversation, &node.identity.public()),
            "same signing key with a changed transport key cannot acknowledge cleanup for this device"
        );
        assert!(node.personal_source_applied(&conversation, &other.identity.public()));
    }

    #[test]
    fn personal_cleanup_failure_retries_the_same_floor_and_rejects_late_cut() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let node = super::super::tests::node(dir.path(), &account);
        let peer = "a".repeat(32);
        let conversation = DocumentConversation::Account(peer.clone());
        let conv = crate::node::conversation::account_conversation_id(&account.account_id(), &peer);
        node.received
            .lock()
            .unwrap()
            .record(
                conv,
                "b".repeat(32),
                1,
                b"old local row",
                EventId::new([6; 32]),
            )
            .unwrap();
        let draft = AccountDocumentKey::Draft {
            conversation: conversation.clone(),
        };
        node.put_account_document_if(draft.clone(), b"old draft".to_vec(), None, |commit| {
            commit()
        })
        .unwrap();
        let received = dir.path().join("received.log");
        let saved = dir.path().join("received-saved.log");
        std::fs::rename(&received, &saved).unwrap();
        std::fs::create_dir(&received).unwrap();
        assert!(
            node.delete_account_contact_if(&account.account_id(), &peer, true, |commit| commit())
                .is_err()
        );
        let floor = node
            .conversation_personal_state(&conversation)
            .floor
            .unwrap();
        let cut = node.capture_personal_history_cut(&node.identity.public());
        assert!(
            !node
                .conversation_personal_state(&conversation)
                .local_cleanup_complete
        );
        assert!(!cut.allows(&node, &conversation));
        std::fs::remove_dir(&received).unwrap();
        std::fs::rename(&saved, &received).unwrap();
        node.delete_account_contact_if(&account.account_id(), &peer, true, |commit| commit())
            .unwrap();
        assert_eq!(
            node.conversation_personal_state(&conversation).floor,
            Some(floor)
        );
        assert!(
            node.conversation_personal_state(&conversation)
                .local_cleanup_complete
        );
        assert!(node.received.lock().unwrap().entries(&conv).is_empty());
        assert!(node.account_document(&draft).unwrap().is_none());
        assert!(
            !cut.allows(&node, &conversation),
            "a late acknowledgment cannot authorize an already started page"
        );
        assert!(
            node.capture_personal_history_cut(&node.identity.public())
                .allows(&node, &conversation)
        );
        let identity = node.identity.secret_bytes();
        drop(node);
        let reopened = super::super::tests::node_with_identity(
            dir.path(),
            &account,
            DeviceIdentity::from_secret_bytes(identity.0, identity.1),
        );
        assert!(
            reopened
                .conversation_personal_state(&conversation)
                .local_cleanup_complete
        );
        assert!(reopened.received.lock().unwrap().entries(&conv).is_empty());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn personal_dm_keep_clear_and_authenticated_new_message_preserve_the_floor() {
        use std::net::IpAddr;
        let dir = tempfile::tempdir().unwrap();
        let a_account = Account::generate();
        let b_account = Account::generate();
        let a = super::super::tests::node(&dir.path().join("a"), &a_account);
        let b = super::super::tests::node(&dir.path().join("b"), &b_account);
        let al = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        for (node, peer, account, port) in [
            (&a, &b, &b_account, bl.local_addr().unwrap().port()),
            (&b, &a, &a_account, al.local_addr().unwrap().port()),
        ] {
            node.roster.lock().unwrap().update(
                &crate::discovery::Announce::new_with_account(
                    &peer.identity,
                    account,
                    "peer",
                    port,
                ),
                IpAddr::from([127, 0, 0, 1]),
                &node.user_id(),
            );
        }
        let aserver = tokio::spawn(a.clone().run_accept_loop(al));
        let bserver = tokio::spawn(b.clone().run_accept_loop(bl));
        let conversation = DocumentConversation::Account(a.account_id());
        let conv =
            crate::node::conversation::account_conversation_id(&a.account_id(), &b.account_id());
        let aa = a.clone();
        let bb = b.clone();
        let outcome = async move {
            let a = aa;
            let b = bb;
            a.send_to_account(&b.account_id(), b"old live row", None)
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(4), async {
                while b.received.lock().unwrap().entries(&conv).is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            b.delete_account_contact_if(&b.account_id(), &a.account_id(), false, |commit| commit())
                .unwrap();
            assert!(b.conversation_personal_state(&conversation).hidden);
            assert_eq!(b.received.lock().unwrap().entries(&conv).len(), 1);
            // Presence and a certified route alone cannot undo personal removal.
            assert!(b.conversation_personal_state(&conversation).hidden);
            a.send_to_account(&b.account_id(), b"new keep row", None)
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(4), async {
                while b.received.lock().unwrap().entries(&conv).len() < 2 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert!(!b.conversation_personal_state(&conversation).hidden);
            b.delete_account_contact_if(&b.account_id(), &a.account_id(), true, |commit| commit())
                .unwrap();
            let floor = b.conversation_personal_state(&conversation).floor.unwrap();
            assert!(b.received.lock().unwrap().entries(&conv).is_empty());
            a.send_to_account(&b.account_id(), b"new clear row", None)
                .await
                .unwrap();
            tokio::time::timeout(std::time::Duration::from_secs(4), async {
                while b.received.lock().unwrap().entries(&conv).is_empty() {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            assert_eq!(b.received.lock().unwrap().entries(&conv).len(), 1);
            let state = b.conversation_personal_state(&conversation);
            assert!(!state.hidden && state.local_cleanup_complete);
            assert_eq!(state.floor, Some(floor));
        };
        // Drain all owners before propagating an assertion failure.
        let result = tokio::spawn(outcome).await;
        aserver.abort();
        bserver.abort();
        let _ = aserver.await;
        let _ = bserver.await;
        a.runtime_work.drain().await;
        b.runtime_work.drain().await;
        result.unwrap();
    }

    #[test]
    fn personal_controls_never_enter_the_frozen_v5_group_scope() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let node = super::super::tests::node(dir.path(), &account);
        node.delete_account_contact_if(&account.account_id(), &"a".repeat(32), false, |commit| {
            commit()
        })
        .unwrap();
        let store = node.account_documents.lock().unwrap();
        assert!(
            store
                .list_operation_scope(AccountDocumentDomain::GroupControl, None, 64, true, true)
                .iter()
                .all(|meta| !matches!(meta.key, AccountDocumentKey::ConversationPersonal { .. })),
            "new personal controls require their explicitly negotiated scope"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn personal_visibility_uses_negotiated_v7_over_certified_noise() {
        use crate::node::{AccountDevice, account_sync_wire::AccountHistoryFrame};
        use std::{
            net::IpAddr,
            sync::{Arc, Mutex},
        };
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let source = super::super::tests::node(&dir.path().join("source"), &account);
        let target = super::super::tests::node(&dir.path().join("target"), &account);
        for (a, b) in [(&source, &target), (&target, &source)] {
            a.account_devices
                .lock()
                .unwrap()
                .insert(AccountDevice {
                    public: b.identity.public(),
                    certificate: account.certify(&b.identity.public().ed25519_pub),
                })
                .unwrap();
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        target.roster.lock().unwrap().update(
            &crate::discovery::Announce::new_with_account(
                &source.identity,
                &account,
                "source",
                addr.port(),
            ),
            IpAddr::from([127, 0, 0, 1]),
            &target.user_id(),
        );
        let versions = Arc::new(Mutex::new(Vec::new()));
        let observed = versions.clone();
        *source.account_session_test_hook.lock().unwrap() = Some(Arc::new(move |_, frame| {
            if let AccountHistoryFrame::DocumentsRequest { body } = frame
                && let Ok(value) = serde_json::from_slice::<serde_json::Value>(&body)
                && value["domain"] == "group_control"
            {
                observed
                    .lock()
                    .unwrap()
                    .push(value["version"].as_u64().unwrap());
            }
            Box::pin(async {})
        }));
        source
            .delete_account_contact_if(&account.account_id(), &"a".repeat(32), false, |commit| {
                commit()
            })
            .unwrap();
        let server = tokio::spawn(source.clone().run_accept_loop(listener));
        let peer = target.routing_peer(&source.user_id()).unwrap();
        let mut cursor = super::super::sync::DocumentSyncCursor::control();
        for _ in 0..4 {
            target
                .pull_account_documents_page(&peer, &mut cursor)
                .await
                .unwrap();
            if target
                .conversation_personal_state(&DocumentConversation::Account("a".repeat(32)))
                .known
            {
                break;
            }
        }
        let state =
            target.conversation_personal_state(&DocumentConversation::Account("a".repeat(32)));
        let used_v7 = versions.lock().unwrap().contains(&7);
        server.abort();
        let _ = server.await;
        source.runtime_work.drain().await;
        assert!(state.known && state.hidden && state.local_cleanup_complete);
        assert!(
            used_v7,
            "personal controls must not ride an unnegotiated v5 response"
        );
    }

    #[tokio::test]
    async fn group_cleanup_waits_for_committed_leave_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let owner = super::super::tests::node(&dir.path().join("owner"), &Account::generate());
        let member_path = dir.path().join("member");
        let member = super::super::tests::node(&member_path, &account);
        let identity = member.identity.secret_bytes();
        let group = new_channel_id();
        let genesis = Event::new(
            &owner.identity,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Personal cleanup".into(),
                members: vec![owner.identity.public(), member.identity.public()],
                epoch: 0,
                owner: owner.user_id(),
            }
            .encode(),
        );
        member.log.lock().unwrap().append_durable(genesis).unwrap();
        member.process_channel(group);
        super::super::tests::settle_group_metadata(&member, group);
        member
            .received
            .lock()
            .unwrap()
            .record(
                group,
                owner.user_id(),
                1,
                b"retained local history",
                EventId::new([9; 32]),
            )
            .unwrap();
        let id = member
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        member
            .submit_account_group_leave_if(&account.account_id(), id, group, Some(true), |commit| {
                commit()
            })
            .unwrap();
        assert_eq!(member.account_group_leave_cleanup(id), Some(true));
        assert_eq!(
            member.account_group_access_with_closed(group).unwrap().role,
            AccountGroupRole::Member
        );
        assert!(
            !member
                .conversation_personal_state(&DocumentConversation::Channel(hex::encode(
                    group.as_bytes()
                )))
                .hidden,
            "pending intent must never erase or hide the conversation"
        );
        assert!(!member.history(group, 10).unwrap().is_empty());
        for _ in 0..8 {
            member.maintain_account_group_operation_effects();
            super::super::tests::settle_group_metadata(&member, group);
            member.retry_conversation_personal_cleanup().unwrap();
        }
        assert_eq!(
            member
                .account_group_operation_status(&account.account_id(), id)
                .unwrap()
                .unwrap()
                .phase,
            AccountGroupOperationPhase::Committed
        );
        assert!(member.history(group, 10).unwrap().is_empty());
        assert!(
            member
                .conversation_personal_state(&DocumentConversation::Channel(hex::encode(
                    group.as_bytes()
                )))
                .local_cleanup_complete
        );
        drop(member);
        let reopened = super::super::tests::node_with_identity(
            &member_path,
            &account,
            DeviceIdentity::from_secret_bytes(identity.0, identity.1),
        );
        assert!(
            reopened
                .conversation_personal_state(&DocumentConversation::Channel(hex::encode(
                    group.as_bytes()
                )))
                .hidden
        );
        super::super::tests::settle_group_metadata(&reopened, group);
        assert!(reopened.history(group, 10).unwrap().is_empty());
    }
}
