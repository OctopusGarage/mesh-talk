//! Recent bodies come exclusively from the qualified outgoing authorization view.
use super::*;
use crate::eventlog::{
    repository::selection::RecentCursor,
    sync::network::{RecentPage, RecentStatus},
};
use bincode::Options;
impl GuardedSyncStore<'_> {
    pub(super) fn recent_history_page(
        &self,
        view: &ParticipantView,
        cursor: Option<&[u8]>,
        records: usize,
        bytes: usize,
    ) -> Result<RecentPage, LogError> {
        if records == 0
            || records > 200
            || bytes == 0
            || bytes > 60 * 1024
            || cursor.is_some_and(|c| c.len() > 512)
        {
            return Err(LogError::InvalidPageBudget);
        }
        let (supported, work) = self.requested_prepare(view, PreparationBudget::default())?;
        if !supported {
            return Ok(RecentPage::status(RecentStatus::Unsupported));
        }
        if !work.ready || work.rows != 0 || work.edges != 0 || work.bytes != 0 {
            return Ok(RecentPage::status(RecentStatus::More));
        }
        view.recent_ready.store(true, Ordering::Release);
        let cursor: Option<RecentCursor> = cursor
            .map(|bytes| {
                bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(512)
                    .reject_trailing_bytes()
                    .deserialize(bytes)
                    .map_err(|_| LogError::StaleCursor)
            })
            .transpose()?;
        let prep = view.preparation.lock().expect("view preparation");
        let log = self.node.log.lock().expect("log lock");
        let repo = log.repository()?;
        let page = repo.selected_recent_message_page(
            prep.selection()?,
            &view.repository,
            cursor.as_ref(),
            IdPageBudget {
                ids: records,
                examined: 1000,
            },
        )?;
        let mut events = Vec::new();
        let mut remaining = bytes;
        let mut next = page.next;
        let examined = page.examined;
        for (i, id) in page.ids.iter().enumerate() {
            let event = match repo.selected_upload_event(
                prep.selection()?,
                &view.repository,
                *id,
                i64::MAX as u64,
                remaining.saturating_sub(512).max(1),
            ) {
                Ok(Some(event)) => event,
                Err(LogError::PageBudgetTooSmall { .. }) => {
                    if i == 0 {
                        // Explicit gap navigation, never a commit acknowledgement.
                        let cursor = RecentCursor::at(
                            prep.selection()?,
                            &view.repository,
                            page.positions[0],
                        );
                        let next = bincode::serialize(&cursor)
                            .map_err(|e| LogError::CorruptFile(e.to_string()))?;
                        return Ok(RecentPage {
                            status: RecentStatus::Blocked,
                            events: vec![],
                            next: Some(next),
                            examined: 1,
                        });
                    }
                    next = Some(RecentCursor::at(
                        prep.selection()?,
                        &view.repository,
                        page.positions[i - 1],
                    ));
                    break;
                }
                Ok(None) => return Err(LogError::StaleCursor),
                Err(e) => return Err(e),
            };
            let size = usize::try_from(
                bincode::serialized_size(&event)
                    .map_err(|e| LogError::CorruptFile(e.to_string()))?,
            )
            .map_err(|_| LogError::InvalidPageBudget)?;
            if size > remaining {
                return Err(LogError::InvalidPageBudget);
            }
            remaining -= size;
            events.push(event);
        }
        let next = next
            .map(|cursor| {
                bincode::serialize(&cursor).map_err(|e| LogError::CorruptFile(e.to_string()))
            })
            .transpose()?;
        drop(log);
        drop(prep);
        self.validate_view(view)?;
        Ok(RecentPage {
            status: RecentStatus::Ready,
            events,
            next,
            examined,
        })
    }
}
