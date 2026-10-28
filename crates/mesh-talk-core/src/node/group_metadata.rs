//! Owner-published group metadata. Each publication has an independent content key;
//! current accepted holders can release only the current field to new members.
//! These are durable group controls, independent of ordinary message retention.
use super::{
    Node, NodeError,
    channel::{ChannelBook, SealedPayload},
    join_history::View,
};
use crate::{
    channel::{GroupKey, open_channel_message, seal_channel_message},
    eventlog::{ConversationId, Event, EventId, EventKind},
    identity::device::PublicIdentity,
};
use base64::Engine;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

const MAX_CONTROL: usize = 60 * 1024;
const MAX_COVER: usize = 32 * 1024;
const MAX_ANNOUNCEMENT: usize = 4096;
const MAX_RECIPIENTS: usize = 128;
const MAX_HISTORY: usize = 20;

#[derive(Default)]
pub(super) struct MetadataCache {
    values: std::collections::VecDeque<CachedMetadata>,
    reconciled: std::collections::VecDeque<(ConversationId, ([u8; 32], usize))>,
}
struct CachedMetadata {
    channel: ConversationId,
    source: ([u8; 32], usize),
    account: String,
    identity: PublicIdentity,
    value: ChannelMetadata,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct ChannelMetadata {
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
struct Publication<'a> {
    event: &'a Event,
    field: Field,
    keys: SealedPayload,
    ciphertext: Vec<u8>,
    author: PublicIdentity,
}
struct MetadataView<'a> {
    view: View<'a>,
    events: Vec<&'a Event>,
    publications: Vec<Publication<'a>>,
    by_id: HashMap<EventId, &'a Event>,
}
impl<'a> MetadataView<'a> {
    fn new(events: &[&'a Event]) -> Self {
        let by_id: HashMap<_, _> = events.iter().map(|e| (e.id, *e)).collect();
        let ordered: Vec<_> = super::timeline::ordered_ids(events, &[], true)
            .iter()
            .filter_map(|id| by_id.get(id).copied())
            .collect();
        let view = View::new(&ordered);
        let publications = ordered
            .iter()
            .filter_map(|event| {
                let Control::Publish {
                    field,
                    keys,
                    ciphertext,
                } = decode(event)?
                else {
                    return None;
                };
                if !view.owner_authorized(event)
                    || keys.sender != event.author.user_id()
                    || keys.entries.is_empty()
                    || keys.entries.len() > MAX_RECIPIENTS
                    || ciphertext.len() > MAX_COVER + 128
                {
                    return None;
                }
                let before = view.before(event)?;
                let canonical = view.canonical_before(event)?;
                let mut recipients = HashSet::new();
                if keys.entries.iter().any(|entry| {
                    !before.is_accepted_member(&entry.user_id)
                        || !canonical.is_accepted_member(&entry.user_id)
                        || entry.sealed.is_empty()
                        || entry.sealed.len() > 256
                        || !recipients.insert(&entry.user_id)
                }) {
                    return None;
                }
                let author = before
                    .members()
                    .iter()
                    .find(|member| member.user_id() == event.author.user_id())?
                    .clone();
                Some(Publication {
                    event,
                    field,
                    keys,
                    ciphertext,
                    author,
                })
            })
            .collect();
        Self {
            view,
            events: ordered,
            publications,
            by_id,
        }
    }
    fn latest(&self, field: Field) -> Option<&Publication<'a>> {
        self.publications.iter().rev().find(|p| p.field == field)
    }
    fn release_valid(
        &self,
        event: &Event,
        publication: &Publication<'_>,
        recipient: &PublicIdentity,
    ) -> bool {
        if !super::channel::descends_from(event, publication.event.id, &self.by_id) {
            return false;
        }
        // A release grants current metadata only. Later publication events never
        // revive eligibility for old announcement history.
        if self
            .publications
            .iter()
            .rev()
            .find(|p| {
                p.field == publication.field
                    && (p.event.lamport, p.event.id) < (event.lamport, event.id)
            })
            .is_none_or(|p| p.event.id != publication.event.id)
        {
            return false;
        }
        let author = event.author.user_id();
        [self.view.before(event), self.view.canonical_before(event)]
            .into_iter()
            .all(|state| {
                state.is_some_and(|state| {
                    !state.is_dissolved()
                        && state.is_accepted_member(&author)
                        && state.is_accepted_member(&recipient.user_id())
                        && state.members().contains(recipient)
                })
            })
    }
    fn key(
        &self,
        publication: &Publication<'_>,
        identity: &crate::identity::device::DeviceIdentity,
    ) -> Option<GroupKey> {
        let own = identity.user_id();
        if let Some(mut bytes) = publication
            .keys
            .open(identity, &publication.author.x25519_pub)
        {
            let key = bytes.as_slice().try_into().ok().map(GroupKey::from_bytes);
            zeroize::Zeroize::zeroize(&mut bytes);
            if let Some(key) = key
                && open_channel_message(&key, &publication.ciphertext).is_ok()
            {
                return Some(key);
            }
        }
        for event in &self.events {
            let Some(Control::Release {
                publication: id,
                recipient,
                sealed_key,
            }) = decode(event)
            else {
                continue;
            };
            if id != publication.event.id
                || recipient.user_id() != own
                || recipient != identity.public()
                || !self.release_valid(event, publication, &recipient)
            {
                continue;
            }
            let Some(before) = self.view.before(event) else {
                continue;
            };
            let Some(author) = before
                .members()
                .iter()
                .find(|m| m.user_id() == event.author.user_id())
            else {
                continue;
            };
            if let Ok(mut bytes) = crate::dm::open(identity, &author.x25519_pub, &sealed_key) {
                let key = bytes.as_slice().try_into().ok().map(GroupKey::from_bytes);
                zeroize::Zeroize::zeroize(&mut bytes);
                if let Some(key) = key
                    && open_channel_message(&key, &publication.ciphertext).is_ok()
                {
                    return Some(key);
                }
            }
        }
        None
    }
    fn value(
        &self,
        publication: &Publication<'_>,
        identity: &crate::identity::device::DeviceIdentity,
    ) -> Option<ChannelMetadataValue> {
        let key = self.key(publication, identity)?;
        let bytes =
            zeroize::Zeroizing::new(open_channel_message(&key, &publication.ciphertext).ok()?);
        let content: Option<String> = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit((MAX_COVER + 32) as u64)
            .reject_trailing_bytes()
            .deserialize(&bytes)
            .ok()?;
        validate_content(publication.field, content.as_deref()).ok()?;
        let state = self.view.before(publication.event)?;
        Some(ChannelMetadataValue {
            revision: hex::encode(publication.event.id.as_bytes()),
            content,
            author: state
                .member_account_id(&publication.author.user_id())
                .unwrap_or(&publication.author.user_id())
                .to_owned(),
            authored_at: publication.event.wall_clock,
        })
    }
}
impl Node {
    fn metadata_events(&self, channel: ConversationId) -> Vec<Event> {
        self.log
            .lock()
            .expect("log lock")
            .events(&channel)
            .into_iter()
            .cloned()
            .collect()
    }
    pub fn channel_metadata(&self, channel: ConversationId) -> Result<ChannelMetadata, NodeError> {
        let (present, source) = {
            let log = self.log.lock().expect("log lock");
            (
                log.requires_channel_v5(&channel),
                if log.requires_channel_v5(&channel) {
                    log.fingerprint_and_len(&channel)
                } else {
                    ([0; 32], 0)
                },
            )
        };
        if !present {
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
        let account = self.account_id();
        let identity = self.identity.public();
        if let Some(cached) = self
            .group_metadata_cache
            .lock()
            .expect("metadata cache lock")
            .values
            .iter()
            .find(|cached| {
                cached.channel == channel
                    && cached.source == source
                    && cached.account == account
                    && cached.identity == identity
            })
        {
            return Ok(cached.value.clone());
        }
        let events = self.metadata_events(channel);
        let refs: Vec<_> = events.iter().collect();
        let state = ChannelBook::projected_metadata(channel, &refs)
            .ok_or_else(|| NodeError::Channel("unknown channel".into()))?;
        if !self.channel_identity_is_current(&state) {
            return Err(NodeError::Authorization(
                "accepted group membership required".into(),
            ));
        }
        let view = MetadataView::new(&refs);
        let cover = view
            .latest(Field::Cover)
            .and_then(|p| view.value(p, &self.identity));
        let announcement = view
            .latest(Field::Announcement)
            .and_then(|p| view.value(p, &self.identity));
        let value = ChannelMetadata {
            cover_pending: view.latest(Field::Cover).is_some() && cover.is_none(),
            announcement_pending: view.latest(Field::Announcement).is_some()
                && announcement.is_none(),
            cover,
            announcement,
            announcement_history: view
                .publications
                .iter()
                .rev()
                .filter(|p| p.field == Field::Announcement)
                .take(MAX_HISTORY)
                .filter_map(|p| view.value(p, &self.identity))
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect(),
        };
        let mut cache = self
            .group_metadata_cache
            .lock()
            .expect("metadata cache lock");
        cache.values.retain(|cached| cached.channel != channel);
        if cache.values.len() == 64 {
            cache.values.pop_front();
        }
        cache.values.push_back(CachedMetadata {
            channel,
            source,
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
            if !self.channel_can_manage(channel) {
                return Err(NodeError::Authorization(
                    "only the active group owner can publish group metadata".into(),
                ));
            }
            let events = self.metadata_events(channel);
            let refs: Vec<_> = events.iter().collect();
            let state = ChannelBook::projected_metadata(channel, &refs)
                .ok_or_else(|| NodeError::Channel("unknown group".into()))?;
            if state.is_dissolved() {
                return Err(NodeError::Channel("group is dissolved".into()));
            }
            let view = MetadataView::new(&refs);
            let latest = view.latest(field);
            if latest.map(|p| p.event.id) != expected_revision {
                return Err(NodeError::RequestConflict);
            }
            if latest
                .and_then(|p| view.value(p, &self.identity))
                .is_some_and(|v| v.content.as_deref() == content)
            {
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
            self.append_event(
                channel,
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
        let _membership = self
            .channel_membership_commit
            .lock()
            .expect("channel membership commit lock");
        let mut channels = self.channels.lock().expect("channels lock").channel_ids();
        channels.sort_by_key(|c| *c.as_bytes());
        if !channels.is_empty() {
            let offset = (super::node::now_millis() / 3000) as usize % channels.len();
            channels.rotate_left(offset);
        }
        let mut changed = Vec::new();
        let mut budget = 8;
        for channel in channels.into_iter().take(32) {
            let source = {
                let log = self.log.lock().expect("log lock");
                if !log.requires_channel_v5(&channel) {
                    continue;
                }
                log.fingerprint_and_len(&channel)
            };
            if self
                .group_metadata_cache
                .lock()
                .expect("metadata cache lock")
                .reconciled
                .contains(&(channel, source))
            {
                continue;
            }
            let events = self.metadata_events(channel);
            let refs: Vec<_> = events.iter().collect();
            let Some(state) = ChannelBook::projected_metadata(channel, &refs) else {
                continue;
            };
            if state.is_dissolved()
                || !self.channel_identity_is_current(&state)
                || !self.channel_replica_enabled(channel, &self.user_id())
            {
                continue;
            }
            let view = MetadataView::new(&refs);
            for field in [Field::Cover, Field::Announcement] {
                let Some(publication) = view.latest(field) else {
                    continue;
                };
                let Some(key) = view.key(publication, &self.identity) else {
                    continue;
                };
                for recipient in state
                    .members()
                    .iter()
                    .filter(|m| state.is_accepted_member(&m.user_id()))
                {
                    let included = publication
                        .keys
                        .entries
                        .iter()
                        .any(|entry| entry.user_id == recipient.user_id());
                    // Another member's opaque release cannot prove it carried the correct key.
                    // Each holder deduplicates its own releases, allowing honest fallback sources.
                    let already_released = view.events.iter().any(|event| {
                        event.author.user_id() == self.user_id() && matches!(decode(event), Some(Control::Release {publication: id, recipient: ref target, ..}) if id == publication.event.id && target == recipient && view.release_valid(event, publication, target))
                    });
                    if included || already_released {
                        continue;
                    }
                    if budget == 0 {
                        return Ok(changed);
                    }
                    let sealed_key =
                        crate::dm::seal(&self.identity, &recipient.x25519_pub, key.as_bytes())
                            .map_err(NodeError::Seal)?;
                    self.append_event(
                        channel,
                        EventKind::ChannelMetadata,
                        encode(&Control::Release {
                            publication: publication.event.id,
                            recipient: recipient.clone(),
                            sealed_key,
                        })?,
                    )?;
                    budget -= 1;
                    if !changed.contains(&channel) {
                        changed.push(channel);
                    }
                    let _ = self.channel_changes.send(channel);
                }
            }
            // Record only the source actually examined. Own appended releases
            // or concurrent imports require another pass, never an unseen-source skip.
            let completed = source;
            let mut cache = self
                .group_metadata_cache
                .lock()
                .expect("metadata cache lock");
            cache.reconciled.retain(|(id, _)| *id != channel);
            if cache.reconciled.len() == 64 {
                cache.reconciled.pop_front();
            }
            cache.reconciled.push_back((channel, completed));
        }
        Ok(changed)
    }
}

#[cfg(test)]
#[path = "group_metadata_tests.rs"]
mod tests;
