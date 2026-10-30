//! Unfiltered repository network selectors. These are not an expiry/participant
//! policy facade; runtime owners must supply their own current authorization.
use super::*;
use crate::eventlog::sync::FileConsentStatus;
use crate::eventlog::sync::network::{
    ExportPage, ExportPageBudget, IdPageBudget, NetworkSyncStore, SyncMetadata, SyncPage,
    scan_limit,
};
use std::collections::HashSet;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Binding {
    store_uuid: [u8; 16],
    domain: EventDomainId,
    conversation: ConversationId,
    pub(super) cutoff: i64,
    epoch: i64,
}
/// Retains the profile lease even while the owning store is unlocked/dropped.
pub(crate) struct NetworkView {
    pub(super) binding: Binding,
    metadata: SyncMetadata,
    _lease: RepositoryOwner,
}
#[derive(Clone, Debug)]
pub(crate) struct HaveCursor {
    binding: Binding,
    after: i64,
    id: [u8; 32],
}
#[derive(Clone, Debug)]
pub(crate) struct ExportCursor {
    binding: Binding,
    after: i64,
    id: [u8; 32],
}
// Index shape is part of this owner: no ID-order postfilter, sorting or temp keyset.
const SELECT_COMMIT_IDS: &str = "SELECT commit_ordinal,id FROM retained_events INDEXED BY retained_events_commit WHERE domain=?1 AND conversation=?2 AND commit_ordinal>=?3 AND commit_ordinal<?4 AND (commit_ordinal,id)>(?5,?6) ORDER BY commit_ordinal,id LIMIT ?7";

