//! Durable monotonic expiry policy; no history walk or idle mutation.
use super::*;
pub(super) const SELECT_NEXT_AFTER: &str = "SELECT first_seen FROM relay_mailboxes WHERE domain=?1 AND pending=0 AND first_seen>?2 ORDER BY first_seen,conversation LIMIT 1";
pub(super) const SELECT_NEXT_FIRST: &str = "SELECT first_seen FROM relay_mailboxes WHERE domain=?1 AND pending=0 ORDER BY first_seen,conversation LIMIT 1";
impl IndexedRelayState {
    pub(super) fn cutoff_on(&self, c: &Connection) -> Result<Option<u64>, LogError> {
        let value: Option<Vec<u8>> = c
            .prepare_cached("SELECT expired_cutoff FROM relay_runtime WHERE singleton=1")?
            .query_row([], |r| r.get(0))?;
        value.map(decode_time).transpose()
    }
    pub(super) fn next_arrival_on(
        &self,
        c: &Connection,
        cutoff: Option<u64>,
    ) -> Result<Option<u64>, LogError> {
        let value: Option<Vec<u8>> = if let Some(cutoff) = cutoff {
            c.prepare_cached(SELECT_NEXT_AFTER)?
                .query_row(
                    params![RELAY_ARRIVALS.as_slice(), cutoff.to_be_bytes().as_slice()],
                    |r| r.get(0),
                )
                .optional()?
        } else {
            c.prepare_cached(SELECT_NEXT_FIRST)?
                .query_row([RELAY_ARRIVALS.as_slice()], |r| r.get(0))
                .optional()?
        };
        value.map(decode_time).transpose()
    }
    pub(super) fn advance_cutoff_on(
        &self,
        c: &Connection,
        now: u64,
    ) -> Result<Option<u64>, LogError> {
        let cutoff = self.cutoff_on(c)?;
        let deadline = self
            .next_arrival_on(c, cutoff)?
            .and_then(|first| first.checked_add(super::super::super::MAX_RELAY_AGE.as_secs()));
        if deadline.is_some_and(|deadline| deadline <= now) {
            let candidate = now
                .checked_sub(super::super::super::MAX_RELAY_AGE.as_secs())
                .ok_or_else(|| invalid("relay deadline arithmetic disagrees"))?;
            let next = cutoff.map_or(candidate, |old| old.max(candidate));
            c.execute(
                "UPDATE relay_runtime SET expired_cutoff=?1 WHERE singleton=1",
                [next.to_be_bytes().as_slice()],
            )?;
            inventory::invalidate_visibility_on(c)?;
            fault_cut(c, "cutoff")?;
            return Ok(Some(next));
        }
        Ok(cutoff)
    }
    /// Expected local availability outcomes commit policy state before observation.
    /// All authoritative storage and validation failures retain rollback semantics.
    pub(super) fn policy_transaction<T>(
        &self,
        now: u64,
        operation: impl FnOnce(&Connection, Option<u64>) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        self.transaction_notifying(now, true, operation)
    }
    pub(super) fn derived_transaction<T>(
        &self,
        now: u64,
        operation: impl FnOnce(&Connection, Option<u64>) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        self.transaction_notifying(now, false, operation)
    }
    /// Session authority fences the whole callback, including initial failures
    /// after cutoff advancement. Expected availability is commit-preserving only
    /// while the current session still owns authority.
    pub(super) fn session_derived_transaction<T>(
        &self,
        now: u64,
        session: Option<&crate::postoffice::storage::federation::SessionLease>,
        complete: bool,
        operation: impl FnOnce(&Connection, Option<u64>) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        self.transaction_with_session(now, session, complete, false, operation)
    }
    pub(super) fn session_policy_transaction<T>(
        &self,
        now: u64,
        session: &crate::postoffice::storage::federation::SessionLease,
        complete: bool,
        operation: impl FnOnce(&Connection, Option<u64>) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        self.transaction_with_session(now, Some(session), complete, true, operation)
    }
    fn transaction_with_session<T>(
        &self,
        now: u64,
        session: Option<&crate::postoffice::storage::federation::SessionLease>,
        complete: bool,
        visible_writes: bool,
        operation: impl FnOnce(&Connection, Option<u64>) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        if session.is_some_and(|s| !s.belongs_to(&self.profile)) {
            return Err(LogError::StaleCursor);
        }
        self.transaction_notifying(now, visible_writes, |c, cutoff| {
            let result = (|| {
                if let Some(session) = session {
                    session.validate_on(c, complete)?;
                }
                operation(c, cutoff)
            })();
            match session {
                Some(session) => session.finish_on(c, complete, result),
                None => result,
            }
        })
    }
    fn transaction_notifying<T>(
        &self,
        now: u64,
        visible_writes: bool,
        operation: impl FnOnce(&Connection, Option<u64>) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        let (result, changed, visible_changed) = self.profile.database().transaction(|c| {
            require_ready(c)?;
            let before = c.total_changes();
            let visibility: i64 = c
                .prepare_cached(
                    "SELECT inventory_visibility_epoch FROM relay_runtime WHERE singleton=1",
                )?
                .query_row([], |r| r.get(0))?;
            let cutoff = self.advance_cutoff_on(c, now)?;
            let result = operation(c, cutoff);
            let result = match result {
                Err(
                    error @ (LogError::RelayMailboxUnavailable
                    | LogError::ConversationRetiring
                    | LogError::StaleCursor
                    | LogError::RelayCapacityUnavailable
                    | LogError::RelayEventTooLarge),
                ) => Err(error),
                Err(error) => return Err(error),
                Ok(value) => Ok(value),
            };
            fault_cut(c, "commit")?;
            let after_visibility: i64 = c
                .prepare_cached(
                    "SELECT inventory_visibility_epoch FROM relay_runtime WHERE singleton=1",
                )?
                .query_row([], |r| r.get(0))?;
            Ok((
                result,
                c.total_changes() != before,
                visibility != after_visibility,
            ))
        })?;
        if changed {
            self.work_changes
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        if changed && (visible_writes || visible_changed) {
            self.changes
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        result
    }
}

impl IndexedRelayState {
    pub(super) fn eligible_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        cutoff: Option<u64>,
    ) -> Result<Option<MailboxMetadata>, LogError> {
        let metadata = self.mailbox_on(c, conv)?;
        if let Some(m) = metadata {
            if m.retained.retiring {
                return Err(LogError::ConversationRetiring);
            }
            if cutoff.is_some_and(|cutoff| m.first_seen <= cutoff) {
                return Err(LogError::RelayMailboxUnavailable);
            }
        }
        Ok(metadata)
    }
    #[cfg(test)]
    pub(crate) fn mailbox_metadata_at(
        &self,
        conv: &ConversationId,
        now: u64,
    ) -> Result<Option<MailboxMetadata>, LogError> {
        self.policy_transaction(now, |c, cutoff| self.eligible_on(c, conv, cutoff))
    }
    #[cfg(test)]
    pub(crate) fn retained_event_at(
        &self,
        id: &EventId,
        now: u64,
    ) -> Result<Option<Event>, LogError> {
        self.policy_transaction(now, |c, cutoff| {
            let Some(conv) = self.repository.event_conversation_on(c, id)? else {
                return Ok(None);
            };
            self.eligible_on(c, &conv, cutoff)?
                .ok_or_else(|| invalid("retained relay event lacks arrival"))?;
            self.repository.get_on(c, id)
        })
    }
    pub(crate) fn durable_have_at(
        &self,
        conv: &ConversationId,
        id: &EventId,
        now: u64,
    ) -> Result<bool, LogError> {
        self.policy_transaction(now, |c, cutoff| {
            if self.eligible_on(c, conv, cutoff)?.is_none() {
                return Ok(false);
            }
            Ok(self
                .repository
                .event_conversation_on(c, id)?
                .is_some_and(|scope| scope == *conv))
        })
    }
    #[cfg(test)]
    pub(crate) fn policy_view_at(
        &self,
        conv: &ConversationId,
        now: u64,
    ) -> Result<Option<RelayPolicyView>, LogError> {
        self.policy_transaction(now, |c, cutoff| {
            let Some(metadata) = self.eligible_on(c, conv, cutoff)? else {
                return Ok(None);
            };
            Ok(Some(RelayPolicyView {
                repository: self.repository.policy_view_on(c, conv)?,
                conversation: *conv,
                cutoff,
                first_seen: metadata.first_seen,
            }))
        })
    }
    #[cfg(test)]
    pub(crate) fn validate_policy_view_at(
        &self,
        view: &RelayPolicyView,
        now: u64,
    ) -> Result<(), LogError> {
        self.policy_transaction(now, |c, cutoff| {
            // Compare policy identity before interpreting a newer scope's job.
            if cutoff != view.cutoff {
                return Err(LogError::StaleCursor);
            }
            let arrival = self.arrival_on(c, &view.conversation)?;
            if arrival.is_none_or(|a| a.pending || a.first_seen != view.first_seen) {
                return Err(LogError::StaleCursor);
            }
            // Repository binding includes actual connection, lease, UUID, domain
            // and destructive epoch. Append/touch do not retire a finite view.
            self.repository
                .validate_policy_view_on(c, &view.repository, &view.conversation)?;
            let metadata = match self.eligible_on(c, &view.conversation, cutoff) {
                Ok(Some(m)) => m,
                Ok(None)
                | Err(LogError::ConversationRetiring | LogError::RelayMailboxUnavailable) => {
                    return Err(LogError::StaleCursor);
                }
                Err(error) => return Err(error),
            };
            if cutoff != view.cutoff || metadata.first_seen != view.first_seen {
                return Err(LogError::StaleCursor);
            }
            Ok(())
        })
    }
}
