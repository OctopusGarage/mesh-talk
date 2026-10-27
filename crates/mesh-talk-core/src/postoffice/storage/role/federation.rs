//! Session authority, exact origin lookup and acceptance share one transaction.
use super::*;
use crate::postoffice::storage::federation::{OriginCursor, SessionLease};
pub(crate) struct OriginCorrectionProgress {
    pub examined: usize,
    pub after: Option<OriginCursor>,
}
impl IndexedRelayState {
    pub(crate) fn record_with_session_at(
        &self,
        event: &Event,
        now: u64,
        session: &SessionLease,
    ) -> Result<AppendOutcome, LogError> {
        if !session.belongs_to(&self.profile) {
            return Err(LogError::StaleCursor);
        }
        self.session_policy_transaction(now, session, true, |c, cutoff| {
            let origin = session.origin_on(c, &event.conversation_id, now)?;
            self.record_on(c, cutoff, event, now, origin)
        })
    }
    pub(crate) fn correct_known_origins_at(
        &self,
        session: &SessionLease,
        after: Option<&OriginCursor>,
        examined: usize,
        now: u64,
    ) -> Result<OriginCorrectionProgress, LogError> {
        if !session.belongs_to(&self.profile) {
            return Err(LogError::StaleCursor);
        }
        self.session_policy_transaction(now,session,true,|c,_|{
            let page=session.origins_page_on(c,after,examined,now)?;
            for (id,first) in &page.entries {
                if let Some(prior)=self.mailbox_on(c,id)? && !prior.retained.retiring && *first<prior.first_seen {
                    c.execute("UPDATE relay_mailboxes SET first_seen=?3 WHERE domain=?1 AND conversation=?2",params![RELAY_ARRIVALS.as_slice(),id.as_bytes().as_slice(),first.to_be_bytes().as_slice()])?;
                    inventory::invalidate_visibility_on(c)?;
                }
            }
            self.advance_cutoff_on(c,now)?;
            Ok(OriginCorrectionProgress{examined:page.entries.len(),after:page.after})
        })
    }
}