impl EventRepository {
    pub(super) fn network_epoch_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<i64, LogError> {
        let epoch: Option<i64> = c.prepare_cached("SELECT destructive_epoch FROM event_conversations WHERE domain=?1 AND conversation=?2")?.query_row(params![self.domain.0.as_slice(),conv.as_bytes().as_slice()],|r|r.get(0)).optional()?;
        nonnegative(epoch.unwrap_or(0))
    }
    pub(crate) fn validate_network_on(
        &self,
        c: &Connection,
        view: &NetworkView,
    ) -> Result<(), LogError> {
        if view.binding.store_uuid != self.store_uuid
            || view.binding.domain != self.domain
            || self.network_epoch_on(c, &view.binding.conversation)? != view.binding.epoch
        {
            return Err(LogError::StaleCursor);
        }
        if self
            .scope_on(c, &view.binding.conversation)?
            .is_some_and(|s| s.lifecycle == 1)
        {
            return Err(LogError::StaleCursor);
        }
        // A source cutoff freezes membership, not the minimum supported wire
        // protocol. A newly required control invalidates an older capability
        // binding before even its cached fingerprint or Have can be handed off.
        // Read fixed metadata only; ordinary appends retain this finite view.
        if let Some(current) = self.metadata_on(c, &view.binding.conversation)? {
            let v5 = view.metadata.channel_v5;
            let v4 = v5 || view.metadata.channel_v4;
            let v3 = v4 || view.metadata.channel_v3;
            let v2 = v3 || view.metadata.channel_v2;
            if (self.has_kind_on(c, &view.binding.conversation, EventKind::ChannelMetadata)? && !v5)
                || (current.channel_v4 && !v4)
                || (current.channel_v3 && !v3)
                || (current.channel_v2 && !v2)
            {
                return Err(LogError::StaleCursor);
            }
        }
        Ok(())
    }
    fn network_header_on(&self, c: &Connection, id: &EventId) -> Result<EventHeader, LogError> {
        #[cfg(test)]
        self.network_header_fetches
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut stmt = c.prepare_cached(&format!(
            "SELECT {HEADER_COLUMNS},length(body) FROM retained_events WHERE domain=?1 AND id=?2"
        ))?;
        let mut rows = stmt.query(params![self.domain.0.as_slice(), id.as_bytes().as_slice()])?;
        let row = rows
            .next()?
            .ok_or_else(|| corrupt("network selected header disappeared"))?;
        let header = read_header(row)?;
        if size(row, 12)? != header.stored_bytes {
            return Err(corrupt("stored event length disagrees with body"));
        }
        Ok(header)
    }
    /// Current cache only, never a synchronous O(N) fingerprint computation.
    fn cached_fingerprint_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        revision: i64,
    ) -> Result<Option<[u8; 32]>, LogError> {
        let cache:Option<(Option<i64>,Option<Vec<u8>>)>=c.prepare_cached("SELECT fingerprint_revision,fingerprint FROM event_conversations WHERE domain=?1 AND conversation=?2")?.query_row(params![self.domain.0.as_slice(),conv.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        match cache {
            None | Some((None, None)) => Ok(None),
            Some((Some(cached), Some(bytes))) => {
                nonnegative(cached)?;
                let digest = bytes
                    .try_into()
                    .map_err(|_| corrupt("invalid persisted fingerprint"))?;
                if cached > revision {
                    return Err(corrupt("fingerprint revision exceeds conversation"));
                }
                Ok((cached == revision).then_some(digest))
            }
            _ => Err(corrupt("incomplete persisted fingerprint")),
        }
    }
}
impl EventRepository {
    pub(crate) fn network_open_view_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<NetworkView, LogError> {
        let cutoff = nonnegative(
            c.prepare_cached("SELECT next_commit FROM event_domains WHERE domain=?1")?
                .query_row([self.domain.0.as_slice()], |r| r.get(0))?,
        )?;
        let metadata = self.metadata_on(c, conv)?;
        let mut captured = SyncMetadata::default();
        if let Some(m) = metadata {
            captured = SyncMetadata {
                count: m.count,
                channel_v2: m.channel_v2,
                channel_v3: m.channel_v3,
                channel_v4: m.channel_v4,
                channel_v5: self.has_kind_on(c, conv, EventKind::ChannelMetadata)?,
                fingerprint: self.cached_fingerprint_on(c, conv, m.revision)?,
            };
        }
        Ok(NetworkView {
            binding: Binding {
                store_uuid: self.store_uuid,
                domain: self.domain,
                conversation: *conv,
                cutoff,
                epoch: self.network_epoch_on(c, conv)?,
            },
            metadata: captured,
            _lease: self.database.clone(),
        })
    }
    pub(crate) fn network_have_page_on(
        &self,
        c: &Connection,
        view: &NetworkView,
        after: Option<&HaveCursor>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, HaveCursor>, LogError> {
        budget.validate()?;

        self.validate_network_on(c, view)?;
        if after.is_some_and(|a| a.binding != view.binding) {
            return Err(LogError::StaleCursor);
        }
        let limit = budget.ids.min(budget.examined);
        let (last, id) = after.map_or((-1, [0; 32]), |a| (a.after, a.id));
        let mut stmt = c.prepare_cached(SELECT_COMMIT_IDS)?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            view.binding.conversation.as_bytes().as_slice(),
            last.max(0),
            view.binding.cutoff,
            last,
            id.as_slice(),
            scan_limit(limit)?
        ])?;
        let mut items = Vec::new();
        let mut cursor = None;
        while let Some(row) = rows.next()? {
            if items.len() == limit {
                return Ok(SyncPage {
                    examined: items.len(),
                    items,
                    next: cursor,
                });
            }
            let ordinal = nonnegative(row.get(0)?)?;
            let id = blob(row, 1)?;
            items.reserve_exact(1);
            items.push(EventId::new(id));
            cursor = Some(HaveCursor {
                binding: view.binding,
                after: ordinal,
                id,
            });
        }
        Ok(SyncPage {
            examined: items.len(),
            items,
            next: None,
        })
    }
    pub(crate) fn network_export_page_on(
        &self,
        c: &Connection,
        view: &NetworkView,
        have: &HashSet<EventId>,
        after: Option<&ExportCursor>,
        budget: ExportPageBudget,
    ) -> Result<ExportPage<ExportCursor>, LogError> {
        self.network_export_selected_on(c, view, have, after, budget, None)
    }
    pub(super) fn network_export_selected_on(
        &self,
        c: &Connection,
        view: &NetworkView,
        have: &HashSet<EventId>,
        after: Option<&ExportCursor>,
        budget: ExportPageBudget,
        selection: Option<&[u8; 16]>,
    ) -> Result<ExportPage<ExportCursor>, LogError> {
        budget.validate()?;

        self.validate_network_on(c, view)?;
        if after.is_some_and(|a| a.binding != view.binding) {
            return Err(LogError::StaleCursor);
        }
        let (last, id) = after.map_or((-1, [0; 32]), |a| (a.after, a.id));
        let mut cursor = ExportCursor {
            binding: view.binding,
            after: last,
            id,
        };
        let mut page = ExportPage {
            events: Vec::new(),
            next: None,
            examined: 0,
            encoded_bytes: 0,
            allocated_bytes: 0,
        };
        let mut stmt = c.prepare_cached(SELECT_COMMIT_IDS)?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            view.binding.conversation.as_bytes().as_slice(),
            last.max(0),
            view.binding.cutoff,
            last,
            id.as_slice(),
            scan_limit(budget.examined)?
        ])?;
        while let Some(row) = rows.next()? {
            if page.examined == budget.examined || page.events.len() == budget.events {
                page.next = Some(cursor);
                return Ok(page);
            }
            let ordinal = nonnegative(row.get(0)?)?;
            let id = EventId::new(blob(row, 1)?);
            let allowed = match selection {
                Some(selection) => c.query_row("SELECT EXISTS(SELECT 1 FROM participant_sync_rows WHERE view=?1 AND id=?2 AND outgoing=1)", params![selection.as_slice(), id.as_bytes().as_slice()], |r| r.get::<_, bool>(0))?,
                None => true,
            };
            if allowed && !have.contains(&id) {
                let header = self.network_header_on(c, &id)?;
                if header.conversation != view.binding.conversation {
                    return Err(corrupt("network header conversation differs"));
                }
                let allocation = header
                    .stored_bytes
                    .checked_mul(3)
                    .and_then(|n| n.checked_add(std::mem::size_of::<Event>()))
                    .ok_or(LogError::RepositoryCounterExhausted)?;
                let encoded = page
                    .encoded_bytes
                    .checked_add(header.encoded_bytes)
                    .ok_or(LogError::RepositoryCounterExhausted)?;
                let allocated = page
                    .allocated_bytes
                    .checked_add(allocation)
                    .ok_or(LogError::RepositoryCounterExhausted)?;
                if encoded > budget.encoded_bytes || allocated > budget.allocated_bytes {
                    if page.events.is_empty() {
                        return Err(LogError::PageBudgetTooSmall {
                            required: if encoded > budget.encoded_bytes {
                                header.encoded_bytes
                            } else {
                                allocation
                            },
                        });
                    }
                    page.next = Some(cursor);
                    return Ok(page);
                }
                let event = self
                    .get_on(c, &id)?
                    .ok_or_else(|| corrupt("network selected body disappeared"))?;
                page.events.reserve_exact(1);
                page.events.push(event);
                page.encoded_bytes = encoded;
                page.allocated_bytes = allocated;
            }
            page.examined += 1;
            cursor.after = ordinal;
            cursor.id = *id.as_bytes();
        }
        Ok(page)
    }
    pub(crate) fn network_metadata_on(
        &self,
        c: &Connection,
        view: &NetworkView,
    ) -> Result<SyncMetadata, LogError> {
        self.validate_network_on(c, view)?;
        Ok(view.metadata)
    }
}
impl NetworkSyncStore for EventRepository {
    type View = NetworkView;
    type HaveCursor = HaveCursor;
    type ExportCursor = ExportCursor;
    fn open_view(&self, conv: &ConversationId) -> Result<NetworkView, LogError> {
        self.database.read(|c| self.network_open_view_on(c, conv))
    }
    fn metadata(&self, view: &NetworkView) -> Result<SyncMetadata, LogError> {
        self.database.read(|c| self.network_metadata_on(c, view))
    }
    fn validate_view(&self, view: &NetworkView) -> Result<(), LogError> {
        self.database.read(|c| self.validate_network_on(c, view))
    }
    fn have_page(
        &self,
        view: &NetworkView,
        after: Option<&HaveCursor>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, HaveCursor>, LogError> {
        budget.validate()?;
        self.database
            .read(|c| self.network_have_page_on(c, view, after, budget))
    }
    fn export_page(
        &self,
        view: &NetworkView,
        have: &HashSet<EventId>,
        after: Option<&ExportCursor>,
        budget: ExportPageBudget,
    ) -> Result<ExportPage<ExportCursor>, LogError> {
        budget.validate()?;
        self.database
            .read(|c| self.network_export_page_on(c, view, have, after, budget))
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append(&event)
    }
    // Raw repository visibility is deliberately insufficient for runtime custody.
    fn durable_have(&self, _: &ConversationId, _: &EventId) -> Result<bool, LogError> {
        Ok(false)
    }
    fn file_consent(&self, _: &ConversationId, _: &EventId) -> Result<FileConsentStatus, LogError> {
        Ok(FileConsentStatus::Pending)
    }
    fn signed_file_consent(
        &self,
        _: &ConversationId,
        _: &EventId,
    ) -> Result<Option<Event>, LogError> {
        Ok(None)
    }
    fn admission_denied(&self) -> bool {
        false
    }
}
#[cfg(test)]
mod tests;
