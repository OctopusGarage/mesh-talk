//! Private, typed account documents shared only with certified siblings.
use super::{Node, NodeError, SyncCategory};
use crate::identity::{account::Account, device::DeviceIdentity};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;
mod controls;
mod custody;
mod device_hostname;
mod group_activity;
mod group_control;
mod group_legacy_operations;
mod group_operations;
mod inventory;
pub(in crate::node) mod personal;
pub use personal::ConversationPersonalState;
mod public_profile;
pub use inventory::{GroupInventoryMember, GroupInventoryReference};
pub(super) mod store;
pub(super) mod sync;
#[cfg(test)]
mod tests;
pub(super) mod wire;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccountDocumentDomain {
    Draft,
    ReadReceipt,
    CustomizationPack,
    AvatarOverride,
    DmReaction,
    DmRecall,
    ChannelCustody,
    GroupInventory,
    DeviceHostname,
    GroupControl,
    PublicProfile,
    GroupActivity,
}
impl AccountDocumentDomain {
    /// Hostname and group-control domains independently shipped digest byte8.
    /// Preserve those opaque, explicitly domain-scoped hints independently of
    /// their distinct in-memory cursor/index positions after merging the domains.
    fn digest_discriminator(self) -> u8 {
        match self {
            Self::DeviceHostname | Self::GroupControl => 8,
            _ => self as u8,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum DocumentConversation {
    Account(String),
    Channel(String),
}
#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AccountDocumentKey {
    Draft {
        conversation: DocumentConversation,
    },
    ReadReceipt {
        conversation: DocumentConversation,
        message_id: String,
    },
    CustomizationPack {
        pack_id: String,
    },
    AvatarOverride {
        target: DocumentConversation,
    },
    DmReaction {
        peer_account: String,
        target: String,
        actor_account: String,
        emoji: String,
    },
    DmRecall {
        peer_account: String,
        target: String,
        actor_account: String,
    },
    ChannelCustody {
        channel: String,
        event: String,
        holder: String,
    },
    GroupInventory {
        channel: String,
        source_device: String,
    },
    DeviceHostname {
        device_id: String,
    },
    GroupControl {
        channel: String,
        source_device: String,
    },
    GroupOperation {
        channel: String,
        source_device: String,
    },
    PublicProfile {
        account: String,
        source_device: String,
    },
    GroupActivity {
        channel: String,
        source_device: String,
    },
    GroupLegacyOperation {
        channel: String,
        source_device: String,
    },
    ConversationPersonal {
        conversation: DocumentConversation,
        action_id: String,
    },
}
impl DocumentConversation {
    fn valid(&self) -> bool {
        match self {
            Self::Account(id) => hex(id, 32),
            Self::Channel(id) => hex(id, 64),
        }
    }
}
fn hex(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl AccountDocumentKey {
    pub fn domain(&self) -> AccountDocumentDomain {
        match self {
            Self::ConversationPersonal { .. } => AccountDocumentDomain::GroupControl,
            Self::PublicProfile { .. } => AccountDocumentDomain::PublicProfile,
            Self::GroupActivity { .. } => AccountDocumentDomain::GroupActivity,
            Self::DeviceHostname { .. } => AccountDocumentDomain::DeviceHostname,
            Self::Draft { .. } => AccountDocumentDomain::Draft,
            Self::ReadReceipt { .. } => AccountDocumentDomain::ReadReceipt,
            Self::CustomizationPack { .. } => AccountDocumentDomain::CustomizationPack,
            Self::AvatarOverride { .. } => AccountDocumentDomain::AvatarOverride,
            Self::DmReaction { .. } => AccountDocumentDomain::DmReaction,
            Self::DmRecall { .. } => AccountDocumentDomain::DmRecall,
            Self::ChannelCustody { .. } => AccountDocumentDomain::ChannelCustody,
            Self::GroupInventory { .. } => AccountDocumentDomain::GroupInventory,
            Self::GroupControl { .. }
            | Self::GroupOperation { .. }
            | Self::GroupLegacyOperation { .. } => AccountDocumentDomain::GroupControl,
        }
    }
    fn valid(&self) -> bool {
        match self {
            Self::ConversationPersonal {
                conversation,
                action_id,
            } => conversation.valid() && hex(action_id, 64),
            Self::PublicProfile {
                account,
                source_device,
            } => hex(account, 32) && hex(source_device, 32),
            Self::GroupActivity {
                channel,
                source_device,
            } => hex(channel, 64) && hex(source_device, 32),
            Self::DeviceHostname { device_id } => hex(device_id, 32),
            Self::GroupInventory {
                channel,
                source_device,
            }
            | Self::GroupControl {
                channel,
                source_device,
            }
            | Self::GroupOperation {
                channel,
                source_device,
            }
            | Self::GroupLegacyOperation {
                channel,
                source_device,
            } => hex(channel, 64) && hex(source_device, 32),
            Self::ChannelCustody {
                channel,
                event,
                holder,
            } => {
                hex(channel, 64)
                    && hex(event, 64)
                    && hex(holder.strip_prefix("relay:").unwrap_or(holder), 32)
            }
            Self::Draft { conversation } => conversation.valid(),
            Self::ReadReceipt {
                conversation,
                message_id,
            } => {
                conversation.valid()
                    && !message_id.is_empty()
                    && message_id.len() <= 200
                    && !message_id.chars().any(char::is_control)
            }
            Self::CustomizationPack { pack_id } => {
                (1..=80).contains(&pack_id.len())
                    && pack_id.bytes().all(|b| {
                        b.is_ascii_lowercase() || b.is_ascii_digit() || b"._-".contains(&b)
                    })
            }
            Self::AvatarOverride { target } => target.valid(),
            Self::DmReaction {
                peer_account,
                target,
                actor_account,
                emoji,
            } => {
                hex(peer_account, 32)
                    && hex(target, 64)
                    && hex(actor_account, 32)
                    && !emoji.is_empty()
                    && emoji.len() <= 256
                    && !emoji.chars().any(char::is_control)
            }
            Self::DmRecall {
                peer_account,
                target,
                actor_account,
            } => hex(peer_account, 32) && hex(target, 64) && hex(actor_account, 32),
        }
    }
    fn limit(&self) -> usize {
        match self {
            Self::ConversationPersonal { .. } => 64 * 1024,
            Self::PublicProfile { .. } => 1024 * 1024,
            Self::GroupActivity { .. } => 8 * 1024,
            Self::DeviceHostname { .. } => 255,
            Self::GroupInventory { .. } => 128 * 1024,
            Self::GroupControl { .. } => super::account_group::wire::MAX_HEAD_BYTES,
            Self::GroupOperation { .. } | Self::GroupLegacyOperation { .. } => 4 * 1024 * 1024,
            Self::ChannelCustody { .. } => 4096,
            Self::Draft { .. } => 64 * 1024,
            Self::ReadReceipt { .. } => 0,
            Self::CustomizationPack { .. } => 12 * 1024 * 1024,
            Self::AvatarOverride { .. } => 1024 * 1024,
            Self::DmReaction { .. } | Self::DmRecall { .. } => 16 * 1024,
        }
    }
    pub fn is_private_control(&self) -> bool {
        matches!(
            self,
            Self::ConversationPersonal { .. }
                | Self::PublicProfile { .. }
                | Self::GroupActivity { .. }
                | Self::DmReaction { .. }
                | Self::DmRecall { .. }
                | Self::ChannelCustody { .. }
                | Self::GroupInventory { .. }
                | Self::GroupControl { .. }
                | Self::GroupOperation { .. }
                | Self::GroupLegacyOperation { .. }
                | Self::DeviceHostname { .. }
        )
    }
    fn protocol_version(&self) -> u8 {
        match self {
            Self::ConversationPersonal { .. } => 7,
            Self::PublicProfile { .. } | Self::GroupActivity { .. } => 6,
            Self::DeviceHostname { .. } => 5,
            Self::GroupInventory { .. } => 4,
            Self::GroupControl { .. }
            | Self::GroupOperation { .. }
            | Self::GroupLegacyOperation { .. } => 5,
            Self::ChannelCustody { .. } => 3,
            Self::DmReaction { .. } | Self::DmRecall { .. } => 2,
            _ => 1,
        }
    }
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocumentVersion {
    pub revision: u64,
    pub writer_device: String,
    pub content_hash: [u8; 32],
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AccountDocumentMeta {
    pub key: AccountDocumentKey,
    pub revision: u64,
    pub writer_device: String,
    pub content_hash: [u8; 32],
    pub content_len: u32,
    pub tombstone: bool,
    pub(crate) account_public: [u8; 32],
    version: u8,
    signature: Vec<u8>,
}
impl AccountDocumentMeta {
    pub fn version(&self) -> DocumentVersion {
        DocumentVersion {
            revision: self.revision,
            writer_device: self.writer_device.clone(),
            content_hash: self.content_hash,
        }
    }
    fn message(&self) -> Vec<u8> {
        let mut bytes = b"mesh-talk-account-document-v1".to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                self.version,
                self.account_public,
                &self.key,
                self.revision,
                &self.writer_device,
                self.content_hash,
                self.content_len,
                self.tombstone,
            ))
            .expect("document metadata serializes"),
        );
        bytes
    }
    fn valid(&self, account: &[u8; 32]) -> bool {
        self.version == self.key.protocol_version()
            && self.account_public == *account
            && self.key.valid()
            && self.revision > 0
            && self.revision <= 9_007_199_254_740_991
            && hex(&self.writer_device, 32)
            && (!matches!(&self.key, AccountDocumentKey::PublicProfile { source_device, .. } | AccountDocumentKey::GroupActivity { source_device, .. } if source_device != &self.writer_device))
            && (!matches!(&self.key, AccountDocumentKey::GroupInventory { source_device, .. } if source_device != &self.writer_device))
            && (!matches!(&self.key, AccountDocumentKey::GroupControl { source_device, .. } if source_device != &self.writer_device))
            && (!matches!(&self.key, AccountDocumentKey::GroupLegacyOperation { source_device, .. } if source_device != &self.writer_device))
            && (!matches!(&self.key, AccountDocumentKey::DeviceHostname { device_id } if device_id != &self.writer_device))
            && self.content_len as usize <= self.key.limit()
            && (self.content_len != 0
                || self.content_hash
                    == if self.tombstone {
                        [0; 32]
                    } else {
                        content_hash(&[])
                    })
            && (!self.tombstone || (self.content_len == 0 && self.content_hash == [0; 32]))
            && (!matches!(self.key, AccountDocumentKey::ReadReceipt { .. })
                || (!self.tombstone
                    && self.content_len == 0
                    && self.content_hash == content_hash(&[])))
            && <[u8; 64]>::try_from(self.signature.as_slice())
                .is_ok_and(|s| DeviceIdentity::verify(account, &self.message(), &s))
    }
    fn new(
        account: &Account,
        writer: String,
        key: AccountDocumentKey,
        revision: u64,
        payload: &[u8],
        tombstone: bool,
    ) -> Self {
        let mut meta = Self {
            version: key.protocol_version(),
            account_public: account.public().ed25519_pub,
            key,
            revision,
            writer_device: writer,
            content_hash: if tombstone {
                [0; 32]
            } else {
                content_hash(payload)
            },
            content_len: payload.len() as u32,
            tombstone,
            signature: Vec::new(),
        };
        meta.signature = account.sign(&meta.message()).to_vec();
        meta
    }
    fn order(&self) -> (u64, &str, &[u8; 32]) {
        (self.revision, &self.writer_device, &self.content_hash)
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct AccountDocumentPage {
    pub items: Vec<AccountDocumentMeta>,
    pub next: Option<AccountDocumentKey>,
    pub digest: [u8; 32],
}
pub struct AccountDocument {
    pub meta: AccountDocumentMeta,
    payload: Vec<u8>,
}
impl AccountDocument {
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }
}
impl Drop for AccountDocument {
    fn drop(&mut self) {
        use zeroize::Zeroize;
        self.payload.zeroize();
    }
}
impl fmt::Debug for AccountDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AccountDocument")
            .field("meta", &self.meta)
            .field("payload", &"[REDACTED]")
            .finish()
    }
}
pub(in crate::node) fn content_hash(payload: &[u8]) -> [u8; 32] {
    let mut h = Sha256::new();
    h.update(b"mesh-talk-account-document-content-v1");
    h.update(payload);
    h.finalize().into()
}
fn invalid() -> NodeError {
    NodeError::InvalidInput("invalid account document".into())
}
impl Node {
    pub fn account_document_meta(&self, key: &AccountDocumentKey) -> Option<AccountDocumentMeta> {
        self.account_documents
            .lock()
            .expect("documents mutex")
            .meta(key)
    }
    pub fn account_document(
        &self,
        key: &AccountDocumentKey,
    ) -> Result<Option<AccountDocument>, NodeError> {
        self.account_documents
            .lock()
            .expect("documents mutex")
            .get(key)
            .map_err(NodeError::Log)
    }
    pub fn account_documents(
        &self,
        domain: AccountDocumentDomain,
        after: Option<&AccountDocumentKey>,
        limit: usize,
    ) -> Result<AccountDocumentPage, NodeError> {
        if limit == 0 || limit > 64 || after.is_some_and(|k| !k.valid() || k.domain() != domain) {
            return Err(invalid());
        }
        let store = self.account_documents.lock().expect("documents mutex");
        let mut items = store.list(domain, after, 64);
        let next = if items.len() > limit {
            items.truncate(limit);
            items.last().map(|m| m.key.clone())
        } else if items.len() == limit {
            let key = &items.last().unwrap().key;
            (!store.list(domain, Some(key), 1).is_empty()).then(|| key.clone())
        } else {
            None
        };
        Ok(AccountDocumentPage {
            items,
            next,
            digest: store.digest(domain),
        })
    }
    pub fn subscribe_account_documents(&self) -> tokio::sync::watch::Receiver<u64> {
        self.account_document_updates.subscribe()
    }
    fn documents_changed(&self) {
        self.documents_imported();
        self.note_account_local_change();
    }
    /// Incoming rows dirty local readers and reconciliation without claiming a
    /// new local publication. Relaying a sibling import as a publisher wake
    /// would turn mandatory metadata reconciliation into a feedback loop.
    fn documents_imported(&self) {
        for status in self
            .account_sync_status
            .lock()
            .expect("sync status mutex")
            .values_mut()
        {
            if status.document_phase == super::SyncPhase::UpToDate {
                status.document_phase = super::SyncPhase::CatchingUp;
            }
        }
        self.account_document_updates
            .send_modify(|r| *r = r.wrapping_add(1));
        self.account_sync_notify.notify_one();
        self.note_ui_change();
    }
    pub fn put_account_document_if(
        &self,
        key: AccountDocumentKey,
        payload: Vec<u8>,
        expected: Option<DocumentVersion>,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<AccountDocumentMeta, NodeError> {
        if key.is_private_control() {
            return Err(invalid());
        }
        let payload = zeroize::Zeroizing::new(payload);
        let mut result = None;
        authorize(&mut || {
            let _membership = matches!(
                &key,
                AccountDocumentKey::Draft {
                    conversation: DocumentConversation::Channel(_)
                }
            )
            .then(|| {
                self.channel_membership_commit
                    .lock()
                    .expect("membership commit mutex")
            });
            let draft_source = match &key {
                AccountDocumentKey::Draft {
                    conversation: DocumentConversation::Channel(id),
                } => hex::decode(id)
                    .ok()
                    .and_then(|b| <[u8; 32]>::try_from(b).ok())
                    .and_then(|id| {
                        self.account_group_access_with_closed(crate::eventlog::ConversationId::new(
                            id,
                        ))
                        .ok()
                    })
                    .and_then(|access| {
                        access.sources.into_iter().find(|source| {
                            source.device == self.identity.public()
                                && matches!(
                                    source.role,
                                    super::account_group::AccountGroupRole::Owner
                                        | super::account_group::AccountGroupRole::Member
                                )
                        })
                    }),
                _ => None,
            };
            let _cut = self
                .account_sync_commit
                .lock()
                .expect("account sync commit mutex");
            if let AccountDocumentKey::Draft { conversation } = &key {
                let state = self.conversation_personal_state(conversation);
                if state.hidden || (!state.local_cleanup_complete && draft_source.is_none()) {
                    return Err(NodeError::Authorization(
                        "conversation cleanup or explicit reopen is required".into(),
                    ));
                }
            }
            let mut store = self.account_documents.lock().expect("documents mutex");
            result = Some(if let Some(source) = &draft_source {
                store.put_local_group_draft(
                    &self.account,
                    &self.identity.public(),
                    key.clone(),
                    &payload,
                    expected.as_ref(),
                    &source.lifetime,
                )?
            } else {
                store.put(
                    &self.account,
                    self.identity.public().user_id(),
                    key.clone(),
                    &payload,
                    expected.as_ref(),
                    false,
                )?
            });
            Ok(())
        })?;
        let result = result.ok_or_else(invalid)?;
        self.documents_changed();
        Ok(result)
    }
    pub fn remove_account_document_if(
        &self,
        key: AccountDocumentKey,
        expected: Option<DocumentVersion>,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<AccountDocumentMeta, NodeError> {
        if key.is_private_control() {
            return Err(invalid());
        }
        let mut result = None;
        authorize(&mut || {
            let mut store = self.account_documents.lock().expect("documents mutex");
            result = Some(store.put(
                &self.account,
                self.identity.public().user_id(),
                key.clone(),
                &[],
                expected.as_ref(),
                true,
            )?);
            Ok(())
        })?;
        let result = result.ok_or_else(invalid)?;
        self.documents_changed();
        Ok(result)
    }
    pub fn mark_account_message_seen_if(
        &self,
        conversation: DocumentConversation,
        message_id: String,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<AccountDocumentMeta, NodeError> {
        let key = AccountDocumentKey::ReadReceipt {
            conversation,
            message_id,
        };
        let mut result = None;
        let mut changed = false;
        authorize(&mut || {
            let mut store = self.account_documents.lock().expect("documents mutex");
            result = Some(if let Some(old) = store.meta(&key) {
                old
            } else {
                changed = true;
                store.put(
                    &self.account,
                    self.identity.public().user_id(),
                    key.clone(),
                    &[],
                    None,
                    false,
                )?
            });
            Ok(())
        })?;
        let result = result.ok_or_else(invalid)?;
        if changed {
            self.documents_changed();
        }
        Ok(result)
    }
}

pub(in crate::node) use store::MAX_LOCAL_FRONTIER;

pub(in crate::node) use inventory::MAX_DIRECTORY_IDS;
