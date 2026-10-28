//! Finite wire membership is independent of preparation progress.
use super::*;
use crate::postoffice::{
    FederatedMailbox,
    storage::federation::{SessionLease, local::MismatchProgress},
};
pub(crate) struct InventoryView {
    owner: Arc<ProfileStorage>,
    commit: i64,
    upper: Option<Vec<u8>>,
    cutoff: Option<u64>,
    visibility: i64,
    token: Arc<()>,
}
#[derive(Clone)]
pub(crate) struct InventoryCursor {
    after: Vec<u8>,
    token: Arc<()>,
}
pub(crate) struct InventoryPage {
    pub entries: Vec<FederatedMailbox>,
    pub examined: usize,
    pub after: Option<InventoryCursor>,
}
impl IndexedRelayState {
    pub(crate) fn inventory_view_at(&self, now: u64) -> Result<InventoryView, LogError> {
        self.derived_transaction(now,|c,cutoff|{
            self.start_preparation_on(c)?;
            let commit=c.query_row("SELECT next_commit FROM event_domains WHERE domain=?1",[RELAY_EVENTS.0.as_slice()],|r|r.get(0))?;
            let upper=c.query_row("SELECT conversation FROM relay_mailboxes WHERE domain=?1 ORDER BY conversation DESC LIMIT 1",[RELAY_ARRIVALS.as_slice()],|r|r.get(0)).optional()?;
            let visibility=c.query_row("SELECT inventory_visibility_epoch FROM relay_runtime WHERE singleton=1",[],|r|r.get(0))?;
            Ok(InventoryView{owner:self.profile.clone(),commit,upper,cutoff,visibility,token:Arc::new(())})
        })
    }
    fn validate_inventory_on(
        &self,
        c: &Connection,
        cutoff: Option<u64>,
        view: &InventoryView,
    ) -> Result<(), LogError> {
        if !Arc::ptr_eq(&self.profile, &view.owner) {
            return Err(LogError::StaleCursor);
        }
        let visibility: i64 = c
            .prepare_cached(
                "SELECT inventory_visibility_epoch FROM relay_runtime WHERE singleton=1",
            )?
            .query_row([], |r| r.get(0))?;
        if visibility != view.visibility || cutoff != view.cutoff {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn validate_inventory_at(
        &self,
        view: &InventoryView,
        now: u64,
    ) -> Result<(), LogError> {
        self.derived_transaction(now, |c, cutoff| self.validate_inventory_on(c, cutoff, view))
    }
    /// Bound raw metadata BEFORE eligibility/federated-ID/commit tests. A page
    /// with zero emitted but a continuation is progress, never EOF/convergence.
    #[cfg(test)]
    pub(crate) fn inventory_page_at(
        &self,
        view: &InventoryView,
        after: Option<&InventoryCursor>,
        examined: usize,
        now: u64,
    ) -> Result<InventoryPage, LogError> {
        self.inventory_page_inner_at(view, None, after, examined, now)
    }
    /// Select, cache-bind and persist the exact sent snapshot in one role Tx.
    pub(crate) fn selected_inventory_page_at(
        &self,
        view: &InventoryView,
        session: &SessionLease,
        after: Option<&InventoryCursor>,
        examined: usize,
        now: u64,
    ) -> Result<InventoryPage, LogError> {
        if !session.belongs_to(&self.profile) {
            return Err(LogError::StaleCursor);
        }
        self.inventory_page_inner_at(view, Some(session), after, examined, now)
    }
    fn inventory_page_inner_at(
        &self,
        view: &InventoryView,
        session: Option<&SessionLease>,
        after: Option<&InventoryCursor>,
        examined: usize,
        now: u64,
    ) -> Result<InventoryPage, LogError> {
        if examined == 0 || examined > PAGE {
            return Err(LogError::InvalidPageBudget);
        }
        if after.is_some_and(|a| !Arc::ptr_eq(&a.token, &view.token)) {
            return Err(LogError::StaleCursor);
        }
        self.session_derived_transaction(now,session,false,|c,cutoff|{
            self.validate_inventory_on(c,cutoff,view)?;
            let rows={let mut q=c.prepare_cached("SELECT conversation FROM relay_mailboxes WHERE domain=?1 AND conversation>?2 AND conversation<=?3 ORDER BY conversation LIMIT ?4")?;
                q.query_map(params![RELAY_ARRIVALS.as_slice(),after.map_or(&[][..],|a|a.after.as_slice()),view.upper,examined as i64],|r|r.get::<_,Vec<u8>>(0))?.collect::<Result<Vec<_>,_>>()?};
            let mut entries=Vec::new();
            for row in &rows {
                let conv=id(row.clone())?;
                if !super::super::super::super::opaque::RelayMailbox::is_federated_id(&conv){continue}
                let Some(binding)=self.digest_binding_on(c,&conv,cutoff)? else{continue};
                let commits:(Option<i64>,Option<i64>)=c.prepare_cached("SELECT (SELECT commit_ordinal FROM retained_events INDEXED BY retained_events_commit WHERE domain=?1 AND conversation=?2 ORDER BY commit_ordinal LIMIT 1),(SELECT commit_ordinal FROM retained_events INDEXED BY retained_events_commit WHERE domain=?1 AND conversation=?2 ORDER BY commit_ordinal DESC LIMIT 1)")?.query_row(params![RELAY_EVENTS.0.as_slice(),conv.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
                let (Some(first),Some(last))=commits else{return Err(invalid("eligible inventory mailbox lacks events"))};
                if first>=view.commit {continue}
                if last>=view.commit{return Err(LogError::StaleCursor)}
                let Some(cache)=self.cached_digest_on(c,&conv,binding)? else{
                    // Active exchanges may retain a bounded derived demand.
                    // A revoked exchange must roll back even this early miss.
                    self.demand_on(c,&conv,cutoff)?;
                    return Err(LogError::RelayCapacityUnavailable)
                };
                entries.push(FederatedMailbox{id:conv,digest:cache.federation,first_seen_secs:binding.first});
            }
            self.validate_inventory_on(c,cutoff,view)?;
            let after=(rows.len()==examined && rows.last()!=view.upper.as_ref()).then(||InventoryCursor{after:rows.last().expect("nonzero budget").clone(),token:view.token.clone()});
            if let Some(session)=session {
                session.stage_local_batch_on(c,&entries,now)?;
            }
            Ok(InventoryPage{entries,examined:rows.len(),after})
        })
    }
}

impl IndexedRelayState {
    pub(crate) fn finish_inventory_frame_at(
        &self,
        view: &InventoryView,
        session: &SessionLease,
        entries: usize,
        last: bool,
        now: u64,
    ) -> Result<(), LogError> {
        if !session.belongs_to(&self.profile) {
            return Err(LogError::StaleCursor);
        }
        self.session_derived_transaction(now, Some(session), false, |c, cutoff| {
            self.validate_inventory_on(c, cutoff, view)?;
            session.finish_local_frame_on(c, entries, last)
        })
    }
    pub(crate) fn begin_inventory_mismatch_at(
        &self,
        view: &InventoryView,
        session: &SessionLease,
        pivot: Option<&ConversationId>,
        now: u64,
    ) -> Result<(), LogError> {
        if !session.belongs_to(&self.profile) {
            return Err(LogError::StaleCursor);
        }
        self.session_derived_transaction(now, Some(session), true, |c, cutoff| {
            self.validate_inventory_on(c, cutoff, view)?;
            session.begin_mismatch_on(c, pivot)
        })
    }
    pub(crate) fn inventory_mismatch_step_at(
        &self,
        view: &InventoryView,
        session: &SessionLease,
        examined: usize,
        emitted: usize,
        now: u64,
    ) -> Result<MismatchProgress, LogError> {
        if !session.belongs_to(&self.profile) {
            return Err(LogError::StaleCursor);
        }
        self.session_derived_transaction(now, Some(session), true, |c, cutoff| {
            self.validate_inventory_on(c, cutoff, view)?;
            session.mismatch_step_on(c, examined, emitted)
        })
    }
}
