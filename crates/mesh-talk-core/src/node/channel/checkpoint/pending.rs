//! Failed authentication waits for new key material; gaps wait for a reachable
//! receiver position. Indexed
//! per-chain wake work makes unchanged polls independent of stalled history.
use super::*;
use crate::eventlog::event::EventKind;

pub(in crate::node) struct PendingChannelPage {
    pub(in crate::node) ids: Vec<EventId>,
    /// Empty cleanup still consumed one bounded turn; another queued chain may
    /// be ready. A scheduler must not confuse this with exhaustion.
    pub(in crate::node) more_work: bool,
}
/// Two successful opens suffice to distinguish live delivery from catch-up.
/// Neither a transcript body nor its consuming key is published by this preview.
pub(in crate::node) struct PreparedMessageTurn {
    pub(in crate::node) candidates: Vec<PreparedChannelReceive>,
    pub(in crate::node) more_work: bool,
}
/// One synchronous producer invocation shares this budget across successful and
/// failed previews. Selector cleanup also spends a turn even without a body.
pub(in crate::node) struct ChannelReceiveBudget {
    bodies: usize,
    bytes: usize,
    pages: usize,
}
impl Default for ChannelReceiveBudget {
    fn default() -> Self {
        Self {
            bodies: 64,
            // Two individually admissible rows must always fit together so a
            // lone successful preview cannot starve behind the next large row.
            bytes: 2 * super::super::CHANNEL_PENDING_BYTES,
            pages: 64,
        }
    }
}
impl ChannelReceiveBudget {
    pub(in crate::node) fn exhausted(&self) -> bool {
        self.bodies == 0 || self.pages == 0
    }
}
impl ChannelCheckpointStore {
    #[cfg(test)]
    pub(in crate::node) fn prepare_message_turn(
        &self,
        repository: &crate::eventlog::repository::EventRepository,
        channel: ConversationId,
    ) -> Result<PreparedMessageTurn, LogError> {
        self.prepare_message_turn_with_budget(
            repository,
            channel,
            &mut ChannelReceiveBudget::default(),
        )
    }
    pub(in crate::node) fn prepare_message_turn_with_budget(
        &self,
        repository: &crate::eventlog::repository::EventRepository,
        channel: ConversationId,
        budget: &mut ChannelReceiveBudget,
    ) -> Result<PreparedMessageTurn, LogError> {
        let mut candidates = Vec::with_capacity(2);
        loop {
            if budget.exhausted() {
                // A lone success remains durable pending work. Its private
                // receiver candidate cannot establish live/catch-up yet.
                return Ok(PreparedMessageTurn {
                    candidates: Vec::new(),
                    more_work: true,
                });
            }
            budget.pages -= 1;
            let page = self.pending_page_excluding(
                channel,
                EventKind::Message,
                candidates.first().map(PreparedChannelReceive::event_id),
            )?;
            for id in page.ids {
                let header = repository
                    .header(&id)?
                    .ok_or_else(|| invalid("pending channel event disappeared"))?;
                let charge = pending_charge(header.stored_bytes)?;
                if budget.bodies == 0 || charge > budget.bytes {
                    budget.bodies = 0;
                    return Ok(PreparedMessageTurn {
                        candidates: Vec::new(),
                        more_work: true,
                    });
                }
                budget.bodies -= 1;
                budget.bytes -= charge;
                let event = repository
                    .get(&id)?
                    .ok_or_else(|| invalid("pending channel event disappeared"))?;
                let candidate = if let Some(prior) = candidates.first() {
                    self.prepare_receive_following(&event, prior)?
                } else {
                    self.prepare_receive(&event)?
                };
                if let Some(candidate) = candidate {
                    candidates.push(candidate);
                    if candidates.len() == 2 {
                        return Ok(PreparedMessageTurn {
                            candidates,
                            more_work: true,
                        });
                    }
                }
            }
            if !page.more_work {
                return Ok(PreparedMessageTurn {
                    candidates,
                    more_work: false,
                });
            }
        }
    }
    pub(in crate::node) fn pending_page(
        &self,
        channel: ConversationId,
        kind: EventKind,
    ) -> Result<PendingChannelPage, LogError> {
        self.pending_page_excluding(channel, kind, None)
    }
    fn pending_page_excluding(
        &self,
        channel: ConversationId,
        kind: EventKind,
        excluded: Option<EventId>,
    ) -> Result<PendingChannelPage, LogError> {
        if !matches!(kind, EventKind::Message | EventKind::FileManifest) {
            return Err(LogError::InvalidPageBudget);
        }
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            let binding:Option<(Vec<u8>,i64)>=c.query_row("SELECT source_job,source_revision FROM channel_current WHERE channel=?1 AND owner_account=?2",params![channel.as_bytes().as_slice(),self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            let Some((job,revision))=binding else{return Ok(PendingChannelPage{ids:Vec::new(),more_work:false});};
            replay::require_source_on(c,channel,job.try_into().map_err(|_|invalid("invalid channel job"))?,revision)?;
            c.execute("INSERT OR IGNORE INTO channel_pending_schedule VALUES(?1,?2,0,NULL,NULL)",params![channel.as_bytes().as_slice(),kind as i64])?;
            let (prefer_retry,after_author,after_epoch):(bool,Option<String>,Option<Vec<u8>>)=c.query_row(
                "SELECT prefer_retry,after_author,after_epoch FROM channel_pending_schedule WHERE channel=?1 AND kind=?2",
                params![channel.as_bytes().as_slice(),kind as i64],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            if prefer_retry && let Some(page)=retry_page(c,channel,kind,after_author.as_deref(),after_epoch.as_deref(),excluded)? {
                return Ok(page);
            }
            let ids=read_page(c,"SELECT p.event,e.stored_bytes FROM channel_pending p INDEXED BY channel_pending_new JOIN retained_events e ON e.domain=?1 AND e.id=p.event WHERE p.channel=?2 AND p.kind=?3 AND p.reason=0 AND p.admitted=1 AND (?4 IS NULL OR p.event<>?4) ORDER BY p.ordinal,p.event LIMIT 64",params![b"MeshPartEventsV1".as_slice(),channel.as_bytes().as_slice(),kind as i64,excluded.as_ref().map(|id|id.as_bytes().as_slice())])?;
            if !ids.is_empty() {
                c.execute("UPDATE channel_pending_schedule SET prefer_retry=1 WHERE channel=?1 AND kind=?2",params![channel.as_bytes().as_slice(),kind as i64])?;
                return Ok(PendingChannelPage{ids,more_work:true});
            }
            if !prefer_retry && let Some(page)=retry_page(c,channel,kind,after_author.as_deref(),after_epoch.as_deref(),excluded)? {
                return Ok(page);
            }
            Ok(PendingChannelPage{ids:Vec::new(),more_work:false})
        })
    }
    pub(super) fn wake_chain_on(
        c: &Connection,
        channel: ConversationId,
        author: &str,
        epoch: u64,
        revision: i64,
        position: u32,
    ) -> Result<(), LogError> {
        for kind in [EventKind::Message, EventKind::FileManifest] {
            c.execute("INSERT INTO channel_retry_chains VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(channel,kind,author,epoch) DO UPDATE SET revision=MAX(revision,excluded.revision),next_n=MAX(next_n,excluded.next_n)",params![channel.as_bytes().as_slice(),author,epoch.to_be_bytes().as_slice(),kind as i64,revision,i64::from(position)])?;
        }
        Ok(())
    }
    pub(super) fn note_attempt(
        &self,
        event: &Event,
        epoch: Option<u64>,
        revision: i64,
        job: [u8; 16],
        source_revision: i64,
        wait_until: Option<u32>,
    ) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check_account_on(c)?;
            replay::require_source_on(c,event.conversation_id,job,source_revision)?;
            let Some(epoch)=epoch else {
                // A malformed authenticated header cannot become decryptable.
                // Retire generic discovery in the same definitive transaction.
                crate::node::received_store::ReceivedStore::install_evidence_on(c,event.conversation_id,event.id)?;
                c.execute("INSERT OR IGNORE INTO channel_processed VALUES(?1,?2,3)",params![event.conversation_id.as_bytes().as_slice(),event.id.as_bytes().as_slice()])?;
                c.execute("DELETE FROM channel_pending WHERE channel=?1 AND event=?2",params![event.conversation_id.as_bytes().as_slice(),event.id.as_bytes().as_slice()])?;
                return Ok(());
            };
            c.execute("UPDATE channel_pending SET epoch=?3,attempt_job=?4,attempt_revision=?5,reason=CASE WHEN ?6 IS NULL THEN 1 ELSE 3 END,wake_at=COALESCE(?6,0) WHERE channel=?1 AND event=?2",params![event.conversation_id.as_bytes().as_slice(),event.id.as_bytes().as_slice(),epoch.to_be_bytes().as_slice(),job.as_slice(),revision,wait_until.map(i64::from)])?;
            Ok(())
        })
    }
}
fn retry_page(
    c: &Connection,
    channel: ConversationId,
    kind: EventKind,
    after_author: Option<&str>,
    after_epoch: Option<&[u8]>,
    excluded: Option<EventId>,
) -> Result<Option<PendingChannelPage>, LogError> {
    type Chain = (String, Vec<u8>, i64, u32);
    let mut chain: Option<Chain> = c.query_row(
        "SELECT author,epoch,revision,next_n FROM channel_retry_chains WHERE channel=?1 AND kind=?2 AND (author,epoch)>(?3,?4) ORDER BY author,epoch LIMIT 1",
        params![channel.as_bytes().as_slice(),kind as i64,after_author.unwrap_or(""),after_epoch.unwrap_or(&[])],
        |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    if chain.is_none() && after_author.is_some() {
        chain=c.query_row("SELECT author,epoch,revision,next_n FROM channel_retry_chains WHERE channel=?1 AND kind=?2 ORDER BY author,epoch LIMIT 1",
            params![channel.as_bytes().as_slice(),kind as i64],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?))).optional()?;
    }
    let Some((author, epoch, revision, position)) = chain else {
        return Ok(None);
    };
    let mut ids = read_page(
        c,
        "SELECT p.event,e.stored_bytes FROM channel_pending p INDEXED BY channel_pending_retry JOIN retained_events e ON e.domain=?1 AND e.id=p.event WHERE p.channel=?2 AND p.author=?3 AND p.epoch=?4 AND p.kind=?5 AND p.reason=1 AND p.admitted=1 AND p.attempt_revision<?6 AND (?7 IS NULL OR p.event<>?7) ORDER BY p.attempt_revision,p.ordinal,p.event LIMIT 64",
        params![
            b"MeshPartEventsV1".as_slice(),
            channel.as_bytes().as_slice(),
            &author,
            &epoch,
            kind as i64,
            revision,
            excluded.as_ref().map(|id| id.as_bytes().as_slice())
        ],
    )?;
    if ids.is_empty() {
        ids = read_page(
            c,
            "SELECT p.event,e.stored_bytes FROM channel_pending p INDEXED BY channel_pending_gap JOIN retained_events e ON e.domain=?1 AND e.id=p.event WHERE p.channel=?2 AND p.author=?3 AND p.epoch=?4 AND p.kind=?5 AND p.reason=3 AND p.admitted=1 AND p.wake_at<=?6 AND (?7 IS NULL OR p.event<>?7) ORDER BY p.wake_at,p.ordinal,p.event LIMIT 64",
            params![
                b"MeshPartEventsV1".as_slice(),
                channel.as_bytes().as_slice(),
                &author,
                &epoch,
                kind as i64,
                i64::from(position),
                excluded.as_ref().map(|id| id.as_bytes().as_slice())
            ],
        )?;
    }
    c.execute("UPDATE channel_pending_schedule SET prefer_retry=0,after_author=?3,after_epoch=?4 WHERE channel=?1 AND kind=?2",
        params![channel.as_bytes().as_slice(),kind as i64,&author,&epoch])?;
    if ids.is_empty() {
        c.execute("DELETE FROM channel_retry_chains WHERE channel=?1 AND kind=?2 AND author=?3 AND epoch=?4 AND revision=?5",params![channel.as_bytes().as_slice(),kind as i64,author,epoch,revision])?;
    }
    Ok(Some(PendingChannelPage {
        ids,
        more_work: true,
    }))
}

