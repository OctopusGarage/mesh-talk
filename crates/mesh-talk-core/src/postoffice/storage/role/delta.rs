//! Finite append-only mailbox prefixes qualified by current relay policy.
use super::*;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MailboxCursor {
    pub store_uuid: [u8; 16],
    pub conversation: ConversationId,
    pub epoch: i64,
    pub first_seen: Option<u64>,
    pub revision: i64,
    pub after: i64,
    pub window: Option<(i64, i64)>,
}

/// Read-only invalidation hint; never a delivery checkpoint or receipt.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub(crate) struct MailboxWatchToken {
    pub store_uuid: [u8; 16],
    pub conversation: ConversationId,
    pub epoch: i64,
    pub first_seen: Option<u64>,
    pub revision: i64,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub(crate) struct MailboxDeltaPage {
    pub cursor: MailboxCursor,
    pub ids: Vec<EventId>,
    pub complete: bool,
    pub reset: bool,
}

const SELECT_DELTA: &str = "SELECT commit_ordinal,id FROM retained_events INDEXED BY retained_events_commit WHERE domain=?1 AND conversation=?2 AND commit_ordinal>=?3 AND commit_ordinal<?4 ORDER BY commit_ordinal,id LIMIT ?5";

impl MailboxCursor {
    fn accepts(&self, current: &Self) -> bool {
        self.store_uuid == current.store_uuid
            && self.conversation == current.conversation
            && self.epoch == current.epoch
            && self.first_seen == current.first_seen
            && self.revision >= 0
            && self.revision <= current.revision
            && self.after >= 0
            && self.after <= current.after
            && self.window.is_none_or(|(upper, revision)| {
                self.after <= upper
                    && upper <= current.after
                    && self.revision <= revision
                    && revision <= current.revision
            })
    }
}

impl IndexedRelayState {
    pub(crate) fn mailbox_watch_snapshot_at(
        &self,
        scopes: &[ConversationId],
        now: u64,
    ) -> Result<Vec<MailboxWatchToken>, LogError> {
        if scopes.len() > 64
            || scopes
                .iter()
                .enumerate()
                .any(|(i, c)| scopes[..i].contains(c))
        {
            return Err(LogError::InvalidPageBudget);
        }
        self.profile.database().read(|c| {
            require_ready(c)?;
            let uuid: Vec<u8> = c.prepare_cached("SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1")?.query_row([], |r| r.get(0))?;
            let store_uuid: [u8;16] = uuid.try_into().map_err(|_| invalid("invalid mailbox store identity"))?;
            let durable_cutoff: Option<Vec<u8>> = c.prepare_cached("SELECT expired_cutoff FROM relay_runtime WHERE singleton=1")?.query_row([], |r|r.get(0))?;
            let durable_cutoff = durable_cutoff.map(decode_time).transpose()?;
            scopes.iter().map(|conv| {
                let metadata = self.mailbox_on(c, conv)?;
                let first_seen = metadata.filter(|m| !m.retained.retiring && !durable_cutoff.is_some_and(|cutoff|m.first_seen<=cutoff) && m.first_seen.saturating_add(crate::postoffice::MAX_RELAY_AGE.as_secs()) > now).map(|m|m.first_seen);
                let (epoch,revision): (i64,i64) = c.prepare_cached("SELECT destructive_epoch,revision FROM event_conversations WHERE domain=?1 AND conversation=?2")?.query_row(params![RELAY_EVENTS.0.as_slice(),conv.as_bytes().as_slice()], |r|Ok((r.get(0)?,r.get(1)?))).optional()?.unwrap_or((0,0));
                if epoch < 0 || revision < 0 { return Err(invalid("negative mailbox watch authority")); }
                Ok(MailboxWatchToken { store_uuid, conversation:*conv, epoch, first_seen, revision })
            }).collect()
        })
    }

