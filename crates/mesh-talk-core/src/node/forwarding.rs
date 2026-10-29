//! Canonical source forwarding and encrypted, resumable local operation records.
use super::delivery_store::RequestRecord;
use super::outgoing_message::{MessageDestination, OutgoingMessageRequest};
use super::{FavoriteMessage, FavoriteMetadata, FavoriteSource, Node, NodeError};
use crate::{
    eventlog::{LogError, event::EventKind},
    storage::record_log::EncryptedRecordLog,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, path::Path, sync::Arc};
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForwardTarget {
    pub kind: String,
    pub target: String,
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ForwardRequest {
    pub operation_id: String,
    pub source: FavoriteSource,
    pub destinations: Vec<ForwardTarget>,
    pub mode: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForwardInspection {
    pub messages: Vec<FavoriteMessage>,
    pub source_label: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForwardItem {
    pub target: ForwardTarget,
    pub source_id: Option<String>,
    pub message_id: Option<String>,
    pub status: String,
    pub error: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForwardResult {
    pub operation_id: String,
    pub items: Vec<ForwardItem>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ForwardBatch {
    pub request: ForwardRequest,
    pub result: ForwardResult,
}
#[derive(Serialize, Deserialize)]
enum ForwardRecord {
    Batch(ForwardBatch),
    Retired(String, [u8; 32]),
}
pub(super) struct ForwardStore {
    file: EncryptedRecordLog<ForwardRecord>,
    batches: BTreeMap<String, ForwardBatch>,
    fingerprints: BTreeMap<String, [u8; 32]>,
    writes: usize,
}
fn invalid(s: &str) -> NodeError {
    NodeError::InvalidInput(s.into())
}
fn fingerprint(request: &ForwardRequest) -> [u8; 32] {
    Sha256::digest(serde_json::to_vec(request).expect("forward request encoding")).into()
}
const FORWARD_RETRY_WINDOW_MS: u64 = 7 * 24 * 60 * 60 * 1000;
fn operation_time(id: &str) -> Option<u64> {
    id.split('-').next()?.parse().ok()
}
fn operation_fresh(id: &str, now: u64) -> bool {
    operation_time(id).is_some_and(|created| {
        created <= now.saturating_add(60_000)
            && now.saturating_sub(created) < FORWARD_RETRY_WINDOW_MS
    })
}
impl ForwardStore {
    pub(super) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (file, records) = EncryptedRecordLog::<ForwardRecord>::open(path, password, b"MTFWD1")?;
        let mut batches = BTreeMap::new();
        let mut fingerprints = BTreeMap::new();
        for record in records {
            match record {
                ForwardRecord::Batch(batch) => {
                    fingerprints.insert(
                        batch.request.operation_id.clone(),
                        fingerprint(&batch.request),
                    );
                    batches.insert(batch.request.operation_id.clone(), batch);
                }
                ForwardRecord::Retired(id, digest) => {
                    batches.remove(&id);
                    fingerprints.insert(id, digest);
                }
            }
        }
        let now = super::node::now_millis();
        fingerprints.retain(|id, _| operation_fresh(id, now));
        batches.retain(|id, _| fingerprints.contains_key(id));
        if batches.len() > 128 || fingerprints.len() > 4096 {
            return Err(LogError::CorruptFile("Excess forwarding history".into()));
        }
        Ok(Self {
            file,
            batches,
            fingerprints,
            writes: 0,
        })
    }
    fn save(&mut self, batch: &ForwardBatch) -> Result<(), NodeError> {
        let now = super::node::now_millis();
        self.fingerprints.retain(|id, _| operation_fresh(id, now));
        self.batches
            .retain(|id, _| self.fingerprints.contains_key(id));
        if !self.fingerprints.contains_key(&batch.request.operation_id)
            && self.fingerprints.len() >= 4096
        {
            return Err(invalid("Forwarding operation history budget reached"));
        }
        if !self.batches.contains_key(&batch.request.operation_id) && self.batches.len() >= 128 {
            let retired = self
                .batches
                .iter()
                .find(|(_, b)| {
                    !b.result.items.is_empty()
                        && b.result.items.iter().all(|i| i.status == "accepted")
                })
                .map(|(id, _)| id.clone())
                .ok_or_else(|| invalid("Too many unfinished forwarding operations"))?;
            let digest = self.fingerprints[&retired];
            self.file
                .append_durable(&ForwardRecord::Retired(retired.clone(), digest))
                .map_err(NodeError::Log)?;
            self.batches.remove(&retired);
        }
        self.file
            .append_durable(&ForwardRecord::Batch(batch.clone()))
            .map_err(NodeError::Log)?;
        self.fingerprints.insert(
            batch.request.operation_id.clone(),
            fingerprint(&batch.request),
        );
        self.batches
            .insert(batch.request.operation_id.clone(), batch.clone());
        self.writes += 1;
        if self.writes >= 32 {
            let records = self
                .fingerprints
                .iter()
                .map(|(id, digest)| {
                    self.batches
                        .get(id)
                        .cloned()
                        .map(ForwardRecord::Batch)
                        .unwrap_or_else(|| ForwardRecord::Retired(id.clone(), *digest))
                })
                .collect::<Vec<_>>();
            self.file.rewrite(&records).map_err(NodeError::Log)?;
            self.writes = 0;
        }
        Ok(())
    }
}

impl Node {
    pub fn inspect_forward_messages(
        &self,
        source: &FavoriteSource,
    ) -> Result<ForwardInspection, NodeError> {
        if source.message_ids.is_empty()
            || source.message_ids.len() > 100
            || source
                .message_ids
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != source.message_ids.len()
        {
            return Err(invalid("Select 1 to 100 distinct messages"));
        }
        let snapshot = self.capture_favorite(source.clone(), FavoriteMetadata::default())?;
        for message in &snapshot.messages {
            if message
                .file
                .as_ref()
                .is_some_and(|f| f.name.to_ascii_lowercase().ends_with(".mtchat"))
            {
                return Err(invalid("Nested chat records cannot be forwarded"));
            }
        }
        Ok(ForwardInspection {
            messages: snapshot.messages,
            source_label: snapshot.source_label,
        })
    }
    pub fn list_forward_batches(&self) -> Vec<ForwardBatch> {
        self.forwarding
            .lock()
            .expect("forwarding lock")
            .batches
            .values()
            .rev()
            .take(128)
            .cloned()
            .collect()
    }
    pub async fn forward_messages_if(
        self: &Arc<Self>,
        request: ForwardRequest,
        mut authorize: impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>
        + Send,
    ) -> Result<ForwardResult, NodeError> {
        let _gate = self.forwarding_gate.lock().await;
        authorize(&mut || Ok(()))?;
        if !operation_fresh(&request.operation_id, super::node::now_millis())
            || request.operation_id.is_empty()
            || request.operation_id.len() > 64
            || !request
                .operation_id
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || request.destinations.is_empty()
            || request.destinations.len() > 5
            || !["individual", "merged"].contains(&request.mode.as_str())
        {
            return Err(invalid("Invalid forwarding request"));
        }
        let mut destinations = std::collections::BTreeSet::new();
        for target in &request.destinations {
            destination(target)?;
            if !destinations.insert((&target.kind, &target.target)) {
                return Err(invalid("Duplicate forwarding target"));
            }
        }
        {
            let store = self.forwarding.lock().expect("forwarding lock");
            if let Some(digest) = store.fingerprints.get(&request.operation_id) {
                if digest != &fingerprint(&request) {
                    return Err(NodeError::RequestConflict);
                }
                if !store.batches.contains_key(&request.operation_id) {
                    return Err(invalid(
                        "This completed forwarding operation has left recent history",
                    ));
                }
            }
        }
        let existing = self
            .forwarding
            .lock()
            .expect("forwarding lock")
            .batches
            .get(&request.operation_id)
            .cloned();
        if let Some(old) = &existing {
            if old.request != request {
                return Err(NodeError::RequestConflict);
            }
            if old.result.items.len()
                == request.destinations.len()
                    * if request.mode == "merged" {
                        1
                    } else {
                        request.source.message_ids.len()
                    }
                && old
                    .result
                    .items
                    .iter()
                    .all(|item| item.status == "accepted")
            {
                return Ok(old.result.clone());
            }
        }
        let snapshot = self.inspect_forward_messages(&request.source)?;
        if snapshot.messages.is_empty() || snapshot.messages.len() > 100 {
            return Err(invalid("Select between 1 and 100 messages"));
        }
        if request.mode == "merged" && snapshot.messages.iter().any(|m| m.expires_at_ms.is_some()) {
            return Err(invalid("Expiring messages cannot be merged"));
        }
        let mut batch = existing.unwrap_or_else(|| ForwardBatch {
            request: request.clone(),
            result: ForwardResult {
                operation_id: request.operation_id.clone(),
                items: Vec::new(),
            },
        });
        for target in &request.destinations {
            for (index, message) in snapshot.messages.iter().enumerate() {
                if request.mode == "merged" && index > 0 {
                    break;
                }
                let source_id = if request.mode == "merged" {
                    None
                } else {
                    Some(message.id.clone())
                };
                if !batch
                    .result
                    .items
                    .iter()
                    .any(|item| item.target == *target && item.source_id == source_id)
                {
                    batch.result.items.push(ForwardItem {
                        target: target.clone(),
                        source_id,
                        message_id: None,
                        status: "failed".into(),
                        error: Some("Not yet accepted".into()),
                    });
                }
            }
        }
        authorize(&mut || {
            self.forwarding
                .lock()
                .expect("forwarding lock")
                .save(&batch)
        })?;
        let merged = if request.mode == "merged" {
            Some(self.forward_bundle(&snapshot)?)
        } else {
            None
        };
        for target in &request.destinations {
            for (index, message) in snapshot.messages.iter().enumerate() {
                if merged.is_some() && index > 0 {
                    break;
                }
                let source_id = if merged.is_some() {
                    None
                } else {
                    Some(message.id.clone())
                };
                if batch.result.items.iter().any(|item| {
                    item.target == *target
                        && item.source_id == source_id
                        && item.status == "accepted"
                }) {
                    continue;
                }
                let key = Sha256::digest(
                    serde_json::to_vec(&(&request.operation_id, target, &source_id))
                        .map_err(|e| invalid(&e.to_string()))?,
                );
                let id: [u8; 32] = key.into();
                let previous = batch
                    .result
                    .items
                    .iter()
                    .find(|item| item.target == *target && item.source_id == source_id)
                    .and_then(|item| item.error.clone())
                    .unwrap_or_default();
                if (previous == "Acceptance started"
                    || previous.starts_with("Uncertain acceptance"))
                    && self
                        .delivery
                        .lock()
                        .expect("delivery lock")
                        .request(&id)
                        .is_none()
                {
                    if let Some(item) = batch
                        .result
                        .items
                        .iter_mut()
                        .find(|item| item.target == *target && item.source_id == source_id)
                    {
                        item.error =
                            Some("Uncertain acceptance; cannot safely retry automatically".into());
                    }
                    authorize(&mut || {
                        self.forwarding
                            .lock()
                            .expect("forwarding lock")
                            .save(&batch)
                    })?;
                    continue;
                }
                if let Some(item) = batch
                    .result
                    .items
                    .iter_mut()
                    .find(|item| item.target == *target && item.source_id == source_id)
                {
                    item.error = Some("Acceptance started".into());
                }
                authorize(&mut || {
                    self.forwarding
                        .lock()
                        .expect("forwarding lock")
                        .save(&batch)
                })?;
                let mut authorize_item = |commit: &mut dyn FnMut() -> Result<(), NodeError>| {
                    authorize(&mut || {
                        self.inspect_forward_messages(&request.source)?;
                        commit()
                    })
                };
                let result = async {
                    // Revalidate canonical source policy immediately before each acceptance.
                    authorize_item(&mut || {
                        self.inspect_forward_messages(&request.source).map(|_| ())
                    })?;
                    if let Some(bytes) = &merged {
                        self.forward_bytes_once(
                            target,
                            bytes,
                            "Chat records.mtchat",
                            false,
                            id,
                            &mut authorize_item,
                        )
                        .await
                    } else if let Some(file) = &message.file {
                        if message.expires_at_ms.is_some() {
                            return Err(invalid("Expiring attachments cannot be forwarded"));
                        }
                        if file.size > 48 * 1024 * 1024 {
                            return Err(invalid("Forwarding attachment exceeds 48 MiB budget"));
                        }
                        let conv = super::favorites::conversation(&file.file_conv)
                            .ok_or_else(|| invalid("Invalid source attachment"))?;
                        let bytes = self
                            .read_media(conv)
                            .map(Ok)
                            .unwrap_or_else(|| self.read_file(conv))?;
                        self.forward_bytes_once(
                            target,
                            &bytes,
                            &file.name,
                            file.media,
                            id,
                            &mut authorize_item,
                        )
                        .await
                    } else {
                        let accepted = self
                            .enqueue_message_once_if(
                                &OutgoingMessageRequest {
                                    request_id: id,
                                    destination: destination(target)?,
                                    text: message.text.as_bytes().to_vec(),
                                    reply_to: None,
                                    expires_at_ms: message.expires_at_ms,
                                },
                                &mut authorize_item,
                            )
                            .await?;
                        Ok(accepted.message_id.to_hex())
                    }
                }
                .await;
                batch
                    .result
                    .items
                    .retain(|item| !(item.target == *target && item.source_id == source_id));
                batch.result.items.push(match result {
                    Ok(id) => ForwardItem {
                        target: target.clone(),
                        source_id,
                        message_id: Some(id),
                        status: "accepted".into(),
                        error: None,
                    },
                    Err(e) => ForwardItem {
                        target: target.clone(),
                        source_id,
                        message_id: None,
                        status: "failed".into(),
                        error: Some(e.to_string()),
                    },
                });
                authorize(&mut || {
                    self.forwarding
                        .lock()
                        .expect("forwarding lock")
                        .save(&batch)
                })?;
            }
        }
        self.account_sync_notify.notify_one();
        Ok(batch.result)
    }
    async fn forward_bytes_once(
        self: &Arc<Self>,
        target: &ForwardTarget,
        bytes: &[u8],
        name: &str,
        media: bool,
        id: [u8; 32],
        authorize: &mut (
                 impl FnMut(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError> + Send
             ),
    ) -> Result<String, NodeError> {
        let _gate = self.outgoing_requests_gate.lock().await;
        let dest = destination(target)?;
        let digest: [u8; 32] =
            Sha256::digest(serde_json::to_vec(&(target, id)).map_err(|e| invalid(&e.to_string()))?)
                .into();
        if let Some(record) = self
            .delivery
            .lock()
            .expect("delivery lock")
            .request(&id)
            .cloned()
        {
            if record.digest != digest {
                return Err(NodeError::RequestConflict);
            }
            return Ok(record.message_id.to_hex());
        }
        match &dest {
            MessageDestination::Account(account) => {
                if account != &self.account_id()
                    && !self
                        .account_fanout_targets(account)
                        .iter()
                        .any(|p| p.account_id.as_deref() == Some(account))
                {
                    return Err(invalid("Unknown forwarding account"));
                }
                if account != &self.account_id() {
                    self.initiate_contact_locally_if(account, authorize).await?;
                }
            }
            MessageDestination::Channel(channel) => {
                let book = self.channels.lock().expect("channels lock");
                if book
                    .state(channel)
                    .is_none_or(|state| !self.channel_identity_is_current(state))
                {
                    return Err(invalid("Not an accepted channel member"));
                }
            }
        }
        authorize(&mut || Ok(()))?;
        let dir = tempfile::tempdir().map_err(|e| NodeError::File(e.to_string()))?;
        let safe = Path::new(name)
            .file_name()
            .and_then(|n| n.to_str())
            .filter(|n| *n == name && !n.is_empty() && *n != "." && *n != ".." && !n.contains('\\'))
            .ok_or_else(|| invalid("Invalid attachment name"))?;
        let path = dir.path().join(safe);
        std::fs::write(&path, bytes).map_err(|e| NodeError::File(e.to_string()))?;
        let (host, account) = match &dest {
            MessageDestination::Account(account) => (
                super::conversation::account_conversation_id(&self.account_id(), account),
                Some(account.clone()),
            ),
            MessageDestination::Channel(channel) => (*channel, None),
        };
        let (manifest, file_conv) = self
            .stage_file_for_host_blocking(
                &path,
                if media {
                    crate::file::FileKind::Media
                } else {
                    crate::file::FileKind::File
                },
                |_| {},
                host,
                account,
                None,
            )
            .await?;
        let record = RequestRecord {
            request_id: id,
            digest,
            destination: dest.clone(),
            message_id: crate::eventlog::EventId::new([0; 32]),
            accepted_at_ms: super::node::now_millis(),
        };
        let result = match dest {
            MessageDestination::Account(account) => {
                let peers = self
                    .account_fanout_targets(&account)
                    .into_iter()
                    .filter(|p| p.account_id.as_deref() != Some(self.account_id().as_str()))
                    .collect::<Vec<_>>();
                if account != self.account_id()
                    && !peers
                        .iter()
                        .any(|p| p.account_id.as_deref() == Some(&account))
                {
                    return Err(invalid("Unknown forwarding account"));
                }
                let host =
                    super::conversation::account_conversation_id(&self.account_id(), &account);
                let mut accepted = None;
                authorize(&mut || {
                    accepted = Some(self.accept_staged_manifest_authorized(
                        &peers,
                        host,
                        Some(account.clone()),
                        &manifest,
                        None,
                        None,
                        Some(record.clone()),
                    )?);
                    Ok(())
                })?;
                accepted.ok_or_else(|| invalid("Forward acceptance was not executed"))?
            }
            MessageDestination::Channel(channel) => {
                let result = self.enqueue_channel_payload_if(
                    channel,
                    manifest.encode(),
                    EventKind::FileManifest,
                    Some(record),
                    authorize,
                )?;
                self.queue_group_file_delivery(file_conv, result);
                result
            }
        };
        Ok(result.to_hex())
    }
    fn forward_bundle(&self, snapshot: &ForwardInspection) -> Result<Vec<u8>, NodeError> {
        use base64::Engine;
        let mut messages = Vec::new();
        let mut budget = 0u64;
        for message in &snapshot.messages {
            if message.sender_name.len() > 256 || message.text.len() > 256 * 1024 {
                return Err(invalid("Chat record text exceeds budget"));
            }
            let file = if let Some(file) = &message.file {
                budget = budget.saturating_add(file.size);
                if budget > 48 * 1024 * 1024 || file.name.len() > 255 {
                    return Err(invalid("Chat record attachments exceed budget"));
                }
                let conv = super::favorites::conversation(&file.file_conv)
                    .ok_or_else(|| invalid("Invalid file"))?;
                let bytes = self
                    .read_media(conv)
                    .map(Ok)
                    .unwrap_or_else(|| self.read_file(conv))?;
                Some(
                    serde_json::json!({"name":file.name,"mime":file.mime,"size":bytes.len(),"data_base64":base64::engine::general_purpose::STANDARD.encode(bytes)}),
                )
            } else {
                None
            };
            messages.push(serde_json::json!({"sender_name":message.sender_name,"wall_clock":message.wall_clock,"text":message.text,"file":file}));
        }
        let mut title = snapshot.source_label.clone();
        while title.len() > 512 {
            title.pop();
        }
        let bytes=serde_json::to_vec(&serde_json::json!({"format":"MTCHAT1","title":title,"created_at_ms":snapshot.messages.iter().map(|m|m.wall_clock).max().unwrap_or(0),"messages":messages})).map_err(|e|invalid(&e.to_string()))?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(invalid("Chat record exceeds 64 MiB"));
        }
        record::validate(&bytes)?;
        Ok(bytes)
    }
}
fn destination(target: &ForwardTarget) -> Result<MessageDestination, NodeError> {
    match target.kind.as_str() {
        "account" if target.target.len() == 32 && hex::decode(&target.target).is_ok() => {
            Ok(MessageDestination::Account(target.target.clone()))
        }
        "channel" => super::favorites::conversation(&target.target)
            .map(MessageDestination::Channel)
            .ok_or_else(|| invalid("Invalid channel")),
        _ => Err(invalid("Invalid forwarding destination")),
    }
}

impl Node {
    pub fn forwarded_attachment_bytes(
        &self,
        file_conv: &str,
        index: usize,
    ) -> Result<Vec<u8>, NodeError> {
        let conv = super::favorites::conversation(file_conv)
            .ok_or_else(|| invalid("Invalid archive identity"))?;
        {
            let files = self.files.lock().expect("files lock");
            let manifest = files
                .manifest(&conv)
                .ok_or_else(|| invalid("Archive unavailable"))?;
            if manifest.size() > 64 * 1024 * 1024 {
                return Err(invalid("Archive exceeds budget"));
            }
        }
        let bytes = self
            .read_media(conv)
            .map(Ok)
            .unwrap_or_else(|| self.read_file(conv))?;
        if bytes.len() > 64 * 1024 * 1024 {
            return Err(invalid("Archive exceeds budget"));
        }
        record::attachment(&bytes, index)
    }
}

#[cfg(test)]
mod tests;

#[path = "forwarding/record.rs"]
mod record;