pub(super) fn read_page(
    c: &Connection,
    sql: &str,
    parameters: impl rusqlite::Params,
) -> Result<Vec<EventId>, LogError> {
    let mut statement = c.prepare(sql)?;
    let mut rows = statement.query(parameters)?;
    let mut result = Vec::new();
    let mut bytes = 0usize;
    while let Some(row) = rows.next()? {
        let stored_bytes: i64 = row.get(1)?;
        // Charge the complete signed body, including parent IDs, plus decoded
        // event/plaintext copies. Receiver key maps have their own fixed bound.
        let charge = pending_charge(
            usize::try_from(stored_bytes).map_err(|_| invalid("invalid channel pending charge"))?,
        )?;
        if bytes
            .checked_add(charge)
            .is_none_or(|total| total > super::super::CHANNEL_PENDING_BYTES)
        {
            if result.is_empty() {
                return Err(LogError::PageBudgetTooSmall { required: charge });
            }
            break;
        }
        let id: Vec<u8> = row.get(0)?;
        result.push(EventId::new(
            id.try_into()
                .map_err(|_| invalid("invalid channel pending identity"))?,
        ));
        bytes += charge;
    }
    Ok(result)
}

fn pending_charge(stored_bytes: usize) -> Result<usize, LogError> {
    stored_bytes
        .checked_mul(4)
        .and_then(|n| n.checked_add(1024))
        .ok_or_else(|| invalid("invalid channel pending charge"))
}
