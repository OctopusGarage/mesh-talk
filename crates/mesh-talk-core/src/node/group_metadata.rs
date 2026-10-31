//! Owner-published group metadata. Each publication has an independent content key;
//! current accepted holders can release only the current field to new members.
//! These are durable group controls, independent of ordinary message retention.
use super::{Node, NodeError, channel::SealedPayload};
use crate::{
    channel::{GroupKey, open_channel_message, seal_channel_message},
    eventlog::{ConversationId, Event, EventId, EventKind, LogError},
    identity::device::PublicIdentity,
};
use base64::Engine;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

mod indexed;
pub(in crate::node) use indexed::{initialize as initialize_indexed, retire_index_on};

const MAX_CONTROL: usize = 60 * 1024;
const MAX_COVER: usize = 32 * 1024;
const MAX_ANNOUNCEMENT: usize = 4096;
const MAX_RECIPIENTS: usize = 128;
const MAX_HISTORY: usize = 20;

#[derive(Default)]
pub(super) struct MetadataCache {
    values: std::collections::VecDeque<CachedMetadata>,
    reconciled: std::collections::VecDeque<(ConversationId, indexed::Stamp)>,
    after_channel: Option<ConversationId>,
    preparations: indexed::PreparationCache,
}
struct CachedMetadata {
    channel: ConversationId,
    source: indexed::Stamp,
    account: String,
    identity: PublicIdentity,
    value: ChannelMetadata,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ChannelMetadata {
    /// Selected history or authority is still being indexed; no partial values are exposed.
    pub query_pending: bool,
    pub cover: Option<ChannelMetadataValue>,
    /// A validated current publication exists, but this device still needs its content key.
    pub cover_pending: bool,
    pub announcement: Option<ChannelMetadataValue>,
    pub announcement_pending: bool,
    pub announcement_history: Vec<ChannelMetadataValue>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ChannelMetadataValue {
    pub revision: String,
    pub content: Option<String>,
    pub author: String,
    pub authored_at: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
enum Field {
    Cover,
    Announcement,
}
#[derive(Serialize, Deserialize)]
enum Control {
    Publish {
        field: Field,
        keys: SealedPayload,
        ciphertext: Vec<u8>,
    },
    Release {
        publication: EventId,
        recipient: PublicIdentity,
        sealed_key: Vec<u8>,
    },
}
fn encode(control: &Control) -> Result<Vec<u8>, NodeError> {
    let mut bytes = b"MTGM1".to_vec();
    bytes.extend(
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(control)
            .map_err(|e| NodeError::InvalidInput(e.to_string()))?,
    );
    if bytes.len() > MAX_CONTROL {
        return Err(NodeError::InvalidInput(
            "group metadata exceeds wire limit".into(),
        ));
    }
    Ok(bytes)
}
fn decode(event: &Event) -> Option<Control> {
    if event.kind != EventKind::ChannelMetadata || event.ciphertext.len() > MAX_CONTROL {
        return None;
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_CONTROL as u64)
        .reject_trailing_bytes()
        .deserialize(event.ciphertext.strip_prefix(b"MTGM1")?)
        .ok()
}
fn validate_content(field: Field, content: Option<&str>) -> Result<(), NodeError> {
    let Some(content) = content else {
        return Ok(());
    };
    if field == Field::Announcement {
        if content.len() > MAX_ANNOUNCEMENT || content.trim().is_empty() || content.contains('\0') {
            return Err(NodeError::InvalidInput(
                "announcement must contain text and fit within 4096 UTF-8 bytes".into(),
            ));
        }
        return Ok(());
    }
    if content.len() > MAX_COVER {
        return Err(NodeError::InvalidInput(
            "group photo must fit within 32768 bytes".into(),
        ));
    }
    let Some((mime, data)) = content.split_once(";base64,") else {
        return Err(NodeError::InvalidInput("invalid group photo".into()));
    };
    let format = match mime {
        "data:image/jpeg" => image::ImageFormat::Jpeg,
        "data:image/png" => image::ImageFormat::Png,
        "data:image/webp" => image::ImageFormat::WebP,
        _ => {
            return Err(NodeError::InvalidInput(
                "unsupported group photo format".into(),
            ));
        }
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|_| NodeError::InvalidInput("invalid group photo encoding".into()))?;
    let dimensions = image::ImageReader::with_format(std::io::Cursor::new(&bytes), format)
        .into_dimensions()
        .map_err(|_| NodeError::InvalidInput("invalid group photo dimensions".into()))?;
    if dimensions.0 == 0
        || dimensions.1 == 0
        || dimensions.0 > 256
        || dimensions.1 > 256
        || image::load_from_memory_with_format(&bytes, format).is_err()
    {
        return Err(NodeError::InvalidInput(
            "group photo must be a valid image no larger than 256 by 256".into(),
        ));
    }
    Ok(())
}
impl Node {
    #[cfg(test)]
    fn metadata_events(&self, channel: ConversationId) -> Result<Vec<Event>, LogError> {
        self.log.lock().expect("log lock").events(&channel)
    }
    fn pending_metadata() -> ChannelMetadata {
        ChannelMetadata {
            query_pending: true,
            ..ChannelMetadata::default()
        }
    }
    fn prepare_metadata_projection(
        &self,
        repository: &crate::eventlog::repository::EventRepository,
        channel: ConversationId,
        budget: &mut indexed::Budget,
    ) -> Result<bool, LogError> {
        match self.channel_checkpoints.source_binding(channel) {
            Ok(Some(_)) => Ok(true),
            Ok(None) | Err(LogError::StaleCursor) => {
                if budget.exhausted() {
                    return Ok(false);
                }
                let work = self.channel_checkpoints.project_step_with_budget(
                    repository,
                    &self.identity,
                    channel,
                    budget.preparation(),
                )?;
                budget.charge(work)?;
                if work.rows != 0 || work.edges != 0 || work.bytes != 0 {
                    self.channel_recovery_notify.notify_one();
                }
                Ok(work.ready)
            }
            Err(error) => Err(error),
        }
    }
    fn selected_metadata_state(
        &self,
        channel: ConversationId,
    ) -> Result<crate::channel::ChannelState, NodeError> {
        let state = self
            .channel_checkpoints
            .current_state(channel)?
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        let projection = self
            .channel_checkpoints
            .published_projection(channel)?
            .ok_or(LogError::StaleCursor)?;
        if !self.channel_identity_is_current_in(&state, &projection) {
            return Err(NodeError::Authorization(
                "accepted group membership required".into(),
            ));
        }
        Ok(state)
    }
    pub fn channel_metadata(&self, channel: ConversationId) -> Result<ChannelMetadata, NodeError> {
        use indexed::Progress;
        let log = self.log.lock().expect("log lock");
        if !log.requires_channel_v5(&channel)? {
            let book = self.channels.lock().expect("channels lock");
            let state = book
                .state(&channel)
                .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
            if !state.is_accepted_member(&self.user_id())
                || !self.channel_identity_matches_account(state)
            {
                return Err(NodeError::Authorization(
                    "accepted group membership required".into(),
                ));
            }
            return Ok(ChannelMetadata::default());
        }
        let mut budget = indexed::Budget::default();
        if !self.prepare_metadata_projection(log.repository()?, channel, &mut budget)? {
            return Ok(Self::pending_metadata());
        }
        match self.selected_metadata_state(channel) {
            Err(NodeError::Log(LogError::StaleCursor)) => return Ok(Self::pending_metadata()),
            result => {
                result?;
            }
        }
        let index = match indexed::Index::open(self, log.repository()?, channel, &mut budget)? {
            Progress::Pending => return Ok(Self::pending_metadata()),
            Progress::Ready(index) => index,
        };
        let account = self.account_id();
        let identity = self.identity.public();
        if let Some(cached) = self
            .group_metadata_cache
            .lock()
            .expect("metadata cache lock")
            .values
            .iter()
            .find(|c| {
                c.channel == channel
                    && c.source == index.stamp
                    && c.account == account
                    && c.identity == identity
            })
        {
            index.check_source()?;
            return Ok(cached.value.clone());
        }
        if !index.advance(&mut budget)? {
            return Ok(Self::pending_metadata());
        }
        let cover_id = index.publications(Field::Cover, 1)?.first().copied();
        let announcements = index.publications(Field::Announcement, MAX_HISTORY)?;
        let mut value = ChannelMetadata::default();
        if let Some(id) = cover_id {
            let publication = match index.load(id, &mut budget)? {
                Progress::Ready(value) => value,
                Progress::Pending => return Ok(Self::pending_metadata()),
            };
            value.cover = match index.value(&publication, &mut budget)? {
                Progress::Ready(value) => value,
                Progress::Pending => return Ok(Self::pending_metadata()),
            };
            value.cover_pending = value.cover.is_none();
        }
        for (position, id) in announcements.into_iter().enumerate() {
            let publication = match index.load(id, &mut budget)? {
                Progress::Ready(value) => value,
                Progress::Pending => return Ok(Self::pending_metadata()),
            };
            let selected = match index.value(&publication, &mut budget)? {
                Progress::Ready(value) => value,
                Progress::Pending => return Ok(Self::pending_metadata()),
            };
            if position == 0 {
                value.announcement_pending = selected.is_none();
                value.announcement = selected.clone();
            }
            if let Some(selected) = selected {
                value.announcement_history.push(selected);
            }
        }
        value.announcement_history.reverse();
        index.check_source()?;
        let mut cache = self
            .group_metadata_cache
            .lock()
            .expect("metadata cache lock");
        cache.values.retain(|c| c.channel != channel);
        if cache.values.len() == 64 {
            cache.values.pop_front();
        }
        cache.values.push_back(CachedMetadata {
            channel,
            source: index.stamp,
            account,
            identity,
            value: value.clone(),
        });
        Ok(value)
    }
    pub async fn set_channel_cover_if(
        &self,
        channel: ConversationId,
        content: Option<&str>,
        expected_revision: Option<EventId>,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        self.set_channel_metadata_if(channel, Field::Cover, content, expected_revision, authorize)
            .await
    }
    pub async fn set_channel_announcement_if(
        &self,
        channel: ConversationId,
        content: Option<&str>,
        expected_revision: Option<EventId>,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        self.set_channel_metadata_if(
            channel,
            Field::Announcement,
            content,
            expected_revision,
            authorize,
        )
        .await
    }
    async fn set_channel_metadata_if(
        &self,
        channel: ConversationId,
        field: Field,
        content: Option<&str>,
        expected_revision: Option<EventId>,
        authorize: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        validate_content(field, content)?;
        let mut recipients = None;
        authorize(&mut || {
            let _membership = self
                .channel_membership_commit
                .lock()
                .expect("channel membership commit lock");
            if !self.channel_can_manage(channel)? {
                return Err(NodeError::Authorization(
                    "only the active group owner can publish group metadata".into(),
                ));
            }
            let (state, source, unchanged) = {
                use indexed::Progress;
                let log = self.log.lock().expect("log lock");
                let mut budget = indexed::Budget::default();
                if !self.prepare_metadata_projection(log.repository()?, channel, &mut budget)? {
                    return Err(NodeError::Channel("group metadata is still loading".into()));
                }
                let state = self.selected_metadata_state(channel)?;
                if state.is_dissolved() {
                    return Err(NodeError::Channel("group is dissolved".into()));
                }
                let index =
                    match indexed::Index::open(self, log.repository()?, channel, &mut budget)? {
                        Progress::Ready(index) => index,
                        Progress::Pending => {
                            return Err(NodeError::Channel(
                                "group metadata is still loading".into(),
                            ));
                        }
                    };
                if !index.advance(&mut budget)? {
                    return Err(NodeError::Channel("group metadata is still loading".into()));
                }
                let latest = index.publications(field, 1)?.first().copied();
                if latest != expected_revision {
                    return Err(NodeError::RequestConflict);
                }
                let unchanged = if let Some(id) = latest {
                    let selected = match index.load(id, &mut budget)? {
                        Progress::Ready(value) => value,
                        Progress::Pending => {
                            return Err(NodeError::Channel(
                                "group metadata is still loading".into(),
                            ));
                        }
                    };
                    match index.value(&selected, &mut budget)? {
                        Progress::Ready(value) => {
                            value.is_some_and(|v| v.content.as_deref() == content)
                        }
                        Progress::Pending => {
                            return Err(NodeError::Channel(
                                "group metadata is still loading".into(),
                            ));
                        }
                    }
                } else {
                    false
                };
                index.check_source()?;
                (state, index.source, unchanged)
            };
            if unchanged {
                return Ok(());
            }
            let members: Vec<_> = std::iter::once(self.identity.public())
                .chain(
                    state
                        .members()
                        .iter()
                        .filter(|m| {
                            **m != self.identity.public() && state.is_accepted_member(&m.user_id())
                        })
                        .cloned(),
                )
                .take(MAX_RECIPIENTS)
                .collect();
            let key = GroupKey::generate();
            let mut plaintext = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .serialize(&content.map(str::to_owned))
                .map_err(|e| NodeError::InvalidInput(e.to_string()))?;
            let ciphertext = seal_channel_message(&key, &plaintext)
                .map_err(|e| NodeError::Channel(e.to_string()))?;
            zeroize::Zeroize::zeroize(&mut plaintext);
            let keys = SealedPayload::seal(&self.identity, &members, key.as_bytes())
                .map_err(|e| NodeError::Channel(e.to_string()))?;
            self.append_channel_control_from_source(
                &source,
                EventKind::ChannelMetadata,
                encode(&Control::Publish {
                    field,
                    keys,
                    ciphertext,
                })?,
            )?;
            let _ = self.channel_changes.send(channel);
            recipients = Some(state.members().to_vec());
            Ok(())
        })?;
        if let Some(members) = recipients {
            self.distribute_channel(channel, &members).await;
        }
        Ok(())
    }
    /// Bounded existing control worker; no additional network loop. Any accepted
    /// holder can share the current metadata keys after the owner goes offline.
    pub(super) fn retry_group_metadata(&self) -> Result<Vec<ConversationId>, NodeError> {
        use indexed::Progress;
        let _membership = self
            .channel_membership_commit
            .lock()
            .expect("channel membership commit lock");
        let after = self
            .group_metadata_cache
            .lock()
            .expect("metadata cache lock")
            .after_channel;
        let mut states = self.channel_checkpoints.current_page(after)?;
        if states.is_empty() && after.is_some() {
            states = self.channel_checkpoints.current_page(None)?;
        }
        if states.is_empty() {
            self.group_metadata_cache
                .lock()
                .expect("metadata cache lock")
                .after_channel = None;
            return Ok(Vec::new());
        }
        let mut changed = Vec::new();
        let mut send_budget = 8;
        let mut budget = indexed::Budget::default();
        for state in states {
            if budget.exhausted() {
                break;
            }
            let channel = state.id();
            self.group_metadata_cache
                .lock()
                .expect("metadata cache lock")
                .after_channel = Some(channel);
            if state.is_dissolved() || !self.channel_replica_enabled(channel, &self.user_id()) {
                continue;
            }
            let prepared = {
                let log = self.log.lock().expect("log lock");
                if !log.requires_channel_v5(&channel)? {
                    continue;
                }
                match self.channel_checkpoints.source_binding(channel) {
                    Ok(Some(_)) => {}
                    Ok(None) | Err(LogError::StaleCursor) => {
                        if !self.prepare_metadata_projection(
                            log.repository()?,
                            channel,
                            &mut budget,
                        )? {
                            continue;
                        }
                    }
                    Err(error) => return Err(error.into()),
                }
                let state = match self.selected_metadata_state(channel) {
                    Ok(state) => state,
                    Err(
                        NodeError::Authorization(_)
                        | NodeError::Channel(_)
                        | NodeError::Log(LogError::StaleCursor),
                    ) => continue,
                    Err(error) => return Err(error),
                };
                if state.is_dissolved() {
                    continue;
                }
                let index =
                    match indexed::Index::open(self, log.repository()?, channel, &mut budget)? {
                        Progress::Pending => continue,
                        Progress::Ready(index) => index,
                    };
                if self
                    .group_metadata_cache
                    .lock()
                    .expect("metadata cache lock")
                    .reconciled
                    .contains(&(channel, index.stamp))
                {
                    continue;
                }
                if !index.advance(&mut budget)? {
                    continue;
                }
                let mut prepared = None;
                let mut pending = false;
                for field in [Field::Cover, Field::Announcement] {
                    let Some(id) = index.publications(field, 1)?.first().copied() else {
                        continue;
                    };
                    let publication = match index.load(id, &mut budget)? {
                        Progress::Ready(value) => value,
                        Progress::Pending => {
                            pending = true;
                            break;
                        }
                    };
                    let key = match index.key(&publication, &mut budget)? {
                        Progress::Ready(Some(key)) => key,
                        Progress::Ready(None) => continue,
                        Progress::Pending => {
                            pending = true;
                            break;
                        }
                    };
                    for recipient in state
                        .members()
                        .iter()
                        .filter(|m| state.is_accepted_member(&m.user_id()))
                    {
                        if publication
                            .keys
                            .entries
                            .iter()
                            .any(|e| e.user_id == recipient.user_id())
                            || index.already_released(id, recipient)?
                        {
                            continue;
                        }
                        let sealed_key =
                            crate::dm::seal(&self.identity, &recipient.x25519_pub, key.as_bytes())
                                .map_err(NodeError::Seal)?;
                        prepared = Some((
                            index.source,
                            encode(&Control::Release {
                                publication: id,
                                recipient: recipient.clone(),
                                sealed_key,
                            })?,
                        ));
                        break;
                    }
                    if prepared.is_some() {
                        break;
                    }
                }
                index.check_source()?;
                if !pending && prepared.is_none() {
                    let mut cache = self
                        .group_metadata_cache
                        .lock()
                        .expect("metadata cache lock");
                    cache.reconciled.retain(|(id, _)| *id != channel);
                    if cache.reconciled.len() == 64 {
                        cache.reconciled.pop_front();
                    }
                    cache.reconciled.push_back((channel, index.stamp));
                    let _ = self.channel_changes.send(channel);
                }
                prepared
            };
            if let Some((source, payload)) = prepared {
                self.append_channel_control_from_source(
                    &source,
                    EventKind::ChannelMetadata,
                    payload,
                )?;
                changed.push(channel);
                let _ = self.channel_changes.send(channel);
                send_budget -= 1;
                if send_budget == 0 {
                    break;
                }
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
#[path = "group_metadata_tests.rs"]
mod tests;
