//! Internal physical retention foundation. Expiry visibility, retirement, bounded
//! reclamation and capacity policy must be added before any runtime serves it.
use super::*;
use crate::{
    eventlog::{
        AppendOutcome, ConversationId, Event, EventId,
        repository::{ConversationMetadata, DomainChargeMetadata},
    },
    storage::profile::context::ProfileStorage,
};
use std::sync::Arc;

#[derive(Clone, Copy)]
pub(crate) enum RelayAdmission {
    OpaqueOnly,
    #[cfg(test)]
    LegacyCompatible,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct MailboxMetadata {
    pub first_seen: u64,
    pub touched: u64,
    /// Physical row count/charge, preserved by future logical retirement until GC.
    pub retained: ConversationMetadata,
}
impl MailboxMetadata {
    pub(crate) fn expiry_deadline(&self) -> Option<u64> {
        self.first_seen
            .checked_add(super::super::MAX_RELAY_AGE.as_secs())
    }
}

pub(crate) struct IndexedRelayState {
    profile: Arc<ProfileStorage>,
    repository: EventRepository,
    admission: RelayAdmission,
    changes: tokio::sync::watch::Sender<u64>,
}
impl IndexedRelayState {
    /// Takes the existing shared profile; never opens another connection/KDF/cache.
    /// This internal foundation deliberately implements no serving/sync trait.
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        admission: RelayAdmission,
    ) -> Result<Self, LogError> {
        validate(profile.database())?;
        profile.database().read(|c| require_ready(c).map(|_| ()))?;
        let repository = profile.events()?;
        Ok(Self {
            profile,
            repository,
            admission,
            changes: tokio::sync::watch::channel(0).0,
        })
    }
    pub(crate) fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }
    pub(crate) fn physical_charge(&self) -> Result<DomainChargeMetadata, LogError> {
        self.profile.database().read(|c| {
            require_ready(c)?;
            self.repository.domain_charge_on(c)
        })
    }
    pub(crate) fn mailbox_metadata(
        &self,
        conversation: &ConversationId,
    ) -> Result<Option<MailboxMetadata>, LogError> {
        self.profile.database().read(|c| {
            require_ready(c)?;
            self.mailbox_on(c, conversation)
        })
    }
    fn mailbox_on(
        &self,
        c: &Connection,
        conversation: &ConversationId,
    ) -> Result<Option<MailboxMetadata>, LogError> {
        let arrival: Option<(Vec<u8>,Vec<u8>)> = c.query_row(
            "SELECT first_seen,touched FROM relay_mailboxes WHERE domain=?1 AND conversation=?2",
            params![RELAY_ARRIVALS.as_slice(),conversation.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let retained = self
            .repository
            .metadata_on(c, conversation)?
            .filter(|m| m.count > 0);
        match (arrival, retained) {
            (None, None) => Ok(None),
            (Some((first_seen, touched)), Some(retained)) => Ok(Some(MailboxMetadata {
                first_seen: decode_time(first_seen)?,
                touched: decode_time(touched)?,
                retained,
            })),
            _ => Err(invalid("relay arrival and retained mailbox disagree")),
        }
    }
    /// Exact physically retained row, revalidated on the shared current connection.
    /// This does NOT assert expiry-safe positive custody; no runtime may serve it yet.
    pub(crate) fn retained_event(&self, id: &EventId) -> Result<Option<Event>, LogError> {
        self.profile.database().read(|c| {
            require_ready(c)?;
            let event = self.repository.get_on(c, id)?;
            if let Some(event) = &event {
                self.mailbox_on(c, &event.conversation_id)?
                    .ok_or_else(|| invalid("retained relay event lacks arrival"))?;
            }
            Ok(event)
        })
    }
    /// `origin` is a session hint from an authenticated trusted federation peer.
    /// Unknown inventory alone has no durable-write operation. Validate again at
    /// actual acceptance; an earlier origin can correct a signed duplicate.
    pub(crate) fn record(
        &self,
        event: &Event,
        now: u64,
        origin: Option<u64>,
    ) -> Result<AppendOutcome, LogError> {
        let (outcome, changed) = self.profile.database().transaction(|c| {
            let next_touch = require_ready(c)?;
            if matches!(self.admission, RelayAdmission::OpaqueOnly)
                && !super::super::opaque::envelope_shape(event)
            {
                return Err(LogError::AdmissionRejected(
                    "post office requires an opaque envelope".into(),
                ));
            }
            if let Some(seen) = origin {
                let expired = seen.checked_add(super::super::MAX_RELAY_AGE.as_secs())
                    .is_some_and(|deadline| deadline <= now);
                let future = now.checked_add(300).is_some_and(|latest| seen > latest);
                if !super::super::opaque::RelayMailbox::is_federated_id(&event.conversation_id)
                    || expired || future
                {
                    return Err(LogError::AdmissionRejected(
                        "federated mailbox arrival is expired or invalid".into(),
                    ));
                }
            }
            let prior = self.mailbox_on(c, &event.conversation_id)?;
            // Repository owns every validation rule, including signed duplicates.
            let outcome = self.repository.append_on(c, event)?;
            fault_cut(c, "event")?;
            let first = origin.unwrap_or(now).min(now);
            let lowered = prior.is_some_and(|m| origin.is_some() && first < m.first_seen);
            let appended = matches!(outcome, AppendOutcome::Appended);
            let mailbox = event.conversation_id.as_bytes().as_slice();
            if appended {
                let following = next_touch.checked_add(1)
                    .ok_or(LogError::RepositoryCounterExhausted)?;
                if prior.is_none() {
                    c.execute(
                        "INSERT INTO relay_mailboxes VALUES(?1,?2,?3,?4)",
                        params![RELAY_ARRIVALS.as_slice(), mailbox,
                            first.to_be_bytes().as_slice(), next_touch.to_be_bytes().as_slice()],
                    )?;
                } else if lowered {
                    c.execute(
                        "UPDATE relay_mailboxes SET first_seen=?3 WHERE domain=?1 AND conversation=?2",
                        params![RELAY_ARRIVALS.as_slice(), mailbox, first.to_be_bytes().as_slice()],
                    )?;
                }
                fault_cut(c, "arrival")?;
                c.execute(
                    "UPDATE relay_mailboxes SET touched=?3 WHERE domain=?1 AND conversation=?2",
                    params![RELAY_ARRIVALS.as_slice(), mailbox, next_touch.to_be_bytes().as_slice()],
                )?;
                fault_cut(c, "touch")?;
                c.execute(
                    "UPDATE relay_runtime SET next_touch=?1 WHERE singleton=1",
                    [following.to_be_bytes().as_slice()],
                )?;
            } else if lowered {
                c.execute(
                    "UPDATE relay_mailboxes SET first_seen=?3 WHERE domain=?1 AND conversation=?2",
                    params![RELAY_ARRIVALS.as_slice(), mailbox, first.to_be_bytes().as_slice()],
                )?;
            }
            fault_cut(c, "runtime")?;
            fault_cut(c, "commit")?;
            Ok((outcome, appended || lowered))
        })?;
        if changed {
            self.changes
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        Ok(outcome)
    }
}
fn decode_time(bytes: Vec<u8>) -> Result<u64, LogError> {
    Ok(u64::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| invalid("invalid relay timestamp"))?,
    ))
}
fn fault_cut(_c: &Connection, _point: &str) -> Result<(), LogError> {
    #[cfg(test)]
    if super::role_tests::CUT.with(|v| v.get() == Some(_point)) {
        if _point == "commit" {
            // A genuine deferred constraint fails COMMIT, after the closure has
            // succeeded. rusqlite rolls back on drop; the database owner faults.
            _c.execute_batch("CREATE TEMP TABLE relay_fault_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE relay_fault_child(id INTEGER REFERENCES relay_fault_parent(id) DEFERRABLE INITIALLY DEFERRED); INSERT INTO relay_fault_child VALUES(1);")?;
        } else {
            return Err(invalid("injected relay transaction cut"));
        }
    }
    Ok(())
}
