//! One persisted pending scope at a time; logical hiding never frees charge.
use super::*;
use crate::eventlog::repository::retirement::{
    RetirementBegin, RetirementBudget, RetirementProgress,
};
use std::time::{Duration, Instant};

const SELECT_PENDING: &str =
    "SELECT conversation FROM relay_mailboxes WHERE domain=?1 AND pending=1 LIMIT 1";
const SELECT_EXPIRED: &str = "SELECT conversation FROM relay_mailboxes WHERE domain=?1 AND pending=0 AND first_seen<=?2 ORDER BY first_seen,conversation LIMIT 1";
const SELECT_LRU: &str = "SELECT conversation FROM relay_mailboxes WHERE domain=?1 AND pending=0 ORDER BY touched,conversation LIMIT 1";

#[derive(Clone, Copy)]
pub(crate) struct RelayMaintenanceBudget {
    /// Pending, expiry/LRU candidate, and selected metadata probes; fixed
    /// readiness/cutoff/physical charge authority checks are constant overhead.
    pub examined: usize,
    pub selected_scopes: usize,
    pub reclaim: RetirementBudget,
}
impl Default for RelayMaintenanceBudget {
    fn default() -> Self {
        Self {
            examined: 4,
            selected_scopes: 1,
            reclaim: RetirementBudget::default(),
        }
    }
}
impl RelayMaintenanceBudget {
    #[cfg(test)]
    pub(crate) fn tiny() -> Self {
        Self {
            examined: 4,
            selected_scopes: 1,
            reclaim: RetirementBudget {
                auxiliary: 1,
                edges: 1,
                vertices: 1,
                bytes: 256,
            },
        }
    }
}
#[derive(Default)]
pub(crate) struct RelayMaintenanceProgress {
    pub examined: usize,
    pub selected_scopes: usize,
    pub reclaim: Option<RetirementProgress>,
    pub transaction_latency: Option<Duration>,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RelayWork {
    pub next_expiry: Option<u64>,
    pub pending_retirement: bool,
    pub expired: bool,
    pub drain_requested: bool,
}
impl RelayWork {
    pub(crate) fn has_work(self) -> bool {
        self.pending_retirement || self.expired || self.drain_requested
    }
}
impl IndexedRelayState {
    fn selected_id(value: Option<Vec<u8>>) -> Result<Option<ConversationId>, LogError> {
        value
            .map(|v| {
                Ok(ConversationId::new(
                    v.try_into()
                        .map_err(|_| invalid("invalid selected relay scope"))?,
                ))
            })
            .transpose()
    }
    pub(super) fn pending_on(&self, c: &Connection) -> Result<Option<ConversationId>, LogError> {
        Self::selected_id(
            c.query_row(SELECT_PENDING, [RELAY_ARRIVALS.as_slice()], |r| r.get(0))
                .optional()?,
        )
    }
    fn expired_on(
        &self,
        c: &Connection,
        cutoff: Option<u64>,
    ) -> Result<Option<ConversationId>, LogError> {
        let Some(cutoff) = cutoff else {
            return Ok(None);
        };
        Self::selected_id(
            c.query_row(
                SELECT_EXPIRED,
                params![RELAY_ARRIVALS.as_slice(), cutoff.to_be_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()?,
        )
    }
    fn lru_on(&self, c: &Connection) -> Result<Option<ConversationId>, LogError> {
        Self::selected_id(
            c.query_row(SELECT_LRU, [RELAY_ARRIVALS.as_slice()], |r| r.get(0))
                .optional()?,
        )
    }
    fn notice_imported_pressure_on(&self, c: &Connection) -> Result<(), LogError> {
        if self
            .limits
            .above_envelope(self.repository.domain_charge_on(c)?)
        {
            self.request_drain_on(c)?
        }
        Ok(())
    }
    pub(crate) fn next_work_at(&self, now: u64) -> Result<RelayWork, LogError> {
        self.policy_transaction(now, |c, cutoff| {
            self.notice_imported_pressure_on(c)?;
            let pending = self.pending_on(c)?;
            if let Some(conv) = pending {
                let m = self
                    .mailbox_on(c, &conv)?
                    .ok_or_else(|| invalid("pending relay arrival missing"))?;
                if !m.retained.retiring {
                    return Err(invalid("pending relay scope is active"));
                }
            }
            let expired = self.expired_on(c, cutoff)?;
            if let Some(conv) = expired {
                self.mailbox_on(c, &conv)?
                    .ok_or_else(|| invalid("expired candidate arrival missing"))?;
            }
            Ok(RelayWork {
                next_expiry: self.next_arrival_on(c, cutoff)?.and_then(|first| {
                    first.checked_add(super::super::super::MAX_RELAY_AGE.as_secs())
                }),
                pending_retirement: pending.is_some(),
                expired: expired.is_some(),
                drain_requested: self.drain_on(c)?,
            })
        })
    }
    pub(crate) fn maintenance_step_at(
        &self,
        now: u64,
        b: RelayMaintenanceBudget,
    ) -> Result<RelayMaintenanceProgress, LogError> {
        if b.examined < 4
            || b.selected_scopes == 0
            || b.reclaim.auxiliary == 0
            || b.reclaim.edges == 0
            || b.reclaim.vertices == 0
            || b.reclaim.bytes < 256
        {
            return Err(LogError::InvalidPageBudget);
        }
        let started = Instant::now();
        let mut p=self.policy_transaction(now,|c,cutoff|{
            self.notice_imported_pressure_on(c)?;
            let mut p=RelayMaintenanceProgress::default();
            p.examined+=1;
            let pending=self.pending_on(c)?;
            let token=if let Some(conv)=pending {
                p.examined+=1;
                let m=self.mailbox_on(c,&conv)?.ok_or_else(||invalid("pending arrival missing"))?;
                if !m.retained.retiring{return Err(invalid("pending relay lifecycle changed"))}
                let token=self.repository.resume_retirement_on(c,&conv)?.ok_or_else(||invalid("pending relay job missing"))?;
                Some((conv,token))
            }else{
                p.examined+=1;
                let mut candidate=self.expired_on(c,cutoff)?;
                if candidate.is_none() && self.drain_on(c)? && !self.limits.at_base(self.repository.domain_charge_on(c)?) {
                    p.examined+=1;
                    candidate=self.lru_on(c)?;
                    if candidate.is_none(){return Err(invalid("relay physical charge lacks reclaimable scope"))}
                }
                if let Some(conv)=candidate {
                    p.examined+=1;
                    let m=self.mailbox_on(c,&conv)?.ok_or_else(||invalid("selected relay arrival missing"))?;
                    if m.retained.retiring{return Err(invalid("active selector chose pending scope"))}
                    let token=match self.repository.begin_retirement_on(c,&conv)? {
                        RetirementBegin::Started(token)=>token,
                        RetirementBegin::Existing(token)=>{drop(token);return Err(invalid("relay begin and selected lifecycle disagree"))},
                        RetirementBegin::Absent=>return Err(invalid("relay begin and selected lifecycle disagree")),
                    };
                    if c.execute("UPDATE relay_mailboxes SET pending=1 WHERE domain=?1 AND conversation=?2 AND pending=0",params![RELAY_ARRIVALS.as_slice(),conv.as_bytes().as_slice()])?!=1{return Err(invalid("relay begin lost arrival"))}
                    inventory::retire_on(c,&conv)?;
                    fault_cut(c,"begin")?;
                    p.selected_scopes=1;
                    Some((conv,token))
                }else{None}
            };
            if let Some((conv,token))=token {
                let progress=self.repository.reclaim_retirement_on(c,&token,b.reclaim)?;
                fault_cut(c,"reclaim")?;
                if progress.complete {
                    // The freshly resumed token was validated before graph work.
                    // Finalization and this exact pending arrival removal share
                    // the same mutex/transaction; recreation cannot interleave.
                    if c.execute("DELETE FROM relay_mailboxes WHERE domain=?1 AND conversation=?2 AND pending=1",params![RELAY_ARRIVALS.as_slice(),conv.as_bytes().as_slice()])?!=1{return Err(invalid("relay finalization lost pending arrival"))}
                    fault_cut(c,"finalize")?;
                }
                p.reclaim=Some(progress);
            }
            if self.drain_on(c)? && self.limits.at_base(self.repository.domain_charge_on(c)?) {
                c.execute("UPDATE relay_runtime SET drain_requested=0 WHERE singleton=1 AND drain_requested=1",[])?;
            }
            Ok(p)
        })?;
        p.transaction_latency = Some(started.elapsed());
        Ok(p)
    }
}

#[cfg(test)]
mod tests;