    fn mailbox_cursor_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        cutoff: Option<u64>,
    ) -> Result<MailboxCursor, LogError> {
        let mailbox = self.eligible_on(c, conv, cutoff)?;
        let uuid: Vec<u8> = c
            .prepare_cached("SELECT store_uuid FROM mesh_database_metadata WHERE singleton=1")?
            .query_row([], |r| r.get(0))?;
        let (epoch, revision): (i64, i64) = c.prepare_cached("SELECT destructive_epoch,revision FROM event_conversations WHERE domain=?1 AND conversation=?2")?.query_row(params![RELAY_EVENTS.0.as_slice(),conv.as_bytes().as_slice()], |r| Ok((r.get(0)?,r.get(1)?))).optional()?.unwrap_or((0,0));
        let after: i64 = c
            .prepare_cached("SELECT next_commit FROM event_domains WHERE domain=?1")?
            .query_row([RELAY_EVENTS.0.as_slice()], |r| r.get(0))?;
        if epoch < 0 || revision < 0 || after < 0 {
            return Err(invalid("negative mailbox checkpoint authority"));
        }
        Ok(MailboxCursor {
            store_uuid: uuid
                .try_into()
                .map_err(|_| invalid("invalid mailbox store identity"))?,
            conversation: *conv,
            epoch,
            first_seen: mailbox.map(|m| m.first_seen),
            revision,
            after,
            window: None,
        })
    }

    pub(crate) fn mailbox_delta_page_at(
        &self,
        conv: &ConversationId,
        prior: Option<&MailboxCursor>,
        limit: usize,
        now: u64,
    ) -> Result<MailboxDeltaPage, LogError> {
        crate::eventlog::sync::network::IdPageBudget {
            ids: limit,
            examined: limit,
        }
        .validate()?;
        self.session_transaction(now, None, |c, cutoff| {
            let current = self.mailbox_cursor_on(c, conv, cutoff)?;
            let reset = prior.is_some_and(|p| !p.accepts(&current));
            let prior = prior.filter(|p| !reset && p.accepts(&current));
            if let Some(prior) =
                prior.filter(|p| p.window.is_none() && p.revision == current.revision)
            {
                return Ok(MailboxDeltaPage {
                    cursor: prior.clone(),
                    ids: vec![],
                    complete: true,
                    reset: false,
                });
            }
            let mut cursor = prior.cloned().unwrap_or_else(|| MailboxCursor {
                after: 0,
                revision: 0,
                ..current.clone()
            });
            let (upper, revision) = cursor.window.unwrap_or((current.after, current.revision));
            let mut stmt = c.prepare_cached(SELECT_DELTA)?;
            let mut rows = stmt.query(params![
                RELAY_EVENTS.0.as_slice(),
                conv.as_bytes().as_slice(),
                cursor.after,
                upper,
                (limit + 1) as i64
            ])?;
            let mut ids = Vec::with_capacity(limit);
            let mut complete = true;
            while let Some(row) = rows.next()? {
                if ids.len() == limit {
                    complete = false;
                    break;
                }
                let ordinal: i64 = row.get(0)?;
                let id: Vec<u8> = row.get(1)?;
                ids.push(EventId::new(
                    id.try_into()
                        .map_err(|_| invalid("invalid mailbox event identity"))?,
                ));
                cursor.after = ordinal
                    .checked_add(1)
                    .ok_or(LogError::RepositoryCounterExhausted)?;
            }
            if complete {
                cursor.after = upper;
                cursor.revision = revision;
                cursor.window = None;
            } else {
                cursor.window = Some((upper, revision));
            }
            Ok(MailboxDeltaPage {
                cursor,
                ids,
                complete,
                reset,
            })
        })
    }

    pub(crate) fn mailbox_delta_events_at(
        &self,
        conv: &ConversationId,
        cursor: &MailboxCursor,
        ids: &[EventId],
        max_bytes: usize,
        now: u64,
    ) -> Result<Vec<Event>, LogError> {
        if ids.len() > 1500 || max_bytes == 0 || max_bytes > crate::transport::MAX_PLAINTEXT {
            return Err(LogError::InvalidPageBudget);
        }
        self.session_transaction(now, None, |c, cutoff| {
            let current = self.mailbox_cursor_on(c, conv, cutoff)?;
            if !cursor.accepts(&current) { return Err(LogError::StaleCursor); }
            let mut encoded = 0usize;
            let mut allocated = 0usize;
            // Check every selected scalar before reading the first body. No
            // supplied ID can escape its mailbox or the delivered prefix.
            for id in ids {
                let sizes: Option<(i64,i64)> = c.prepare_cached("SELECT encoded_bytes,stored_bytes FROM retained_events WHERE domain=?1 AND id=?2 AND conversation=?3 AND commit_ordinal<?4")?.query_row(params![RELAY_EVENTS.0.as_slice(), id.as_bytes().as_slice(),conv.as_bytes().as_slice(),cursor.after], |r| Ok((r.get(0)?,r.get(1)?))).optional()?;
                let (wire, stored) = sizes.ok_or(LogError::StaleCursor)?;
                let wire = usize::try_from(wire).map_err(|_| invalid("negative mailbox wire size"))?;
                let stored = usize::try_from(stored).map_err(|_| invalid("negative mailbox stored size"))?;
                encoded = encoded.checked_add(wire).ok_or(LogError::RepositoryCounterExhausted)?;
                allocated = stored.checked_mul(3).and_then(|n| n.checked_add(std::mem::size_of::<Event>())).and_then(|n| n.checked_add(allocated)).ok_or(LogError::RepositoryCounterExhausted)?;
            }
            if encoded > max_bytes { return Err(LogError::PageBudgetTooSmall { required: encoded }); }
            if allocated > 4 * 1024 * 1024 { return Err(LogError::PageBudgetTooSmall { required: allocated }); }
            ids.iter().map(|id| self.repository.get_on(c,id)?.ok_or(LogError::StaleCursor)).collect()
        })
    }
}

#[cfg(test)]
mod tests;
