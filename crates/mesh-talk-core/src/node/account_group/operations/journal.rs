//! Durable account intent and source replay indexes. Public effects live only in DeliveryStore.
use super::*;
use crate::{
    discovery::Announce,
    eventlog::LogError,
    identity::device::{DeviceIdentity, PublicIdentity},
    storage::record_log::EncryptedRecordLog,
};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path};

const MAX_ENTRIES: usize = 100_000;
const MAX_LIVE: usize = 64 * 1024 * 1024;
const MAX_PENDING: usize = 128;
const DOMAIN: &[u8] = b"mesh-talk-account-group-operation-request-v1";

#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct OperationReference {
    pub id: AccountGroupOperationId,
    pub group: ConversationId,
    pub kind: AccountGroupOperationKind,
    pub digest: [u8; 32],
    pub origin: Announce,
    pub executor: AccountGroupSource,
    pub signature: Vec<u8>,
}
impl OperationReference {
    pub(in crate::node) fn message(&self) -> Vec<u8> {
        let mut bytes = DOMAIN.to_vec();
        bytes.extend(
            serde_json::to_vec(&(
                self.id,
                self.group,
                self.kind,
                self.digest,
                &self.origin,
                &self.executor,
            ))
            .expect("operation reference serializes"),
        );
        bytes
    }
    pub(in crate::node) fn new(
        node: &Node,
        id: AccountGroupOperationId,
        group: ConversationId,
        operation: &AccountGroupOperation,
        executor: AccountGroupSource,
    ) -> Self {
        let mut reference = Self {
            id,
            group,
            kind: operation.kind(),
            digest: operation_digest(group, operation),
            origin: node.own_group_proof(),
            executor,
            signature: Vec::new(),
        };
        reference.signature = node.identity.sign(&reference.message()).to_vec();
        reference
    }
    pub(in crate::node) fn valid(&self, account: &[u8; 32]) -> bool {
        let Ok(signature) = <[u8; 64]>::try_from(self.signature.as_slice()) else {
            return false;
        };
        self.id.as_bytes() != &[0; 32]
            && self.id.matches_origin(&self.origin.public())
            && self.origin.verify()
            && self
                .origin
                .account_cert
                .as_ref()
                .is_some_and(|cert| &cert.account_ed25519_pub == account)
            && DeviceIdentity::verify(&self.origin.ed25519_pub, &self.message(), &signature)
    }
    pub(super) fn same(&self, other: &Self) -> bool {
        self.message() == other.message() && self.signature == other.signature
    }
}
pub(in crate::node) fn operation_digest(
    group: ConversationId,
    operation: &AccountGroupOperation,
) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"mesh-talk-account-group-operation-body-v1");
    hash.update(group.as_bytes());
    hash.update(serde_json::to_vec(operation).expect("operation body serializes"));
    hash.finalize().into()
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    pub reference: OperationReference,
    pub body: Option<AccountGroupOperation>,
    pub status: AccountGroupOperationStatus,
    pub source: bool,
    pub cancel: bool,
    pub sequence: u64,
}
impl Entry {
    pub fn terminal(&self) -> bool {
        matches!(
            self.status.phase,
            AccountGroupOperationPhase::Committed
                | AccountGroupOperationPhase::Cancelled
                | AccountGroupOperationPhase::Rejected { .. }
        )
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Lookup {
    pub id: AccountGroupOperationId,
    pub group: ConversationId,
    pub body: AccountGroupOperation,
    pub digest: [u8; 32],
    pub status: AccountGroupOperationStatus,
    pub cancel: bool,
    pub sequence: u64,
}
#[derive(Serialize, Deserialize)]
struct Record {
    account: [u8; 32],
    device: PublicIdentity,
    entry: Option<Entry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lookup: Option<Lookup>,
}
pub(in crate::node) struct OperationJournal {
    file: EncryptedRecordLog<Vec<u8>>,
    pub(super) entries: BTreeMap<AccountGroupOperationId, Entry>,
    pub(super) lookups: BTreeMap<AccountGroupOperationId, Lookup>,
    account: [u8; 32],
    device: PublicIdentity,
    sequence: u64,
    // Derived from retained source terminal records; never a second job journal.
    fanout_groups: BTreeMap<[u8; 32], u64>,
    fanout_order: BTreeMap<u64, [u8; 32]>,
    records: usize,
    bytes: usize,
    live: usize,
    #[cfg(test)]
    live_limit: usize,
}
fn invalid() -> LogError {
    LogError::CorruptFile("invalid account group operation journal".into())
}
impl OperationJournal {
    pub(in crate::node) fn open(
        dir: &Path,
        password: &str,
        account: [u8; 32],
        device: PublicIdentity,
    ) -> Result<Self, LogError> {
        let account_id = crate::identity::account::AccountPublic {
            ed25519_pub: account,
        }
        .account_id();
        let identity_hash =
            Sha256::digest([device.ed25519_pub.as_slice(), device.x25519_pub.as_slice()].concat());
        let source = hex::encode(&identity_hash[..16]);
        let path = dir.join(format!(
            "account-group-operations-{account_id}-{source}.log"
        ));
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if !meta.is_file() || meta.len() > 128 * 1024 * 1024 => return Err(invalid()),
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(LogError::Io(error)),
        }
        // The node storage password is stable across login-credential updates.
        let scoped = zeroize::Zeroizing::new(format!(
            "mesh-talk-group-operations-v1:{account_id}:{source}:{password}"
        ));
        let (file, rows): (_, Vec<Vec<u8>>) = EncryptedRecordLog::open(&path, &scoped, b"MTGOP1")?;
        let mut journal = Self {
            file,
            entries: BTreeMap::new(),
            lookups: BTreeMap::new(),
            account,
            device,
            sequence: 0,
            fanout_groups: BTreeMap::new(),
            fanout_order: BTreeMap::new(),
            records: rows.len(),
            bytes: 22,
            live: 0,
            #[cfg(test)]
            live_limit: MAX_LIVE,
        };
        for bytes in rows {
            journal.bytes += bytes.len() + 40;
            let row: Record = serde_json::from_slice(&bytes).map_err(|_| invalid())?;
            if row.account != journal.account || row.device != journal.device {
                return Err(invalid());
            }
            match (row.entry, row.lookup) {
                (Some(entry), None) => {
                    journal.validate(&entry)?;
                    if journal
                        .entries
                        .get(&entry.reference.id)
                        .is_none_or(|old| old.sequence < entry.sequence)
                    {
                        journal.install(entry)?;
                    }
                }
                (None, Some(lookup)) => {
                    journal.validate_lookup(&lookup)?;
                    if journal
                        .entries
                        .get(&lookup.id)
                        .is_none_or(|entry| entry.sequence < lookup.sequence)
                        && journal
                            .lookups
                            .get(&lookup.id)
                            .is_none_or(|old| old.sequence < lookup.sequence)
                    {
                        journal.install_lookup(lookup)?;
                    }
                }
                _ => return Err(invalid()),
            }
        }
        Ok(journal)
    }
    fn row(&self, entry: Entry) -> Record {
        Record {
            account: self.account,
            device: self.device.clone(),
            entry: Some(entry),
            lookup: None,
        }
    }
    fn size(&self, entry: &Entry) -> Result<usize, LogError> {
        let actual = serde_json::to_vec(&self.row(entry.clone()))
            .map_err(|_| invalid())?
            .len()
            + 40;
        if entry.terminal() {
            return Ok(actual);
        }
        let mut reserved = entry.clone();
        reserved.body = None;
        reserved.source = false;
        reserved.sequence = u64::MAX;
        let (legs, effects) = match entry.reference.kind {
            AccountGroupOperationKind::LeaveAccount | AccountGroupOperationKind::Reaction => {
                (65, 65)
            }
            AccountGroupOperationKind::Rename | AccountGroupOperationKind::Recall => (1, 1),
            _ => (1, 8),
        };
        let phase = AccountGroupOperationPhase::ActionRequired {
            reason: AccountGroupOperationRejection::OriginalSignerNoLongerAuthorized,
        };
        let event = EventId::new([255; 32]);
        reserved.status.phase = phase.clone();
        reserved.status.primary_event = Some(event);
        reserved.status.committed_events = vec![event; effects];
        reserved.status.executions = (0..legs)
            .map(|_| AccountGroupOperationExecution {
                device_id: "f".repeat(32),
                phase: phase.clone(),
                committed_events: vec![event; if legs == 1 { effects } else { 1 }],
            })
            .collect();
        let bound = serde_json::to_vec(&self.row(reserved))
            .map_err(|_| invalid())?
            .len()
            + 40;
        Ok(actual.max(bound))
    }
    fn live_budget(&self) -> usize {
        #[cfg(test)]
        {
            self.live_limit.min(MAX_LIVE)
        }
        #[cfg(not(test))]
        {
            MAX_LIVE
        }
    }
    fn validate(&self, entry: &Entry) -> Result<(), LogError> {
        let reference = &entry.reference;
        let account_id = crate::identity::account::AccountPublic {
            ed25519_pub: self.account,
        }
        .account_id();
        if !reference.valid(&self.account)
            || entry.status.account_id != account_id
            || entry.status.id != reference.id
            || entry.status.group != reference.group
            || entry.status.kind != reference.kind
            || entry.body.as_ref().is_some_and(|body| {
                operation_digest(reference.group, body) != reference.digest
                    || body.kind() != reference.kind
            })
            || entry.source && reference.executor.device != self.device
            || entry.status.committed_events.len() > 65
            || entry.status.executions.len() > 65
        {
            return Err(invalid());
        }
        if let Some(old) = self.entries.get(&reference.id) {
            if !old.reference.same(reference) || old.source && !entry.source {
                return Err(invalid());
            }
            if old.terminal()
                && (entry.status != old.status
                    || entry.body.is_some()
                    || entry.source != old.source
                    || entry.cancel != old.cancel)
            {
                return Err(invalid());
            }
        }
        if matches!(
            entry.status.phase,
            AccountGroupOperationPhase::Cancelled | AccountGroupOperationPhase::Rejected { .. }
        ) && (!entry.status.committed_events.is_empty()
            || entry.status.primary_event.is_some()
            || entry
                .status
                .executions
                .iter()
                .any(|execution| !execution.committed_events.is_empty()))
        {
            return Err(invalid());
        }
        if entry
            .status
            .primary_event
            .is_some_and(|id| !entry.status.committed_events.contains(&id))
            || entry.status.executions.iter().any(|execution| {
                execution.device_id.len() != 32
                    || !execution
                        .device_id
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit())
                    || execution.committed_events.len() > 65
            })
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn install(&mut self, entry: Entry) -> Result<(), LogError> {
        if let Some(old) = self.lookups.remove(&entry.reference.id) {
            self.live -= self.lookup_size(&old)?;
        }
        if let Some(old) = self.entries.get(&entry.reference.id) {
            self.live -= self.size(old)?;
        }
        self.live += self.size(&entry)?;
        self.sequence = self.sequence.max(entry.sequence);
        if entry.source
            && entry.status.phase == AccountGroupOperationPhase::Committed
            && !entry.status.committed_events.is_empty()
        {
            let group = *entry.reference.group.as_bytes();
            if self
                .fanout_groups
                .get(&group)
                .is_none_or(|sequence| *sequence < entry.sequence)
            {
                if let Some(previous) = self.fanout_groups.insert(group, entry.sequence) {
                    self.fanout_order.remove(&previous);
                }
                self.fanout_order.insert(entry.sequence, group);
            }
        }
        self.entries.insert(entry.reference.id, entry);
        Ok(())
    }
    pub(super) fn put(&mut self, mut entry: Entry) -> Result<(), LogError> {
        self.validate(&entry)?;
        let old = self.entries.get(&entry.reference.id);
        let old_lookup = self.lookups.get(&entry.reference.id);
        let pending = self
            .entries
            .values()
            .filter(|entry| !entry.terminal())
            .count()
            + self.lookups.len();
        if old.is_none()
            && old_lookup.is_none()
            && (self.entries.len() + self.lookups.len() >= MAX_ENTRIES
                || !entry.terminal() && pending >= MAX_PENDING)
        {
            return Err(invalid());
        }
        entry.sequence = self.sequence.checked_add(1).ok_or_else(invalid)?;
        if entry.terminal() {
            entry.body = None;
        }
        let next_live = self
            .live
            .saturating_sub(old.map(|old| self.size(old)).transpose()?.unwrap_or(0))
            .saturating_sub(
                old_lookup
                    .map(|old| self.lookup_size(old))
                    .transpose()?
                    .unwrap_or(0),
            )
            .saturating_add(self.size(&entry)?);
        if next_live > self.live_budget() {
            return Err(invalid());
        }
        let bytes = serde_json::to_vec(&self.row(entry.clone())).map_err(|_| invalid())?;
        let size = bytes.len() + 40;
        if self.records
            >= (self.entries.len() + self.lookups.len())
                .saturating_mul(2)
                .saturating_add(32)
            || self.bytes.saturating_add(size) > 120 * 1024 * 1024
        {
            let rows = self.compacted_rows()?;
            self.file.rewrite(&rows)?;
            self.records = rows.len();
            self.bytes = 22 + rows.iter().map(|row| row.len() + 40).sum::<usize>();
        }
        self.file.append_durable(&bytes)?;
        self.records += 1;
        self.bytes += size;
        self.install(entry)
    }
    fn lookup_row(&self, lookup: Lookup) -> Record {
        Record {
            account: self.account,
            device: self.device.clone(),
            entry: None,
            lookup: Some(lookup),
        }
    }
    fn lookup_size(&self, lookup: &Lookup) -> Result<usize, LogError> {
        // Reserve bounded canonical reference and terminal result before admitting
        // a foreign-origin retry. Lookup never creates an executable reference.
        Ok(serde_json::to_vec(&self.lookup_row(lookup.clone()))
            .map_err(|_| invalid())?
            .len()
            + 40
            + 80 * 1024)
    }
    fn validate_lookup(&self, lookup: &Lookup) -> Result<(), LogError> {
        if lookup.id.as_bytes() == &[0; 32]
            || lookup.digest != operation_digest(lookup.group, &lookup.body)
            || lookup.status.id != lookup.id
            || lookup.status.group != lookup.group
            || lookup.status.kind != lookup.body.kind()
            || lookup.status.account_id
                != (crate::identity::account::AccountPublic {
                    ed25519_pub: self.account,
                })
                .account_id()
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn install_lookup(&mut self, lookup: Lookup) -> Result<(), LogError> {
        if let Some(old) = self.lookups.get(&lookup.id) {
            self.live -= self.lookup_size(old)?;
        }
        self.live += self.lookup_size(&lookup)?;
        self.sequence = self.sequence.max(lookup.sequence);
        self.lookups.insert(lookup.id, lookup);
        Ok(())
    }
    fn compacted_rows(&self) -> Result<Vec<Vec<u8>>, LogError> {
        self.entries
            .values()
            .cloned()
            .map(|entry| serde_json::to_vec(&self.row(entry)))
            .chain(
                self.lookups
                    .values()
                    .cloned()
                    .map(|lookup| serde_json::to_vec(&self.lookup_row(lookup))),
            )
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid())
    }
    pub(super) fn put_lookup(&mut self, mut lookup: Lookup) -> Result<(), LogError> {
        self.validate_lookup(&lookup)?;
        if self.entries.contains_key(&lookup.id) {
            return Err(invalid());
        }
        let old = self.lookups.get(&lookup.id);
        if old.is_none()
            && (self.entries.len() + self.lookups.len() >= MAX_ENTRIES
                || self
                    .entries
                    .values()
                    .filter(|entry| !entry.terminal())
                    .count()
                    + self.lookups.len()
                    >= MAX_PENDING)
        {
            return Err(invalid());
        }
        lookup.sequence = self.sequence.checked_add(1).ok_or_else(invalid)?;
        let size = self.lookup_size(&lookup)?;
        if self
            .live
            .saturating_sub(
                old.map(|old| self.lookup_size(old))
                    .transpose()?
                    .unwrap_or(0),
            )
            .saturating_add(size)
            > self.live_budget()
        {
            return Err(invalid());
        }
        let bytes = serde_json::to_vec(&self.lookup_row(lookup.clone())).map_err(|_| invalid())?;
        if self.records
            >= (self.entries.len() + self.lookups.len())
                .saturating_mul(2)
                .saturating_add(32)
            || self.bytes + bytes.len() + 40 > 120 * 1024 * 1024
        {
            let rows = self.compacted_rows()?;
            self.file.rewrite(&rows)?;
            self.records = rows.len();
            self.bytes = 22 + rows.iter().map(|row| row.len() + 40).sum::<usize>();
        }
        self.file.append_durable(&bytes)?;
        self.records += 1;
        self.bytes += bytes.len() + 40;
        self.install_lookup(lookup)
    }
    pub(super) fn adopt(&mut self, entry: Entry) -> Result<(), LogError> {
        let id = entry.reference.id;
        if let Some(lookup) = self.lookups.get(&id)
            && (lookup.group != entry.reference.group || lookup.digest != entry.reference.digest)
        {
            return Err(invalid());
        }
        self.put(entry)
    }
    pub(in crate::node) fn complete(
        &mut self,
        reference: &OperationReference,
        events: &[crate::eventlog::Event],
    ) -> Result<(), LogError> {
        let mut entry = self
            .entries
            .get(&reference.id)
            .filter(|entry| entry.source && entry.reference.same(reference))
            .cloned()
            .ok_or_else(invalid)?;
        let ids: Vec<_> = events.iter().map(|event| event.id).collect();
        if entry.terminal() {
            return if entry.status.phase == AccountGroupOperationPhase::Committed
                && entry.status.committed_events == ids
            {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        entry.status.phase = AccountGroupOperationPhase::Committed;
        entry.status.primary_event = events
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.kind,
                    crate::eventlog::EventKind::Message | crate::eventlog::EventKind::FileManifest
                )
            })
            .map(|event| event.id);
        entry.status.committed_events = ids.clone();
        entry.status.executions = vec![AccountGroupOperationExecution {
            device_id: self.device.user_id(),
            phase: AccountGroupOperationPhase::Committed,
            committed_events: ids,
        }];
        self.put(entry)
    }
}

