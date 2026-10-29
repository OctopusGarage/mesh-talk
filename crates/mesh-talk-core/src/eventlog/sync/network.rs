//! Mandatory bounded network-store seam. The collecting [`super::SyncStore`]
//! SDK is deliberately independent: there is no blanket collecting fallback.
use super::{ApplyReport, FileConsentStatus};
use crate::eventlog::{AppendOutcome, ConversationId, Event, EventId, LogError};
use std::collections::HashSet;

/// Metadata captured atomically with a finite, owned view. A missing current
/// cached fingerprint skips the optional probe; it never denotes an empty set.
#[derive(Clone, Copy, Debug, Default)]
pub struct SyncMetadata {
    pub count: u64,
    pub channel_v2: bool,
    pub channel_v3: bool,
    pub channel_v4: bool,
    pub fingerprint: Option<[u8; 32]>,
}

#[derive(Clone, Copy, Debug)]
pub struct IdPageBudget {
    pub ids: usize,
    pub examined: usize,
}
impl IdPageBudget {
    pub fn validate(self) -> Result<(), LogError> {
        if self.ids == 0 || self.ids > 1500 || self.examined == 0 || self.ids > self.examined {
            return Err(LogError::InvalidPageBudget);
        }
        scan_limit(self.examined).map(|_| ())
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ExportPageBudget {
    pub examined: usize,
    pub events: usize,
    pub encoded_bytes: usize,
    pub allocated_bytes: usize,
}
impl ExportPageBudget {
    pub fn validate(self) -> Result<(), LogError> {
        if self.events == 0 || self.encoded_bytes == 0 || self.allocated_bytes == 0 {
            return Err(LogError::InvalidPageBudget);
        }
        scan_limit(self.examined).map(|_| ())
    }
}
pub(crate) fn scan_limit(examined: usize) -> Result<i64, LogError> {
    if examined == 0 {
        return Err(LogError::InvalidPageBudget);
    }
    i64::try_from(examined)
        .ok()
        .and_then(|n| n.checked_add(1))
        .ok_or(LogError::InvalidPageBudget)
}

/// EOF is exclusively `next == None`. Empty items with a continuation are
/// scanning progress and must not be reported as convergence.
#[derive(Debug)]
pub struct SyncPage<T, C> {
    pub items: Vec<T>,
    pub next: Option<C>,
    pub examined: usize,
}
#[derive(Debug)]
pub struct ExportPage<C> {
    pub events: Vec<Event>,
    pub next: Option<C>,
    pub examined: usize,
    pub encoded_bytes: usize,
    pub allocated_bytes: usize,
}

/// A source cutoff freezes membership, never authorization. Every selector and
/// `validate_view` must check current destructive/authority bindings. Views own
/// leases and scalar context, never a borrowed lock/transaction or all history.
/// Local failures propagate with their original source. `StaleCursor` identifies
/// invalid view/cursor context; allocation/frame failures use typed budget errors.
pub trait NetworkSyncStore {
    type View;
    type HaveCursor;
    type ExportCursor;
    fn open_view(&self, conversation: &ConversationId) -> Result<Self::View, LogError>;
    fn metadata(&self, view: &Self::View) -> Result<SyncMetadata, LogError>;
    /// Last policy check immediately before a bounded frame is handed to transport.
    fn validate_view(&self, view: &Self::View) -> Result<(), LogError>;
    fn have_page(
        &self,
        view: &Self::View,
        after: Option<&Self::HaveCursor>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, Self::HaveCursor>, LogError>;
    fn export_page(
        &self,
        view: &Self::View,
        have: &HashSet<EventId>,
        after: Option<&Self::ExportCursor>,
        budget: ExportPageBudget,
    ) -> Result<ExportPage<Self::ExportCursor>, LogError>;
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError>;
    fn durable_have(&self, conversation: &ConversationId, id: &EventId) -> Result<bool, LogError>;
    fn file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<FileConsentStatus, LogError>;
    fn signed_file_consent(
        &self,
        file: &ConversationId,
        manifest: &EventId,
    ) -> Result<Option<Event>, LogError>;
    /// Optional signed logical attachment identity; never part of public history.
    fn file_message_identity(
        &self,
        _file: &ConversationId,
        _manifest: &EventId,
    ) -> Result<Option<Event>, LogError> {
        Ok(None)
    }
    fn accept_file_message_identity(&mut self, _bundle: Event) -> Result<bool, LogError> {
        Ok(false)
    }
    /// Optional exact sticker reconstruction from ciphertext-free signed headers.
    fn sticker_file_consent(
        &mut self,
        _file: &ConversationId,
        _manifest: &EventId,
        _headers: Vec<Event>,
    ) -> Result<Option<Event>, LogError> {
        Ok(None)
    }
    /// Optional metadata-only recovery for an already authorized exact file offer.
    fn sticker_file_proof(
        &self,
        _file: &ConversationId,
        _manifest: &EventId,
    ) -> Result<Option<Vec<Event>>, LogError> {
        Ok(None)
    }
    fn admission_denied(&self) -> bool;
}

/// Shared received-batch gate, independent of any collecting export API.
/// A second local write failure preserves the already committed first event.
pub(crate) fn apply_received_events(
    events: &[Event],
    mut ingest: impl FnMut(Event) -> Result<AppendOutcome, LogError>,
) -> Result<ApplyReport, LogError> {
    let mut report = ApplyReport::default();
    for event in events {
        match ingest(event.clone()) {
            Ok(AppendOutcome::Appended) => report.applied += 1,
            Ok(AppendOutcome::Duplicate) => report.duplicates += 1,
            Err(error) if error.is_peer_rejection() => {
                if report.rejected.len() < super::MAX_REJECTED_DETAIL {
                    report.rejected.push((event.id, error.to_string()));
                }
            }
            Err(error) => return Err(error),
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_budgets_reject_zero_overflow_and_oversized_have() {
        for budget in [
            IdPageBudget {
                ids: 0,
                examined: 1,
            },
            IdPageBudget {
                ids: 1501,
                examined: 1501,
            },
            IdPageBudget {
                ids: 1,
                examined: 0,
            },
            IdPageBudget {
                ids: 1,
                examined: usize::MAX,
            },
        ] {
            assert!(matches!(
                budget.validate(),
                Err(LogError::InvalidPageBudget)
            ));
        }
        assert!(
            IdPageBudget {
                ids: 1500,
                examined: 1500
            }
            .validate()
            .is_ok()
        );
        let mut budget = ExportPageBudget {
            examined: 1,
            events: 1,
            encoded_bytes: 1,
            allocated_bytes: 1,
        };
        assert!(budget.validate().is_ok());
        budget.examined = usize::MAX;
        assert!(matches!(
            budget.validate(),
            Err(LogError::InvalidPageBudget)
        ));
    }

    #[test]
    fn empty_page_with_cursor_is_not_eof() {
        let page = SyncPage::<EventId, u64> {
            items: vec![],
            next: Some(7),
            examined: 3,
        };
        assert!(page.items.is_empty());
        assert!(page.next.is_some());
    }
}
