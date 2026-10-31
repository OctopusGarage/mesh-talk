//! Atomic policy qualification around the repository's bounded network selectors.
use super::*;
use crate::eventlog::{
    repository::network::{ExportCursor, HaveCursor, NetworkView},
    sync::network::{ExportPage, ExportPageBudget, IdPageBudget, SyncMetadata, SyncPage},
};
use crate::postoffice::storage::federation::SessionLease;
use std::collections::HashSet;

/// Finite membership with a current role identity. Unknown is an explicit binding.
pub struct IndexedRelayView {
    repository: NetworkView,
    session: Option<SessionLease>,
    conversation: ConversationId,
    cutoff: Option<u64>,
    first_seen: Option<u64>,
}
#[derive(Clone)]
pub struct IndexedRelayHaveCursor {
    cursor: HaveCursor,
    _owner: Arc<ProfileStorage>,
}
#[derive(Clone)]
pub struct IndexedRelayExportCursor {
    cursor: ExportCursor,
    _owner: Arc<ProfileStorage>,
}
impl std::fmt::Debug for IndexedRelayHaveCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cursor.fmt(f)
    }
}
impl std::fmt::Debug for IndexedRelayExportCursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.cursor.fmt(f)
    }
}
impl IndexedRelayState {
    pub(crate) fn network_view_at(
        &self,
        conv: &ConversationId,
        now: u64,
    ) -> Result<IndexedRelayView, LogError> {
        self.network_view_with_session_at(conv, now, None)
    }
    pub(crate) fn network_view_with_session_at(
        &self,
        conv: &ConversationId,
        now: u64,
        session: Option<&SessionLease>,
    ) -> Result<IndexedRelayView, LogError> {
        self.session_transaction(now, session, |c, cutoff| {
            let mailbox = self.eligible_on(c, conv, cutoff)?;
            Ok(IndexedRelayView {
                repository: self.repository.network_open_view_on(c, conv)?,
                session: session.cloned(),
                conversation: *conv,
                cutoff,
                first_seen: mailbox.map(|m| m.first_seen),
            })
        })
    }
    pub(crate) fn session_transaction<T>(
        &self,
        now: u64,
        session: Option<&SessionLease>,
        operation: impl FnOnce(&Connection, Option<u64>) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        self.session_derived_transaction(now, session, true, operation)
    }

    pub(crate) fn network_session_matches(
        view: &IndexedRelayView,
        session: &SessionLease,
    ) -> Result<(), LogError> {
        if view
            .session
            .as_ref()
            .is_some_and(|s| s.same_session(session))
        {
            Ok(())
        } else {
            Err(LogError::StaleCursor)
        }
    }
    pub(crate) fn session_custody_at(
        &self,
        session: &SessionLease,
        conv: &ConversationId,
        id: &EventId,
        now: u64,
    ) -> Result<bool, LogError> {
        self.session_transaction(now, Some(session), |c, cutoff| {
            if self.eligible_on(c, conv, cutoff)?.is_none() {
                return Ok(false);
            }
            Ok(self
                .repository
                .event_conversation_on(c, id)?
                .is_some_and(|scope| scope == *conv))
        })
    }
    pub(crate) fn session_consent_at(
        &self,
        session: &SessionLease,
        conv: &ConversationId,
        now: u64,
    ) -> Result<(), LogError> {
        self.session_transaction(now, Some(session), |c, cutoff| {
            self.eligible_on(c, conv, cutoff).map(|_| ())
        })
    }
    fn network_validate_on(
        &self,
        c: &Connection,
        view: &IndexedRelayView,
        cutoff: Option<u64>,
    ) -> Result<(), LogError> {
        if cutoff != view.cutoff {
            return Err(LogError::StaleCursor);
        }
        let arrival = self.arrival_on(c, &view.conversation)?;
        if arrival.as_ref().is_some_and(|a| a.pending)
            || arrival.as_ref().map(|a| a.first_seen) != view.first_seen
        {
            return Err(LogError::StaleCursor);
        }
        // Validate owner/epoch before interpreting a newer scope's lifecycle.
        self.repository.validate_network_on(c, &view.repository)?;
        self.eligible_on(c, &view.conversation, cutoff)?;
        Ok(())
    }
    pub(crate) fn network_validate_at(
        &self,
        view: &IndexedRelayView,
        now: u64,
    ) -> Result<(), LogError> {
        self.session_transaction(now, view.session.as_ref(), |c, cutoff| {
            self.network_validate_on(c, view, cutoff)
        })
    }
    pub(crate) fn network_metadata_at(
        &self,
        view: &IndexedRelayView,
        now: u64,
    ) -> Result<SyncMetadata, LogError> {
        self.session_transaction(now, view.session.as_ref(), |c, cutoff| {
            self.network_validate_on(c, view, cutoff)?;
            let metadata = self.repository.network_metadata_on(c, &view.repository)?;
            if view.first_seen.is_some() && metadata.fingerprint.is_none() {
                self.demand_on(c, &view.conversation, cutoff)?;
            }
            Ok(metadata)
        })
    }
    pub(crate) fn network_have_at(
        &self,
        view: &IndexedRelayView,
        after: Option<&IndexedRelayHaveCursor>,
        budget: IdPageBudget,
        now: u64,
    ) -> Result<SyncPage<EventId, IndexedRelayHaveCursor>, LogError> {
        self.session_transaction(now, view.session.as_ref(), |c, cutoff| {
            self.network_validate_on(c, view, cutoff)?;
            let page = self.repository.network_have_page_on(
                c,
                &view.repository,
                after.map(|a| &a.cursor),
                budget,
            )?;
            Ok(SyncPage {
                items: page.items,
                examined: page.examined,
                next: page.next.map(|cursor| IndexedRelayHaveCursor {
                    cursor,
                    _owner: self.profile.clone(),
                }),
            })
        })
    }
    pub(crate) fn network_export_at(
        &self,
        view: &IndexedRelayView,
        have: &HashSet<EventId>,
        after: Option<&IndexedRelayExportCursor>,
        budget: ExportPageBudget,
        now: u64,
    ) -> Result<ExportPage<IndexedRelayExportCursor>, LogError> {
        self.session_transaction(now, view.session.as_ref(), |c, cutoff| {
            self.network_validate_on(c, view, cutoff)?;
            let page = self.repository.network_export_page_on(
                c,
                &view.repository,
                have,
                after.map(|a| &a.cursor),
                budget,
            )?;
            Ok(ExportPage {
                events: page.events,
                examined: page.examined,
                encoded_bytes: page.encoded_bytes,
                allocated_bytes: page.allocated_bytes,
                next: page.next.map(|cursor| IndexedRelayExportCursor {
                    cursor,
                    _owner: self.profile.clone(),
                }),
            })
        })
    }
}
