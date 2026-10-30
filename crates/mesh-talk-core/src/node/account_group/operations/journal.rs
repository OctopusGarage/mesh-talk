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
    // The original device signs randomized file preparation separately. Logical
    // retries bind stable content metadata, while every other operation retains
    // its existing exact digest bytes.
    let logical_file = match operation {
        AccountGroupOperation::Send { message }
            if message.kind == AccountGroupOperationKind::File =>
        {
            super::file_preparation::GroupFilePayload::decode(message.encoded_body())
                .filter(|payload| message.staged_assets == [payload.logical.checksum])
                .and_then(|payload| payload.logical_operation())
        }
        _ => None,
    };
    hash.update(
        serde_json::to_vec(logical_file.as_ref().unwrap_or(operation))
            .expect("operation body serializes"),
    );
    hash.finalize().into()
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(in crate::node) struct GroupFileCopy {
    pub descriptor: super::file_preparation::GroupFileDescriptor,
    pub complete: bool,
}

#[derive(Clone, Serialize, Deserialize)]
pub(super) struct Entry {
    pub reference: OperationReference,
    pub body: Option<AccountGroupOperation>,
    pub status: AccountGroupOperationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_copy: Option<GroupFileCopy>,
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
#[derive(Clone, Serialize, Deserialize)]
pub(super) struct LegacyLeavePlan {
    pub parent: OperationReference,
    pub legs: Vec<OperationReference>,
    pub roll_forward: bool,
}
#[derive(Clone, Serialize, Deserialize)]
struct LegacyBatch {
    plan: Option<LegacyLeavePlan>,
    links: Vec<(AccountGroupOperationId, OperationReference)>,
    entries: Vec<Entry>,
}
#[derive(Serialize, Deserialize)]
struct Record {
    account: [u8; 32],
    device: PublicIdentity,
    entry: Option<Entry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy: Option<LegacyBatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    lookup: Option<Lookup>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_preparation: Option<super::file_preparation::GroupFilePayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    file_assets: Option<super::file_preparation::GroupFileAssetsReceipt>,
}
pub(in crate::node) struct OperationJournal {
    file: EncryptedRecordLog<Vec<u8>>,
    pub(super) entries: BTreeMap<AccountGroupOperationId, Entry>,
    pub(super) lookups: BTreeMap<AccountGroupOperationId, Lookup>,
    pub(super) legacy_plans: BTreeMap<AccountGroupOperationId, LegacyLeavePlan>,
    pub(super) legacy_children: BTreeMap<AccountGroupOperationId, OperationReference>,
    file_asset_receipts:
        BTreeMap<AccountGroupOperationId, super::file_preparation::GroupFileAssetsReceipt>,
    file_preparations: BTreeMap<AccountGroupOperationId, super::file_preparation::GroupFilePayload>,
    account: [u8; 32],
    device: PublicIdentity,
    sequence: u64,
    // Scheduling progress only; reset on open and never encoded in journal rows.
    source_ready_after: Option<AccountGroupOperationId>,
    // Derived from retained source terminal records; never a second job journal.
    fanout_groups: BTreeMap<[u8; 32], u64>,
    fanout_order: BTreeMap<u64, [u8; 32]>,
    // Latest exact source Text/Sticker effect per group; rebuilt on replay.
    fanout_messages: BTreeMap<[u8; 32], (u64, EventId)>,
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
    /// Select a bounded source execution turn without changing durable status.
    pub(super) fn take_source_ready(
        &mut self,
        source: &PublicIdentity,
        limit: usize,
    ) -> Vec<Entry> {
        if limit == 0 {
            return Vec::new();
        }
        // Only pending IDs are collected (bounded by MAX_PENDING), and only
        // this turn's entries are cloned. Retained terminal history is skipped.
        let ids: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, entry)| entry.reference.executor.device == *source && !entry.terminal())
            .map(|(id, _)| *id)
            .collect();
        if ids.is_empty() {
            return Vec::new();
        }
        let start = self
            .source_ready_after
            .map_or(0, |after| ids.partition_point(|id| *id <= after));
        let ready: Vec<_> = (0..limit.min(ids.len()))
            .map(|offset| {
                let id = ids[(start + offset) % ids.len()];
                self.entries
                    .get(&id)
                    .expect("selected pending entry")
                    .clone()
            })
            .collect();
        self.source_ready_after = ready.last().map(|entry| entry.reference.id);
        ready
    }
    fn pending_count(&self) -> usize {
        self.entries
            .values()
            .filter(|entry| !entry.terminal())
            .count()
            + self.lookups.len()
            + self
                .file_preparations
                .keys()
                .filter(|id| !self.entries.contains_key(id) && !self.lookups.contains_key(id))
                .count()
    }
    #[cfg(test)]
    pub(super) fn test_file_preparation_pressure(
        &mut self,
        limit: usize,
        compact: bool,
    ) -> Result<(), LogError> {
        self.live_limit = limit;
        if compact {
            let rows = self.compacted_rows()?;
            self.file.rewrite(&rows)?;
            self.records = rows.len();
            self.bytes = 22 + rows.iter().map(|row| row.len() + 40).sum::<usize>();
        }
        Ok(())
    }
    fn preparation_row(&self, payload: super::file_preparation::GroupFilePayload) -> Record {
        Record {
            account: self.account,
            device: self.device.clone(),
            entry: None,
            legacy: None,
            lookup: None,
            file_assets: payload
                .descriptor
                .as_ref()
                .and_then(|descriptor| self.file_asset_receipts.get(&descriptor.id))
                .cloned(),
            file_preparation: Some(payload),
        }
    }
    fn preparation_bytes(
        &self,
        payload: &super::file_preparation::GroupFilePayload,
    ) -> Result<Vec<u8>, LogError> {
        serde_json::to_vec(&self.preparation_row(payload.clone())).map_err(|_| invalid())
    }
    fn file_body_matches_preparation(
        body: &AccountGroupOperation,
        original: &super::file_preparation::GroupFilePayload,
        require_descriptor: bool,
    ) -> bool {
        let AccountGroupOperation::Send { message } = body else {
            return false;
        };
        let Some(payload) =
            super::file_preparation::GroupFilePayload::decode(message.encoded_body())
        else {
            return false;
        };
        message.kind == AccountGroupOperationKind::File
            && message.staged_assets == [original.logical.checksum]
            && payload.logical == original.logical
            && match payload.descriptor.as_ref() {
                Some(descriptor) => original.descriptor.as_ref() == Some(descriptor),
                None => !require_descriptor,
            }
    }
    fn validate_preparation(
        &self,
        payload: &super::file_preparation::GroupFilePayload,
    ) -> Result<AccountGroupOperationId, LogError> {
        let descriptor = payload.descriptor.as_ref().ok_or_else(invalid)?;
        let signature: [u8; 64] = descriptor
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| invalid())?;
        let operation = payload.logical_operation().ok_or_else(invalid)?;
        if !descriptor.id.matches_origin(&descriptor.creator.public())
            || !descriptor.creator.verify()
            || descriptor.creator.post_office
            || descriptor
                .creator
                .account_cert
                .as_ref()
                .is_none_or(|cert| cert.account_ed25519_pub != self.account)
            || descriptor.logical_digest != operation_digest(descriptor.group, &operation)
            || !DeviceIdentity::verify(
                &descriptor.creator.ed25519_pub,
                &descriptor.message(),
                &signature,
            )
            || super::file_preparation::GroupFileLogical::from_manifest(&descriptor.manifest)
                .as_ref()
                != Some(&payload.logical)
            || payload.encode().is_none()
        {
            return Err(invalid());
        }
        if descriptor.creator.public() != self.device {
            let entry = self.entries.get(&descriptor.id).ok_or_else(invalid)?;
            if entry.reference.executor.device != self.device
                || !descriptor.valid_for(&entry.reference, &payload.logical)
            {
                return Err(invalid());
            }
        }
        if let Some(old) = self.file_preparations.get(&descriptor.id)
            && old != payload
        {
            return Err(invalid());
        }
        if let Some(entry) = self.entries.get(&descriptor.id)
            && (entry.reference.kind != AccountGroupOperationKind::File
                || entry.reference.group != descriptor.group
                || entry.reference.digest != descriptor.logical_digest
                || entry
                    .file_copy
                    .as_ref()
                    .is_some_and(|copy| payload.descriptor.as_ref() != Some(&copy.descriptor))
                || entry.body.as_ref().is_some_and(|body| {
                    !Self::file_body_matches_preparation(body, payload, entry.source)
                }))
        {
            return Err(invalid());
        }
        if let Some(lookup) = self.lookups.get(&descriptor.id)
            && (lookup.body.kind() != AccountGroupOperationKind::File
                || lookup.group != descriptor.group
                || lookup.digest != descriptor.logical_digest
                || !Self::file_body_matches_preparation(&lookup.body, payload, false))
        {
            return Err(invalid());
        }
        Ok(descriptor.id)
    }
    fn install_preparation(
        &mut self,
        payload: super::file_preparation::GroupFilePayload,
        assets: Option<super::file_preparation::GroupFileAssetsReceipt>,
    ) -> Result<(), LogError> {
        let id = self.validate_preparation(&payload)?;
        if assets
            .as_ref()
            .is_some_and(|receipt| !receipt.valid_for(&payload))
            || self.file_asset_receipts.contains_key(&id) && assets.is_none()
        {
            return Err(invalid());
        }
        let old_size = self
            .file_preparations
            .get(&id)
            .map(|old| self.preparation_bytes(old).map(|bytes| bytes.len() + 40))
            .transpose()?
            .unwrap_or(0);
        let mut row = self.preparation_row(payload.clone());
        row.file_assets = assets.clone();
        let size = serde_json::to_vec(&row).map_err(|_| invalid())?.len() + 40;
        let next_live = self.live.saturating_sub(old_size).saturating_add(size);
        if next_live > self.live_budget()
            || !self.file_preparations.contains_key(&id)
                && self.file_preparations.len() + self.entries.len() + self.lookups.len()
                    >= MAX_ENTRIES
        {
            return Err(invalid());
        }
        self.live = next_live;
        self.file_preparations.insert(id, payload);
        if let Some(receipt) = assets {
            self.file_asset_receipts.insert(id, receipt);
        }
        Ok(())
    }
    pub(super) fn preflight_file_preparation(&self) -> Result<(), LogError> {
        // A canonical MGF1 body is at most60KiB; its JSON byte arrays
        // expand at most fourfold, plus the existing scoped record envelope.
        if self.live.saturating_add(60 * 1024 * 4 + 4096) > self.live_budget()
            || self.pending_count() >= MAX_PENDING
            || self.entries.len() + self.lookups.len() + self.file_preparations.len() >= MAX_ENTRIES
        {
            return Err(invalid());
        }
        Ok(())
    }
    pub(super) fn put_file_preparation(
        &mut self,
        payload: super::file_preparation::GroupFilePayload,
    ) -> Result<(), LogError> {
        self.put_file_preparation_inner(payload, None)
    }
    pub(super) fn put_file_asset_receipt(
        &mut self,
        payload: super::file_preparation::GroupFilePayload,
        receipt: super::file_preparation::GroupFileAssetsReceipt,
    ) -> Result<(), LogError> {
        self.put_file_preparation_inner(payload, Some(receipt))
    }
    fn put_file_preparation_inner(
        &mut self,
        payload: super::file_preparation::GroupFilePayload,
        assets: Option<super::file_preparation::GroupFileAssetsReceipt>,
    ) -> Result<(), LogError> {
        let id = self.validate_preparation(&payload)?;
        if assets
            .as_ref()
            .is_some_and(|receipt| !receipt.valid_for(&payload))
        {
            return Err(invalid());
        }
        let assets = assets.or_else(|| self.file_asset_receipts.get(&id).cloned());
        if self.file_preparations.contains_key(&id)
            && assets.as_ref() == self.file_asset_receipts.get(&id)
        {
            return Ok(());
        }
        let mut row = self.preparation_row(payload.clone());
        row.file_assets = assets.clone();
        let bytes = serde_json::to_vec(&row).map_err(|_| invalid())?;
        let size = bytes.len() + 40;
        let old_size = self
            .file_preparations
            .get(&id)
            .map(|old| self.preparation_bytes(old).map(|bytes| bytes.len() + 40))
            .transpose()?
            .unwrap_or(0);
        if self.live.saturating_sub(old_size).saturating_add(size) > self.live_budget()
            || !self.file_preparations.contains_key(&id)
                && (self.file_preparations.len() + self.entries.len() + self.lookups.len()
                    >= MAX_ENTRIES
                    || self.pending_count() >= MAX_PENDING
                        && !self.entries.contains_key(&id)
                        && !self.lookups.contains_key(&id))
        {
            return Err(invalid());
        }
        if self.bytes.saturating_add(size) > 120 * 1024 * 1024
            || self.records
                >= (self.entries.len() + self.lookups.len() + self.file_preparations.len())
                    .saturating_mul(2)
                    .saturating_add(32)
        {
            let rows = self.compacted_rows()?;
            self.file.rewrite(&rows)?;
            self.records = rows.len();
            self.bytes = 22 + rows.iter().map(|row| row.len() + 40).sum::<usize>();
        }
        self.file.append_durable(&bytes)?;
        self.records += 1;
        self.bytes += size;
        self.install_preparation(payload, assets)
    }
    pub(super) fn file_asset_receipt(
        &self,
        id: AccountGroupOperationId,
    ) -> Option<&super::file_preparation::GroupFileAssetsReceipt> {
        self.file_asset_receipts.get(&id)
    }
    pub(super) fn file_preparation(
        &self,
        id: AccountGroupOperationId,
    ) -> Option<&super::file_preparation::GroupFilePayload> {
        self.file_preparations.get(&id)
    }
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
            legacy_plans: BTreeMap::new(),
            legacy_children: BTreeMap::new(),
            file_preparations: BTreeMap::new(),
            file_asset_receipts: BTreeMap::new(),
            account,
            device,
            sequence: 0,
            source_ready_after: None,
            fanout_groups: BTreeMap::new(),
            fanout_order: BTreeMap::new(),
            fanout_messages: BTreeMap::new(),
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
            if let Some(batch) = row.legacy {
                if row.entry.is_some()
                    || row.lookup.is_some()
                    || row.file_preparation.is_some()
                    || row.file_assets.is_some()
                {
                    return Err(invalid());
                }
                journal.validate_legacy_batch(&batch)?;
                journal.install_legacy_batch(batch)?;
                continue;
            }
            match (row.entry, row.lookup, row.file_preparation, row.file_assets) {
                (Some(entry), None, None, None) => {
                    journal.validate(&entry)?;
                    if journal
                        .entries
                        .get(&entry.reference.id)
                        .is_none_or(|old| old.sequence < entry.sequence)
                    {
                        journal.install(entry)?;
                    }
                }
                (None, Some(lookup), None, None) => {
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
                (None, None, Some(payload), assets) => {
                    journal.install_preparation(payload, assets)?
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
            legacy: None,
            lookup: None,
            file_preparation: None,
            file_assets: None,
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
        if let Some(copy) = &entry.file_copy {
            let logical =
                super::file_preparation::GroupFileLogical::from_manifest(&copy.descriptor.manifest)
                    .ok_or_else(invalid)?;
            let payload = super::file_preparation::GroupFilePayload {
                logical,
                descriptor: Some(copy.descriptor.clone()),
            };
            if reference.kind != AccountGroupOperationKind::File
                || entry.status.phase != AccountGroupOperationPhase::Committed
                || entry.status.primary_event.is_none()
                || !copy.descriptor.valid_for(reference, &payload.logical)
                || payload.encode().is_none()
                || self
                    .file_preparations
                    .get(&reference.id)
                    .is_some_and(|original| original != &payload)
                || entry
                    .body
                    .as_ref()
                    .is_some_and(|body| !Self::file_body_matches_preparation(body, &payload, false))
            {
                return Err(invalid());
            }
        }
        if self
            .entries
            .get(&reference.id)
            .and_then(|old| old.file_copy.as_ref())
            .is_some_and(|old| {
                entry.file_copy.as_ref().is_none_or(|copy| {
                    copy.descriptor != old.descriptor || old.complete && !copy.complete
                })
            })
        {
            return Err(invalid());
        }
        if let Some(payload) = self.file_preparations.get(&reference.id) {
            let descriptor = payload.descriptor.as_ref().ok_or_else(invalid)?;
            if reference.kind != AccountGroupOperationKind::File
                || reference.group != descriptor.group
                || reference.digest != descriptor.logical_digest
                || entry.body.as_ref().is_some_and(|body| {
                    !Self::file_body_matches_preparation(body, payload, entry.source)
                })
            {
                return Err(invalid());
            }
        }
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
            if matches!(
                entry.reference.kind,
                AccountGroupOperationKind::Text | AccountGroupOperationKind::Sticker
            ) && let Some(event) = entry.status.primary_event
                && self
                    .fanout_messages
                    .get(&group)
                    .is_none_or(|(sequence, _)| *sequence < entry.sequence)
            {
                self.fanout_messages.insert(group, (entry.sequence, event));
            }
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
    pub(super) fn complete_file_copy(
        &mut self,
        reference: &OperationReference,
        descriptor: &super::file_preparation::GroupFileDescriptor,
    ) -> Result<(), LogError> {
        let mut entry = self
            .entries
            .get(&reference.id)
            .cloned()
            .ok_or_else(invalid)?;
        if !entry.reference.same(reference)
            || entry.status.phase != AccountGroupOperationPhase::Committed
        {
            return Err(invalid());
        }
        let copy = entry.file_copy.as_mut().ok_or_else(invalid)?;
        if &copy.descriptor != descriptor {
            return Err(invalid());
        }
        if copy.complete {
            return Ok(());
        }
        copy.complete = true;
        self.put(entry)
    }

    fn pins_file_preparation(&self, file: ConversationId) -> bool {
        self.file_preparations.iter().any(|(id, payload)| {
            self.entries.get(id).is_none_or(|entry| !entry.terminal())
                && payload
                    .descriptor
                    .as_ref()
                    .and_then(|descriptor| {
                        crate::node::files::validated_manifest(&descriptor.manifest)
                    })
                    .is_some_and(|manifest| manifest.file_conv() == file)
        })
    }

    fn legacy_pair_valid(&self, parent: &OperationReference, child: &OperationReference) -> bool {
        parent.valid(&self.account)
            && child.valid(&self.account)
            && parent.kind == AccountGroupOperationKind::LeaveAccount
            && child.kind == parent.kind
            && parent.group == child.group
            && parent.id != child.id
            && parent.executor.authority.authority.is_none()
            && child.executor.authority.authority.is_none()
            && parent.executor.role == AccountGroupRole::Member
            && child.executor.role == AccountGroupRole::Member
            && parent.executor.device == child.origin.public()
            && parent.executor.lifetime.member == parent.executor.device
            && child.executor.lifetime.member == child.executor.device
            && parent.digest == operation_digest(parent.group, &AccountGroupOperation::LeaveAccount)
            && child.digest == parent.digest
            && serde_json::to_vec(parent).is_ok_and(|v| v.len() <= 16 * 1024)
            && serde_json::to_vec(child).is_ok_and(|v| v.len() <= 16 * 1024)
    }
    fn legacy_plan_size(plan: &LegacyLeavePlan) -> Result<usize, LogError> {
        Ok(serde_json::to_vec(plan).map_err(|_| invalid())?.len() + 40)
    }
    fn legacy_link_size(
        id: AccountGroupOperationId,
        parent: &OperationReference,
    ) -> Result<usize, LogError> {
        Ok(serde_json::to_vec(&(id, parent))
            .map_err(|_| invalid())?
            .len()
            + 40)
    }
    fn legacy_bytes(&self, batch: &LegacyBatch) -> Result<Vec<u8>, LogError> {
        serde_json::to_vec(&Record {
            account: self.account,
            device: self.device.clone(),
            entry: None,
            legacy: Some(batch.clone()),
            lookup: None,
            file_preparation: None,
            file_assets: None,
        })
        .map_err(|_| invalid())
    }
    fn validate_legacy_batch(&self, batch: &LegacyBatch) -> Result<(), LogError> {
        if batch.entries.len() > 65
            || batch.links.len() > 65
            || (batch.plan.is_none() && (batch.links.len() != 1 || batch.entries.len() > 1))
        {
            return Err(invalid());
        }
        let mut ids = std::collections::BTreeSet::new();
        let mut next_live = self.live;
        let mut pending = self.pending_count();
        let mut entries = self.entries.len() + self.lookups.len() + self.file_preparations.len();
        for entry in &batch.entries {
            self.validate(entry)?;
            if !ids.insert(entry.reference.id) || entry.source {
                return Err(invalid());
            }
            if let Some(lookup) = self.lookups.get(&entry.reference.id)
                && (lookup.group != entry.reference.group
                    || lookup.digest != entry.reference.digest)
            {
                return Err(invalid());
            }
            let old = self.entries.get(&entry.reference.id);
            let lookup = self.lookups.get(&entry.reference.id);
            let old_pending = old.map_or(usize::from(lookup.is_some()), |old| {
                usize::from(!old.terminal())
            });
            pending = pending.saturating_sub(old_pending) + usize::from(!entry.terminal());
            if old.is_none() && lookup.is_none() {
                entries += 1;
            }
            next_live = next_live
                .saturating_sub(old.map(|old| self.size(old)).transpose()?.unwrap_or(0))
                .saturating_sub(
                    lookup
                        .map(|old| self.lookup_size(old))
                        .transpose()?
                        .unwrap_or(0),
                )
                .checked_add(self.size(entry)?)
                .ok_or_else(invalid)?;
        }
        let entry_for = |id: &AccountGroupOperationId| {
            batch
                .entries
                .iter()
                .find(|entry| entry.reference.id == *id)
                .or_else(|| self.entries.get(id))
        };
        let mut links = std::collections::BTreeSet::new();
        for (id, parent) in &batch.links {
            let child = entry_for(id).ok_or_else(invalid)?;
            if !links.insert(*id)
                || !self.legacy_pair_valid(parent, &child.reference)
                || (parent.executor.device != self.device
                    && child.reference.executor.device != self.device)
                || self
                    .legacy_children
                    .get(id)
                    .is_some_and(|old| !old.same(parent))
                || self.legacy_plans.contains_key(id)
            {
                return Err(invalid());
            }
            if !self.legacy_children.contains_key(id) {
                next_live = next_live
                    .checked_add(Self::legacy_link_size(*id, parent)?)
                    .ok_or_else(invalid)?;
            }
        }
        if batch
            .entries
            .iter()
            .any(|entry| !links.contains(&entry.reference.id))
        {
            return Err(invalid());
        }
        if let Some(plan) = &batch.plan {
            let parent = self.entries.get(&plan.parent.id).ok_or_else(invalid)?;
            if !parent.source
                || (parent.terminal() && !batch.entries.is_empty())
                || !parent.reference.same(&plan.parent)
                || plan.parent.executor.device != self.device
                || plan.legs.is_empty()
                || plan.legs.len() > 65
            {
                return Err(invalid());
            }
            let mut leg_ids = std::collections::BTreeSet::new();
            let mut devices = std::collections::BTreeSet::new();
            for leg in &plan.legs {
                if !leg_ids.insert(leg.id)
                    || !devices.insert(leg.executor.device.user_id())
                    || !self.legacy_pair_valid(&plan.parent, leg)
                    || !entry_for(&leg.id).is_some_and(|entry| entry.reference.same(leg))
                    || !batch
                        .links
                        .iter()
                        .any(|(id, parent)| *id == leg.id && parent.same(&plan.parent))
                {
                    return Err(invalid());
                }
            }
            if batch
                .entries
                .iter()
                .any(|entry| !leg_ids.contains(&entry.reference.id))
            {
                return Err(invalid());
            }
            if let Some(old) = self.legacy_plans.get(&plan.parent.id) {
                if !old.parent.same(&plan.parent)
                    || old.legs.len() != plan.legs.len()
                    || (old.roll_forward && !plan.roll_forward)
                {
                    return Err(invalid());
                }
                let mut changed = 0;
                for (previous, next) in old.legs.iter().zip(&plan.legs) {
                    if previous.same(next) {
                        continue;
                    }
                    changed += 1;
                    let retired = self.entries.get(&previous.id).ok_or_else(invalid)?;
                    if previous.id == next.id
                        || previous.executor.device != next.executor.device
                        || previous.executor.lifetime != next.executor.lifetime
                        || previous.executor.authority != next.executor.authority
                        || previous.executor.role != next.executor.role
                        || !matches!(
                            retired.status.phase,
                            AccountGroupOperationPhase::Cancelled
                                | AccountGroupOperationPhase::Rejected { .. }
                        )
                        || !retired.status.committed_events.is_empty()
                        || retired.status.primary_event.is_some()
                        || retired
                            .status
                            .executions
                            .iter()
                            .any(|execution| !execution.committed_events.is_empty())
                    {
                        return Err(invalid());
                    }
                }
                if changed > 1 {
                    return Err(invalid());
                }
                next_live = next_live.saturating_sub(Self::legacy_plan_size(old)?);
            }
            next_live = next_live
                .checked_add(Self::legacy_plan_size(plan)?)
                .ok_or_else(invalid)?;
        }
        if entries > MAX_ENTRIES || pending > MAX_PENDING || next_live > self.live_budget() {
            return Err(invalid());
        }
        Ok(())
    }
    fn install_legacy_batch(&mut self, batch: LegacyBatch) -> Result<(), LogError> {
        for entry in batch.entries {
            self.install(entry)?;
        }
        for (id, parent) in batch.links {
            if !self.legacy_children.contains_key(&id) {
                self.live += Self::legacy_link_size(id, &parent)?;
            }
            self.legacy_children.insert(id, parent);
        }
        if let Some(plan) = batch.plan {
            if let Some(old) = self.legacy_plans.get(&plan.parent.id) {
                self.live -= Self::legacy_plan_size(old)?;
            }
            self.live += Self::legacy_plan_size(&plan)?;
            self.legacy_plans.insert(plan.parent.id, plan);
        }
        Ok(())
    }
    fn append_legacy_batch(&mut self, mut batch: LegacyBatch) -> Result<(), LogError> {
        for (offset, entry) in batch.entries.iter_mut().enumerate() {
            entry.sequence = self
                .sequence
                .checked_add(offset as u64 + 1)
                .ok_or_else(invalid)?;
            if entry.terminal() {
                entry.body = None;
            }
        }
        self.validate_legacy_batch(&batch)?;
        let bytes = self.legacy_bytes(&batch)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(invalid());
        }
        if self.records
            >= self
                .entries
                .len()
                .saturating_add(self.lookups.len())
                .saturating_add(self.legacy_children.len())
                .saturating_mul(2)
                .saturating_add(32)
            || self.bytes.saturating_add(bytes.len() + 40) > 120 * 1024 * 1024
        {
            let rows = self.compacted_rows()?;
            self.file.rewrite(&rows)?;
            self.records = rows.len();
            self.bytes = 22 + rows.iter().map(|row| row.len() + 40).sum::<usize>();
        }
        self.file.append_durable(&bytes)?;
        self.records += 1;
        self.bytes += bytes.len() + 40;
        self.install_legacy_batch(batch)
    }
    pub(super) fn put_legacy_plan(
        &mut self,
        plan: LegacyLeavePlan,
        children: Vec<Entry>,
    ) -> Result<(), LogError> {
        let links = plan
            .legs
            .iter()
            .map(|leg| (leg.id, plan.parent.clone()))
            .collect();
        self.append_legacy_batch(LegacyBatch {
            plan: Some(plan),
            links,
            entries: children,
        })
    }
    pub(super) fn put_legacy_child(
        &mut self,
        parent: OperationReference,
        entry: Entry,
    ) -> Result<(), LogError> {
        if let Some(old_parent) = self.legacy_children.get(&entry.reference.id) {
            return if old_parent.same(&parent)
                && self
                    .entries
                    .get(&entry.reference.id)
                    .is_some_and(|old| old.reference.same(&entry.reference))
            {
                Ok(())
            } else {
                Err(invalid())
            };
        }
        self.append_legacy_batch(LegacyBatch {
            plan: None,
            links: vec![(entry.reference.id, parent)],
            entries: vec![entry],
        })
    }
    pub(super) fn put(&mut self, mut entry: Entry) -> Result<(), LogError> {
        self.validate(&entry)?;
        let old = self.entries.get(&entry.reference.id);
        let old_lookup = self.lookups.get(&entry.reference.id);
        let pending = self.pending_count();
        if old.is_none()
            && old_lookup.is_none()
            && (self.entries.len() + self.lookups.len() + self.file_preparations.len()
                >= MAX_ENTRIES
                || !entry.terminal()
                    && pending >= MAX_PENDING
                    && !self.file_preparations.contains_key(&entry.reference.id))
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
            legacy: None,
            lookup: Some(lookup),
            file_preparation: None,
            file_assets: None,
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
        if let Some(payload) = self.file_preparations.get(&lookup.id) {
            let descriptor = payload.descriptor.as_ref().ok_or_else(invalid)?;
            if lookup.body.kind() != AccountGroupOperationKind::File
                || lookup.group != descriptor.group
                || lookup.digest != descriptor.logical_digest
                || !Self::file_body_matches_preparation(&lookup.body, payload, false)
            {
                return Err(invalid());
            }
        }
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
        let mut rows = self
            .entries
            .values()
            .cloned()
            .map(|entry| serde_json::to_vec(&self.row(entry)))
            .chain(
                self.lookups
                    .values()
                    .cloned()
                    .map(|lookup| serde_json::to_vec(&self.lookup_row(lookup))),
            )
            .chain(
                self.file_preparations
                    .values()
                    .cloned()
                    .map(|payload| serde_json::to_vec(&self.preparation_row(payload))),
            )
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| invalid())?;
        for plan in self.legacy_plans.values() {
            rows.push(
                self.legacy_bytes(&LegacyBatch {
                    plan: Some(plan.clone()),
                    links: plan
                        .legs
                        .iter()
                        .map(|leg| (leg.id, plan.parent.clone()))
                        .collect(),
                    entries: Vec::new(),
                })?,
            );
        }
        let current_legs: std::collections::BTreeSet<_> = self
            .legacy_plans
            .values()
            .flat_map(|plan| plan.legs.iter().map(|leg| leg.id))
            .collect();
        for (id, parent) in &self.legacy_children {
            if current_legs.contains(id) {
                continue;
            }
            rows.push(self.legacy_bytes(&LegacyBatch {
                plan: None,
                links: vec![(*id, parent.clone())],
                entries: Vec::new(),
            })?);
        }
        Ok(rows)
    }
    pub(super) fn put_lookup(&mut self, mut lookup: Lookup) -> Result<(), LogError> {
        self.validate_lookup(&lookup)?;
        if self.entries.contains_key(&lookup.id) {
            return Err(invalid());
        }
        let old = self.lookups.get(&lookup.id);
        if old.is_none()
            && (self.entries.len() + self.lookups.len() + self.file_preparations.len()
                >= MAX_ENTRIES
                || self.pending_count() >= MAX_PENDING
                    && !self.file_preparations.contains_key(&lookup.id))
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
        if reference.kind == AccountGroupOperationKind::File {
            let Some(AccountGroupOperation::Send { message }) = entry.body.as_ref() else {
                return Err(invalid());
            };
            let payload = super::file_preparation::GroupFilePayload::decode(message.encoded_body())
                .ok_or_else(invalid)?;
            let descriptor = payload
                .descriptor
                .filter(|descriptor| descriptor.valid_for(reference, &payload.logical))
                .ok_or_else(invalid)?;
            entry.file_copy = Some(GroupFileCopy {
                descriptor,
                complete: true,
            });
        }
        entry.status.phase = AccountGroupOperationPhase::Committed;
        entry.status.primary_event = events
            .iter()
            .rev()
            .find(|event| {
                matches!(
                    event.kind,
                    crate::eventlog::EventKind::Message
                        | crate::eventlog::EventKind::FileManifest
                        | crate::eventlog::EventKind::React
                        | crate::eventlog::EventKind::Delete
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
    pub(in crate::node) fn account_group_file_preparation_pinned(
        &self,
        file: ConversationId,
    ) -> bool {
        self.group_operations
            .lock()
            .expect("group operation journal mutex")
            .pins_file_preparation(file)
    }
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
    /// Exact public Message identity selected from committed source intents,
    /// never from unrelated history or a caller's logical operation identifier.
    pub(in crate::node) fn account_group_fanout_message(
        &self,
        group: ConversationId,
    ) -> Result<Option<EventId>, NodeError> {
        let Some(event) = self
            .group_operations
            .lock()
            .expect("group operation journal mutex")
            .fanout_messages
            .get(group.as_bytes())
            .map(|(_, event)| *event)
        else {
            return Ok(None);
        };
        Ok(self
            .log
            .lock()
            .expect("log mutex")
            .get(&event)?
            .filter(|event| {
                event.conversation_id == group
                    && event.kind == crate::eventlog::EventKind::Message
                    && event.author.ed25519_pub() == &self.identity.public().ed25519_pub
            })
            .map(|event| event.id))
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
    fn legacy_entry(
        node: &Node,
        group: ConversationId,
        executor: PublicIdentity,
        source: bool,
    ) -> Entry {
        use crate::node::account_group::{
            AccountGroupExecution, AuthorityExpectation, MembershipLifetime,
        };
        let executor = AccountGroupSource {
            device: executor.clone(),
            role: AccountGroupRole::Member,
            lifetime: MembershipLifetime {
                member: executor.clone(),
                membership: EventId::new([9; 32]),
                acceptance: None,
            },
            authority: AuthorityExpectation {
                authority: None,
                epoch: 1,
            },
            execution: AccountGroupExecution::Local,
        };
        let reference = OperationReference::new(
            node,
            node.new_account_group_operation_id(&node.account_id())
                .unwrap(),
            group,
            &AccountGroupOperation::LeaveAccount,
            executor,
        );
        let phase = if source {
            AccountGroupOperationPhase::Accepted
        } else {
            AccountGroupOperationPhase::AwaitingAcceptance
        };
        Entry {
            status: AccountGroupOperationStatus {
                id: reference.id,
                account_id: node.account_id(),
                group,
                kind: AccountGroupOperationKind::LeaveAccount,
                phase: phase.clone(),
                executions: vec![AccountGroupOperationExecution {
                    device_id: reference.executor.device.user_id(),
                    phase,
                    committed_events: vec![],
                }],
                committed_events: vec![],
                primary_event: None,
            },
            reference,
            body: Some(AccountGroupOperation::LeaveAccount),
            file_copy: None,
            source,
            cancel: false,
            sequence: 0,
        }
    }
    #[test]
    fn legacy_plan_batch_is_atomic_under_append_failure_and_reserves_all_future_children() {
        let (_dir, node, _account, template, _) = fixture();
        let group = template.reference.group;
        let parent = legacy_entry(&node, group, node.identity.public(), true);
        let first = legacy_entry(&node, group, DeviceIdentity::generate().public(), false);
        let second = legacy_entry(&node, group, DeviceIdentity::generate().public(), false);
        let plan = LegacyLeavePlan {
            parent: parent.reference.clone(),
            legs: vec![first.reference.clone(), second.reference.clone()],
            roll_forward: false,
        };
        let mut journal = node.group_operations.lock().unwrap();
        journal.put(parent).unwrap();
        let before = (journal.sequence, journal.records, journal.live);
        journal.file.fail_appends_for_test(true).unwrap();
        assert!(
            journal
                .put_legacy_plan(plan.clone(), vec![first.clone(), second.clone()])
                .is_err()
        );
        journal.file.fail_appends_for_test(false).unwrap();
        assert_eq!((journal.sequence, journal.records, journal.live), before);
        assert!(journal.legacy_plans.is_empty() && journal.legacy_children.is_empty());
        assert!(!journal.entries.contains_key(&first.reference.id));
        journal.live_limit = journal.live + journal.size(&first).unwrap();
        assert!(
            journal
                .put_legacy_plan(plan.clone(), vec![first.clone(), second.clone()])
                .is_err()
        );
        assert_eq!((journal.sequence, journal.records, journal.live), before);
        journal.live_limit = MAX_LIVE;
        journal
            .put_legacy_plan(plan, vec![first.clone(), second.clone()])
            .unwrap();
        assert!(
            journal.entries.contains_key(&first.reference.id)
                && journal.entries.contains_key(&second.reference.id)
        );
        assert_eq!(journal.legacy_children.len(), 2);
    }
    #[test]
    fn legacy_plan_retries_only_effect_free_retired_leg_and_compacts_all_original_links() {
        let (dir, node, account, template, _) = fixture();
        let group = template.reference.group;
        let parent = legacy_entry(&node, group, node.identity.public(), true);
        let child = legacy_entry(&node, group, DeviceIdentity::generate().public(), false);
        let mut plan = LegacyLeavePlan {
            parent: parent.reference.clone(),
            legs: vec![child.reference.clone()],
            roll_forward: false,
        };
        let mut journal = node.group_operations.lock().unwrap();
        journal.put(parent).unwrap();
        journal
            .put_legacy_plan(plan.clone(), vec![child.clone()])
            .unwrap();
        let mut retry = legacy_entry(&node, group, child.reference.executor.device.clone(), false);
        retry.reference = OperationReference::new(
            &node,
            retry.reference.id,
            group,
            &AccountGroupOperation::LeaveAccount,
            child.reference.executor.clone(),
        );
        let mut next = plan.clone();
        next.legs[0] = retry.reference.clone();
        assert!(
            journal
                .put_legacy_plan(next.clone(), vec![retry.clone()])
                .is_err(),
            "a live original child can still commit"
        );
        let mut retired = journal.entries.get(&child.reference.id).unwrap().clone();
        retired.status.phase = AccountGroupOperationPhase::Cancelled;
        retired.status.executions[0].phase = AccountGroupOperationPhase::Cancelled;
        retired.body = None;
        journal.put(retired).unwrap();
        journal
            .put_legacy_plan(next.clone(), vec![retry.clone()])
            .unwrap();
        next.roll_forward = true;
        journal.put_legacy_plan(next.clone(), vec![]).unwrap();
        plan.legs[0] = retry.reference.clone();
        assert!(
            journal.put_legacy_plan(plan, vec![]).is_err(),
            "roll-forward cannot be undone"
        );
        journal
            .test_file_preparation_pressure(MAX_LIVE, true)
            .unwrap();
        let live = journal.live;
        drop(journal);
        let reopened = OperationJournal::open(
            dir.path(),
            "pw",
            account.public().ed25519_pub,
            node.identity.public(),
        )
        .unwrap();
        assert_eq!(reopened.live, live);
        assert!(reopened.legacy_plans[&next.parent.id].roll_forward);
        assert!(reopened.legacy_plans[&next.parent.id].legs[0].same(&retry.reference));
        assert!(reopened.legacy_children[&child.reference.id].same(&next.parent));
        assert!(reopened.legacy_children[&retry.reference.id].same(&next.parent));
        assert!(reopened.entries[&child.reference.id].terminal());
    }
    #[test]
    fn legacy_child_link_and_placeholder_survive_reopen_without_downgrading_source_progress() {
        let (dir, coordinator, account, template, _) = fixture();
        let source = open(
            &dir.path().join("child"),
            &account,
            DeviceIdentity::generate(),
        );
        let parent = legacy_entry(
            &coordinator,
            template.reference.group,
            coordinator.identity.public(),
            true,
        )
        .reference;
        let child = legacy_entry(
            &coordinator,
            template.reference.group,
            source.identity.public(),
            false,
        );
        let mut journal = source.group_operations.lock().unwrap();
        journal.file.fail_appends_for_test(true).unwrap();
        assert!(
            journal
                .put_legacy_child(parent.clone(), child.clone())
                .is_err()
        );
        assert!(journal.legacy_children.is_empty() && journal.entries.is_empty());
        journal.file.fail_appends_for_test(false).unwrap();
        journal
            .put_legacy_child(parent.clone(), child.clone())
            .unwrap();
        let mut accepted = child.clone();
        accepted.source = true;
        accepted.status.phase = AccountGroupOperationPhase::Accepted;
        accepted.status.executions[0].phase = AccountGroupOperationPhase::Accepted;
        journal.put(accepted).unwrap();
        journal
            .put_legacy_child(parent.clone(), child.clone())
            .unwrap();
        assert!(journal.entries[&child.reference.id].source);
        journal
            .test_file_preparation_pressure(MAX_LIVE, true)
            .unwrap();
        drop(journal);
        let reopened = OperationJournal::open(
            &dir.path().join("child"),
            "pw",
            account.public().ed25519_pub,
            source.identity.public(),
        )
        .unwrap();
        assert!(reopened.legacy_children[&child.reference.id].same(&parent));
        assert!(reopened.entries[&child.reference.id].source);
    }
    #[test]
    fn source_ready_turns_reach_ninth_pending_without_rewriting_status_or_foreign_work() {
        use std::collections::{BTreeMap, BTreeSet};
        let (_dir, node, account, template, _) = fixture();
        let mut journal = node.group_operations.lock().unwrap();
        let mut expected = BTreeSet::new();
        for index in 0..11 {
            let mut entry = template.clone();
            let mut executor = entry.reference.executor.clone();
            if index == 9 {
                executor.device = DeviceIdentity::generate().public();
                executor.lifetime.member = executor.device.clone();
                entry.source = false;
            }
            entry.reference = OperationReference::new(
                &node,
                node.new_account_group_operation_id(&account.account_id())
                    .unwrap(),
                template.reference.group,
                template.body.as_ref().unwrap(),
                executor,
            );
            entry.status.id = entry.reference.id;
            if index == 10 {
                entry.status.phase = AccountGroupOperationPhase::Cancelled;
            } else if index < 9 {
                expected.insert(entry.reference.id);
            }
            journal.put(entry).unwrap();
        }
        let before: BTreeMap<_, _> = journal
            .entries
            .iter()
            .map(|(id, entry)| (*id, serde_json::to_vec(entry).unwrap()))
            .collect();
        let revision = (journal.sequence, journal.records, journal.bytes);
        assert!(
            journal
                .take_source_ready(&node.identity.public(), 0)
                .is_empty()
        );
        let first = journal.take_source_ready(&node.identity.public(), 8);
        let second = journal.take_source_ready(&node.identity.public(), 8);
        assert_eq!(first.len(), 8);
        assert_eq!(second.len(), 8);
        let observed: BTreeSet<_> = first
            .iter()
            .chain(&second)
            .map(|entry| entry.reference.id)
            .collect();
        assert_eq!(
            observed, expected,
            "a waiting first cohort must not permanently exclude the ninth source request"
        );
        assert!(
            first.iter().chain(&second).all(|entry| !entry.terminal()
                && entry.reference.executor.device == node.identity.public())
        );
        assert_eq!(
            (journal.sequence, journal.records, journal.bytes),
            revision,
            "turn selection must be transient, without a new durable status or original ID"
        );
        let after: BTreeMap<_, _> = journal
            .entries
            .iter()
            .map(|(id, entry)| (*id, serde_json::to_vec(entry).unwrap()))
            .collect();
        assert_eq!(after, before);
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
            file_copy: None,
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
        node.persist_account_adoption(
            &destination.public(),
            || Ok(()),
            || Ok(destination.public()),
        )
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
        node.persist_account_adoption(
            &destination.public(),
            || Ok(()),
            || Ok(destination.public()),
        )
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
        assert_eq!(node.log.lock().unwrap().events(&group).unwrap().len(), 1);
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
                .unwrap()
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
        assert_eq!(node.account_group_fanout_sequence(group), None);
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
        assert!(first[0].0 > 0);
        assert_eq!(node.account_group_fanout_sequence(group), Some(first[0].0));
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
        assert_eq!(node.account_group_fanout_sequence(group), Some(later[0].0));
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
    fn source_message_fanout_rebuilds_latest_exact_effect_after_control_and_reopen() {
        let (dir, node, account, template, control) = fixture();
        let group = control.conversation_id;
        let mut parent = control.parents[0];
        let mut latest = None;
        for (offset, kind) in [
            AccountGroupOperationKind::Text,
            AccountGroupOperationKind::Sticker,
        ]
        .into_iter()
        .enumerate()
        {
            let body = if kind == AccountGroupOperationKind::Text {
                crate::node::message::MessageBody::new(b"indexed text".to_vec(), None)
            } else {
                crate::node::message::MessageBody::sticker("1f602".into(), b"sticker".to_vec())
            };
            let operation = AccountGroupOperation::Send {
                message: PreparedAccountGroupMessage::from_plaintext(kind, body.encode()).unwrap(),
            };
            let mut entry = template.clone();
            entry.reference = OperationReference::new(
                &node,
                node.new_account_group_operation_id(&account.account_id())
                    .unwrap(),
                group,
                &operation,
                template.reference.executor.clone(),
            );
            entry.status.id = entry.reference.id;
            entry.status.kind = kind;
            entry.body = Some(operation);
            let number = offset as u64 + 2;
            let effect = Event::new(
                &node.identity,
                group,
                number,
                vec![parent],
                number,
                number,
                EventKind::Message,
                b"signed index fixture ciphertext".to_vec(),
            );
            assert_ne!(effect.id.as_bytes(), entry.reference.id.as_bytes());
            node.log
                .lock()
                .unwrap()
                .append_durable(effect.clone())
                .unwrap();
            {
                let mut journal = node.group_operations.lock().unwrap();
                journal.put(entry.clone()).unwrap();
                journal
                    .complete(&entry.reference, std::slice::from_ref(&effect))
                    .unwrap();
            }
            parent = effect.id;
            latest = Some(effect.id);
            assert_eq!(node.account_group_fanout_message(group).unwrap(), latest);
        }
        let control = Event::new(
            &node.identity,
            group,
            4,
            vec![parent],
            4,
            4,
            EventKind::ChannelRename,
            b"later control".to_vec(),
        );
        node.log
            .lock()
            .unwrap()
            .append_durable(control.clone())
            .unwrap();
        {
            let mut journal = node.group_operations.lock().unwrap();
            journal.put(template.clone()).unwrap();
            journal.complete(&template.reference, &[control]).unwrap();
        }
        assert_eq!(node.account_group_fanout_message(group).unwrap(), latest);
        let public = node.identity.public();
        drop(node);
        let reopened =
            OperationJournal::open(dir.path(), "pw", account.public().ed25519_pub, public).unwrap();
        assert_eq!(
            reopened
                .fanout_messages
                .get(group.as_bytes())
                .map(|(_, event)| *event),
            latest
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
        assert!(node.log.lock().unwrap().get(&effect.id).unwrap().is_some());
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
            .unwrap()
            .into_iter()
            .filter(|event| event.kind == EventKind::ChannelRename)
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