impl Node {
    #[cfg(test)]
    pub(in crate::node) fn fail_account_group_operation_journal_appends_for_test(
        &self,
        enabled: bool,
    ) -> std::io::Result<()> {
        self.group_operations
            .lock()
            .unwrap()
            .file
            .fail_appends_for_test(enabled)
    }

    /// Bounded metadata over the existing source terminal replay index. The
    /// caller advances only past admitted groups, retaining durable overflow.
    pub(in crate::node) fn account_group_fanout_page(
        &self,
        after: u64,
        limit: usize,
    ) -> Vec<(u64, ConversationId)> {
        use std::ops::Bound::{Excluded, Unbounded};
        let journal = self
            .group_operations
            .lock()
            .expect("group operation journal mutex");
        journal
            .fanout_order
            .range((Excluded(after), Unbounded))
            .take(limit.clamp(1, 128))
            .map(|(sequence, group)| (*sequence, ConversationId::new(*group)))
            .collect()
    }
    pub(in crate::node) fn account_group_fanout_sequence(
        &self,
        group: ConversationId,
    ) -> Option<u64> {
        self.group_operations
            .lock()
            .expect("group operation journal mutex")
            .fanout_groups
            .get(group.as_bytes())
            .copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        channel::{ChannelMeta, new_channel_id},
        discovery::Roster,
        eventlog::{Author, Event, EventKind},
        identity::account::Account,
        node::delivery_store::{DeliveryTransaction, RequestRecord},
    };
    use std::sync::{Arc, Mutex};
    fn open(path: &Path, account: &Account, identity: DeviceIdentity) -> Arc<Node> {
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (files, _) = tokio::sync::mpsc::unbounded_channel();
        Node::open_with_account(
            identity,
            Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            files,
            &path.join("events.log"),
            &path.join("sent.log"),
            "pw",
        )
        .unwrap()
    }
    fn fixture() -> (tempfile::TempDir, Arc<Node>, Account, Entry, Event) {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let node = open(dir.path(), &account, DeviceIdentity::generate());
        let group = new_channel_id();
        let genesis = Event::new(
            &node.identity,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Journal before".into(),
                members: vec![node.identity.public()],
                epoch: 0,
                owner: node.user_id(),
            }
            .encode(),
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(genesis.clone())
            .unwrap();
        node.process_channel(group);
        let operation = AccountGroupOperation::Rename {
            name: "Journal after".into(),
        };
        let executor = node
            .account_group_access(group)
            .unwrap()
            .sources
            .into_iter()
            .find(|source| source.device == node.identity.public())
            .unwrap();
        let reference = OperationReference::new(
            &node,
            node.new_account_group_operation_id(&account.account_id())
                .unwrap(),
            group,
            &operation,
            executor,
        );
        assert!(reference.valid(&account.public().ed25519_pub));
        let status = AccountGroupOperationStatus {
            id: reference.id,
            account_id: account.account_id(),
            group,
            kind: AccountGroupOperationKind::Rename,
            phase: AccountGroupOperationPhase::Accepted,
            executions: Vec::new(),
            committed_events: Vec::new(),
            primary_event: None,
        };
        let entry = Entry {
            reference,
            body: Some(operation),
            status,
            source: true,
            cancel: false,
            sequence: 0,
        };
        let effect = Event::new(
            &node.identity,
            group,
            2,
            vec![genesis.id],
            2,
            2,
            EventKind::ChannelRename,
            b"Journal after".to_vec(),
        );
        (dir, node, account, entry, effect)
    }
    fn transaction(accepted: &Entry, effect: &Event) -> DeliveryTransaction {
        DeliveryTransaction::GroupRequested {
            operation: accepted.reference.clone(),
            transaction: Box::new(DeliveryTransaction::Requested {
                request: RequestRecord {
                    request_id: *accepted.reference.id.as_bytes(),
                    digest: accepted.reference.digest,
                    destination: crate::node::MessageDestination::Channel(effect.conversation_id),
                    message_id: effect.id,
                    accepted_at_ms: 2,
                },
                transaction: Box::new(DeliveryTransaction::ChannelControl {
                    events: vec![effect.clone()],
                }),
            }),
        }
    }
    #[tokio::test]
    async fn account_adoption_drains_prepared_effect_and_stops_old_unprepared_work() {
        let (_dir, node, _account, accepted, effect) = fixture();
        node.group_operations
            .lock()
            .unwrap()
            .put(accepted.clone())
            .unwrap();
        let mut pending = accepted.clone();
        pending.reference.id = node
            .new_account_group_operation_id(&node.account_id())
            .unwrap();
        pending.reference.signature = node.identity.sign(&pending.reference.message()).to_vec();
        pending.status.id = pending.reference.id;
        node.group_operations
            .lock()
            .unwrap()
            .put(pending.clone())
            .unwrap();
        node.delivery
            .lock()
            .unwrap()
            .begin(transaction(&accepted, &effect))
            .unwrap();
        let destination = Account::generate();
        node.persist_account_adoption(&destination.account_id(), || Ok(()))
            .unwrap();
        assert_eq!(
            node.group_operations.lock().unwrap().entries[&accepted.reference.id]
                .status
                .phase,
            AccountGroupOperationPhase::Committed,
            "prepared old-account effects settle in their original journal before account replacement"
        );
        node.maintain_account_group_operations().await;
        assert_eq!(
            node.group_operations.lock().unwrap().entries[&pending.reference.id]
                .status
                .phase,
            AccountGroupOperationPhase::Accepted,
            "retired old-account work cannot be prepared or completed after successful adoption"
        );
    }
    #[test]
    fn a_retired_account_node_rejects_new_group_intent_before_journal_admission() {
        let (_dir, node, account, accepted, _effect) = fixture();
        let destination = Account::generate();
        node.persist_account_adoption(&destination.account_id(), || Ok(()))
            .unwrap();
        let id = node
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        let result = node.submit_account_group_operation(
            &account.account_id(),
            id,
            accepted.reference.group,
            AccountGroupOperation::Rename {
                name: "Must not accept retired intent".into(),
            },
        );
        assert!(
            matches!(result, Err(NodeError::Authorization(_))),
            "successful account replacement must close old-node group admission before accepting work"
        );
        assert!(
            !node
                .group_operations
                .lock()
                .unwrap()
                .entries
                .contains_key(&id)
        );
    }
    #[tokio::test]
    async fn local_noop_and_cancelled_rename_never_fabricate_public_effects() {
        let (_dir, node, account, accepted, _effect) = fixture();
        let group = accepted.reference.group;
        let id = node
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        node.submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "  Journal before  ".into(),
            },
        )
        .unwrap();
        node.maintain_account_group_operations().await;
        let status = node
            .account_group_operation_status(&account.account_id(), id)
            .unwrap()
            .unwrap();
        assert_eq!(status.phase, AccountGroupOperationPhase::Committed);
        assert!(status.committed_events.is_empty() && status.primary_event.is_none());
        let cancelled = node
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        node.submit_account_group_operation(
            &account.account_id(),
            cancelled,
            group,
            AccountGroupOperation::Rename {
                name: "Must not rename".into(),
            },
        )
        .unwrap();
        assert_eq!(
            node.cancel_account_group_operation(&account.account_id(), cancelled)
                .unwrap()
                .phase,
            AccountGroupOperationPhase::CancellationRequested
        );
        node.maintain_account_group_operations().await;
        let status = node
            .account_group_operation_status(&account.account_id(), cancelled)
            .unwrap()
            .unwrap();
        assert_eq!(status.phase, AccountGroupOperationPhase::Cancelled);
        assert!(status.committed_events.is_empty());
        assert_eq!(node.log.lock().unwrap().events(&group).len(), 1);
    }
    #[tokio::test]
    async fn a_terminal_status_query_is_not_starved_by_active_source_results() {
        let (_dir, node, account, accepted, _effect) = fixture();
        let group = accepted.reference.group;
        let id = node
            .new_account_group_operation_id(&account.account_id())
            .unwrap();
        node.submit_account_group_operation(
            &account.account_id(),
            id,
            group,
            AccountGroupOperation::Rename {
                name: "Journal before".into(),
            },
        )
        .unwrap();
        node.maintain_account_group_operations().await;
        let mut journal = node.group_operations.lock().unwrap();
        for _ in 0..16 {
            let mut active = accepted.clone();
            active.reference.id = node
                .new_account_group_operation_id(&account.account_id())
                .unwrap();
            active.reference.signature = node.identity.sign(&active.reference.message()).to_vec();
            active.status.id = active.reference.id;
            journal.put(active).unwrap();
        }
        let queried = journal.entries[&id].clone();
        journal.put(queried).unwrap();
        drop(journal);
        assert!(
            node.operation_projection(group)
                .results
                .iter()
                .any(|result| result.reference.id == id),
            "a source must republish an old immutable terminal result even while other accepted work is pending"
        );
    }
    #[test]
    fn recovered_control_refreshes_metadata_while_plaintext_retry_remains_blocked() {
        let (_dir, node, _account, accepted, effect) = fixture();
        node.group_operations
            .lock()
            .unwrap()
            .put(accepted.clone())
            .unwrap();
        node.delivery
            .lock()
            .unwrap()
            .begin(transaction(&accepted, &effect))
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        node.pending_channel_plaintext.lock().unwrap().retry_at = Some(deadline);
        node.recover_delivery(&mut node.delivery.lock().unwrap())
            .unwrap();
        assert_eq!(
            node.pending_channel_plaintext.lock().unwrap().retry_at,
            Some(deadline),
            "control refresh must preserve the existing plaintext retry gate"
        );
        assert_eq!(
            node.group_operations
                .lock()
                .unwrap()
                .entries
                .get(&accepted.reference.id)
                .unwrap()
                .status
                .phase,
            AccountGroupOperationPhase::Committed
        );
        assert_eq!(
            node.channels
                .lock()
                .unwrap()
                .state(&effect.conversation_id)
                .unwrap()
                .name(),
            "Journal after",
            "a durable control must refresh canonical metadata before a later local no-op decision"
        );
    }
    #[test]
    fn source_terminal_fanout_coalesces_history_per_group_and_rebuilds_on_reopen() {
        let (dir, node, account, accepted, effect) = fixture();
        let group = effect.conversation_id;
        node.log
            .lock()
            .unwrap()
            .append_durable(effect.clone())
            .unwrap();
        {
            let mut journal = node.group_operations.lock().unwrap();
            journal.put(accepted.clone()).unwrap();
            journal
                .complete(&accepted.reference, std::slice::from_ref(&effect))
                .unwrap();
        }
        let first = node.account_group_fanout_page(0, 128);
        assert_eq!(first.len(), 1);
        let operation = AccountGroupOperation::Rename {
            name: "another exact effect".into(),
        };
        let mut next = accepted.clone();
        next.reference = OperationReference::new(
            &node,
            node.new_account_group_operation_id(&account.account_id())
                .unwrap(),
            group,
            &operation,
            accepted.reference.executor.clone(),
        );
        next.status.id = next.reference.id;
        next.body = Some(operation);
        let next_effect = Event::new(
            &node.identity,
            group,
            3,
            vec![effect.id],
            3,
            3,
            EventKind::ChannelRename,
            b"another exact effect".to_vec(),
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(next_effect.clone())
            .unwrap();
        {
            let mut journal = node.group_operations.lock().unwrap();
            journal.put(next.clone()).unwrap();
            journal.complete(&next.reference, &[next_effect]).unwrap();
        }
        let later = node.account_group_fanout_page(0, 128);
        assert_eq!(
            later.len(),
            1,
            "many historical source operations schedule one current group"
        );
        assert!(later[0].0 > first[0].0);
        assert_eq!(later[0].1, group);
        let public = node.identity.public();
        drop(node);
        let journal =
            OperationJournal::open(dir.path(), "pw", account.public().ed25519_pub, public).unwrap();
        assert_eq!(journal.fanout_order.len(), 1);
        assert_eq!(
            journal.fanout_groups.get(group.as_bytes()),
            Some(&later[0].0)
        );
    }

    #[test]
    fn journal_physical_frame_accounting_excludes_reserved_terminal_capacity() {
        let (dir, node, _account, accepted, _effect) = fixture();
        let mut journal = node.group_operations.lock().unwrap();
        journal.put(accepted).unwrap();
        let path = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("account-group-operations-")
            })
            .unwrap();
        assert_eq!(
            journal.bytes as u64,
            std::fs::metadata(path).unwrap().len(),
            "physical log bytes and admission's reserved terminal capacity are distinct budgets"
        );
    }
    #[test]
    fn accepted_intent_reserves_its_larger_terminal_receipt_before_preparation() {
        let (_dir, node, _account, accepted, effect) = fixture();
        {
            let mut journal = node.group_operations.lock().unwrap();
            journal.live_limit = journal.size(&accepted).unwrap();
            journal.put(accepted.clone()).unwrap();
        }
        node.log
            .lock()
            .unwrap()
            .append_durable(effect.clone())
            .unwrap();
        let mut journal = node.group_operations.lock().unwrap();
        journal
            .complete(&accepted.reference, &[effect])
            .expect("terminal capacity was reserved before source acceptance");
        assert_eq!(
            journal
                .entries
                .get(&accepted.reference.id)
                .unwrap()
                .status
                .phase,
            AccountGroupOperationPhase::Committed
        );
        assert!(journal.live <= journal.live_budget());
    }
    #[test]
    fn terminal_commit_cannot_regress_or_change_effects_after_reopen() {
        let (dir, node, account, accepted, effect) = fixture();
        node.group_operations
            .lock()
            .unwrap()
            .put(accepted.clone())
            .unwrap();
        node.log
            .lock()
            .unwrap()
            .append_durable(effect.clone())
            .unwrap();
        node.group_operations
            .lock()
            .unwrap()
            .complete(&accepted.reference, std::slice::from_ref(&effect))
            .unwrap();
        let device = node.identity.public();
        drop(node);
        let mut journal =
            OperationJournal::open(dir.path(), "pw", account.public().ed25519_pub, device).unwrap();
        let terminal = journal.entries.get(&accepted.reference.id).unwrap().clone();
        assert_eq!(terminal.status.phase, AccountGroupOperationPhase::Committed);
        assert!(
            journal.put(accepted).is_err(),
            "a stale Accepted projection cannot replace a durable terminal outcome"
        );
        let mut conflicting = terminal.clone();
        conflicting.status.committed_events = vec![EventId::new([5; 32])];
        assert!(
            journal.put(conflicting).is_err(),
            "an ID's exact terminal effects are immutable"
        );
        assert!(journal.entries.get(&terminal.reference.id).unwrap().status == terminal.status);
    }
    #[test]
    fn terminal_effects_cannot_be_replaced_by_a_conflicting_terminal_result() {
        let (_dir, node, _account, accepted, effect) = fixture();
        node.group_operations
            .lock()
            .unwrap()
            .put(accepted.clone())
            .unwrap();
        node.log
            .lock()
            .unwrap()
            .append_durable(effect.clone())
            .unwrap();
        let mut journal = node.group_operations.lock().unwrap();
        journal.complete(&accepted.reference, &[effect]).unwrap();
        let mut conflicting = journal.entries.get(&accepted.reference.id).unwrap().clone();
        conflicting.status.committed_events = vec![EventId::new([5; 32])];
        assert!(
            journal.put(conflicting).is_err(),
            "one operation cannot change its exact terminal effect IDs"
        );
    }
    #[test]
    fn replay_cannot_restore_an_accepted_row_after_a_durable_terminal() {
        let (dir, node, account, accepted, effect) = fixture();
        let device = node.identity.public();
        node.group_operations
            .lock()
            .unwrap()
            .put(accepted.clone())
            .unwrap();
        node.log
            .lock()
            .unwrap()
            .append_durable(effect.clone())
            .unwrap();
        let mut journal = node.group_operations.lock().unwrap();
        journal.complete(&accepted.reference, &[effect]).unwrap();
        let mut stale = accepted.clone();
        stale.sequence = journal.sequence + 1;
        let bytes = serde_json::to_vec(&journal.row(stale)).unwrap();
        journal.file.append_durable(&bytes).unwrap();
        drop(journal);
        drop(node);
        let replayed =
            OperationJournal::open(dir.path(), "pw", account.public().ed25519_pub, device);
        assert!(
            replayed.is_err()
                || replayed
                    .unwrap()
                    .entries
                    .get(&accepted.reference.id)
                    .unwrap()
                    .status
                    .phase
                    == AccountGroupOperationPhase::Committed,
            "an obsolete projection cannot erase replay safety during encrypted journal recovery"
        );
    }
    #[test]
    fn cancelled_or_rejected_terminal_cannot_claim_committed_effects() {
        let (_dir, node, _account, accepted, effect) = fixture();
        for phase in [
            AccountGroupOperationPhase::Cancelled,
            AccountGroupOperationPhase::Rejected {
                reason: AccountGroupOperationRejection::MembershipChanged,
            },
        ] {
            let mut invalid = accepted.clone();
            invalid.status.phase = phase;
            invalid.status.committed_events = vec![effect.id];
            assert!(
                node.group_operations.lock().unwrap().put(invalid).is_err(),
                "a terminal no-effect outcome cannot conceal an already committed effect"
            );
        }
    }
    #[test]
    fn full_origin_identity_binds_minted_ids_and_signed_references() {
        let (_dir, node, account, entry, _effect) = fixture();
        let origin = node.identity.public();
        let mut changed = origin.clone();
        changed.x25519_pub[0] ^= 1;
        let first = AccountGroupOperationId::for_origin(&origin, [9; 16]);
        let second = AccountGroupOperationId::for_origin(&changed, [9; 16]);
        assert_ne!(first, second);
        let mut reference = entry.reference.clone();
        reference.id = second;
        reference.signature = node.identity.sign(&reference.message()).to_vec();
        assert!(!reference.valid(&account.public().ed25519_pub));
        let mut invalid = entry;
        invalid.status.id = second;
        invalid.reference = reference;
        assert!(node.group_operations.lock().unwrap().put(invalid).is_err());
        assert!(node.group_operations.lock().unwrap().entries.is_empty());
    }
    #[test]
    fn node_restart_recovers_prepared_effect_before_terminal_and_later_allocation() {
        let (dir, node, account, accepted, effect) = fixture();
        let group = effect.conversation_id;
        node.group_operations
            .lock()
            .unwrap()
            .put(accepted.clone())
            .unwrap();
        let transaction = transaction(&accepted, &effect);
        node.delivery.lock().unwrap().begin(transaction).unwrap();
        node.group_operations
            .lock()
            .unwrap()
            .file
            .fail_appends_for_test(true)
            .unwrap();
        assert!(
            node.recover_delivery(&mut node.delivery.lock().unwrap())
                .is_err()
        );
        assert!(node.log.lock().unwrap().get(&effect.id).is_some());
        assert_eq!(
            node.delivery.lock().unwrap().pending_transactions().len(),
            1
        );
        let secrets = node.identity.secret_bytes();
        drop(node);
        let restarted = open(
            dir.path(),
            &account,
            DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
        );
        let fanout = restarted.account_group_fanout_page(0, 128);
        assert_eq!(
            fanout.len(),
            1,
            "bootstrap recovery must retain one durable source group for public fanout"
        );
        assert_eq!(fanout[0].1, group);
        assert!(
            restarted
                .account_group_fanout_page(fanout[0].0, 128)
                .is_empty()
        );
        assert!(
            restarted
                .delivery
                .lock()
                .unwrap()
                .pending_transactions()
                .is_empty()
        );
        assert_eq!(
            restarted
                .group_operations
                .lock()
                .unwrap()
                .entries
                .get(&accepted.reference.id)
                .unwrap()
                .status
                .phase,
            AccountGroupOperationPhase::Committed
        );
        assert_eq!(
            restarted
                .channels
                .lock()
                .unwrap()
                .state(&group)
                .unwrap()
                .name(),
            "Journal after"
        );
        let author = Author::from_ed25519(restarted.identity.public().ed25519_pub);
        assert_eq!(
            restarted
                .log
                .lock()
                .unwrap()
                .next_sequence(&group, &author)
                .unwrap(),
            effect.seq + 1
        );
        restarted
            .append_event(group, EventKind::ChannelRename, b"Still later".to_vec())
            .unwrap();
        let events: Vec<_> = restarted
            .log
            .lock()
            .unwrap()
            .events(&group)
            .into_iter()
            .filter(|event| event.kind == EventKind::ChannelRename)
            .cloned()
            .collect();
        assert_eq!(events.len(), 2);
        assert!(
            events
                .iter()
                .any(|event| event.id == effect.id && event.seq == effect.seq)
        );
        assert!(
            events
                .iter()
                .all(|event| event.verify_signature() && event.verify_integrity())
        );
    }
}
