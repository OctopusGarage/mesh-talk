//! Encrypted, conservatively charged federation staging on the profile connection.
use super::*;
pub(super) const SCHEMA: &str = "
CREATE TABLE relay_federation_runtime(singleton INTEGER PRIMARY KEY CHECK(singleton=1), owner_epoch INTEGER NOT NULL DEFAULT 0 CHECK(typeof(owner_epoch)='integer' AND owner_epoch>=0), next_session INTEGER NOT NULL DEFAULT 1 CHECK(typeof(next_session)='integer' AND next_session>0), session_rows INTEGER NOT NULL DEFAULT 0 CHECK(typeof(session_rows)='integer' AND session_rows BETWEEN 0 AND 512), staged_rows INTEGER NOT NULL DEFAULT 0 CHECK(typeof(staged_rows)='integer' AND staged_rows BETWEEN 0 AND 262144), staged_bytes INTEGER NOT NULL DEFAULT 0 CHECK(typeof(staged_bytes)='integer' AND staged_bytes BETWEEN 0 AND 67108864), live_inbound INTEGER NOT NULL DEFAULT 0 CHECK(typeof(live_inbound)='integer' AND live_inbound BETWEEN 0 AND 256), live_outbound INTEGER NOT NULL DEFAULT 0 CHECK(typeof(live_outbound)='integer' AND live_outbound BETWEEN 0 AND 8));
CREATE TABLE relay_federation_sessions(session_id INTEGER PRIMARY KEY CHECK(session_id>0), owner_epoch INTEGER NOT NULL CHECK(typeof(owner_epoch)='integer' AND owner_epoch>0), session_epoch INTEGER NOT NULL DEFAULT 1 CHECK(typeof(session_epoch)='integer' AND session_epoch>0), live INTEGER NOT NULL DEFAULT 1 CHECK(typeof(live)='integer' AND live IN (0,1)), direction INTEGER NOT NULL CHECK(typeof(direction)='integer' AND direction IN (0,1)), deadline BLOB NOT NULL CHECK(typeof(deadline)='blob' AND length(deadline)=8), row_charge INTEGER NOT NULL DEFAULT 0 CHECK(typeof(row_charge)='integer' AND row_charge BETWEEN 0 AND 262144), byte_charge INTEGER NOT NULL DEFAULT 1024 CHECK(typeof(byte_charge)='integer' AND byte_charge>=1024), complete INTEGER NOT NULL DEFAULT 0 CHECK(typeof(complete)='integer' AND complete IN (0,1)), receive_pages INTEGER NOT NULL DEFAULT 0 CHECK(typeof(receive_pages)='integer' AND receive_pages BETWEEN 0 AND 256), receive_entries INTEGER NOT NULL DEFAULT 0 CHECK(typeof(receive_entries)='integer' AND receive_entries BETWEEN 0 AND 131072), local_complete INTEGER NOT NULL DEFAULT 0 CHECK(typeof(local_complete)='integer' AND local_complete IN (0,1)), local_entries INTEGER NOT NULL DEFAULT 0 CHECK(typeof(local_entries)='integer' AND local_entries BETWEEN 0 AND 131072), local_framed_entries INTEGER NOT NULL DEFAULT 0 CHECK(typeof(local_framed_entries)='integer' AND local_framed_entries BETWEEN 0 AND 131072), local_pages INTEGER NOT NULL DEFAULT 0 CHECK(typeof(local_pages)='integer' AND local_pages BETWEEN 0 AND 256), scan_phase INTEGER NOT NULL DEFAULT 0 CHECK(typeof(scan_phase)='integer' AND scan_phase BETWEEN 0 AND 3), scan_pivot BLOB CHECK(scan_pivot IS NULL OR (typeof(scan_pivot)='blob' AND length(scan_pivot)=32)), local_after BLOB CHECK(local_after IS NULL OR (typeof(local_after)='blob' AND length(local_after)=32)), remote_after BLOB CHECK(remote_after IS NULL OR (typeof(remote_after)='blob' AND length(remote_after)=32)));
CREATE INDEX relay_federation_sessions_owner ON relay_federation_sessions(owner_epoch,session_id);
CREATE INDEX relay_federation_sessions_deadline ON relay_federation_sessions(live,deadline,session_id);
CREATE TABLE relay_federation_entries(session_id INTEGER NOT NULL REFERENCES relay_federation_sessions(session_id), conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32), digest BLOB NOT NULL CHECK(typeof(digest)='blob' AND length(digest)=32), first_seen BLOB NOT NULL CHECK(typeof(first_seen)='blob' AND length(first_seen)=8), PRIMARY KEY(session_id,conversation)) WITHOUT ROWID;
CREATE TABLE relay_federation_local_entries(session_id INTEGER NOT NULL REFERENCES relay_federation_sessions(session_id), conversation BLOB NOT NULL CHECK(typeof(conversation)='blob' AND length(conversation)=32), digest BLOB NOT NULL CHECK(typeof(digest)='blob' AND length(digest)=32), first_seen BLOB NOT NULL CHECK(typeof(first_seen)='blob' AND length(first_seen)=8), PRIMARY KEY(session_id,conversation)) WITHOUT ROWID;
";

pub(super) fn validate_runtime_on(c: &Connection) -> Result<(), LogError> {
    let row:Option<(i64,i64,i64,i64,i64,i64,i64)>=c.query_row("SELECT owner_epoch,next_session,session_rows,staged_rows,staged_bytes,live_inbound,live_outbound FROM relay_federation_runtime WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?))).optional()?;
    let Some((owner, next, sessions, rows, bytes, inbound, outbound)) = row else {
        return Err(invalid("missing federation resource authority"));
    };
    if owner < 0
        || next <= 0
        || !(0..=MAX_SESSIONS).contains(&sessions)
        || !(0..=MAX_ROWS).contains(&rows)
        || !(0..=MAX_BYTES).contains(&bytes)
        || !(0..=256).contains(&inbound)
        || !(0..=8).contains(&outbound)
        || inbound + outbound > sessions
        || bytes != sessions * SESSION_BYTES + rows * ENTRY_BYTES
    {
        return Err(invalid("invalid federation resource authority"));
    }
    Ok(())
}

use crate::{
    eventlog::ConversationId,
    postoffice::{FederatedMailbox, MAX_RELAY_AGE},
    storage::profile::context::ProfileStorage,
};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
const SESSION_BYTES: i64 = 1024;
const ENTRY_BYTES: i64 = 192;
const MAX_BYTES: i64 = 64 * 1024 * 1024;
const MAX_ROWS: i64 = 262144;
const MAX_SESSIONS: i64 = 512;
const PAGE_SIZE: usize = 512;
const MAX_PAGES: i64 = 256;
const SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Once per actual profile owner start, never per facade/session clone. Old rows
/// keep their charges and become unusable without scanning their entries.
pub(crate) fn start_owner(db: &EncryptedDatabase) -> Result<i64, LogError> {
    db.transaction(|c| {
        super::require_ready(c)?;
        let old: i64=c.query_row("SELECT owner_epoch FROM relay_federation_runtime WHERE singleton=1", [], |r|r.get(0))?;
        let epoch=old.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?;
        c.execute("UPDATE relay_federation_runtime SET owner_epoch=?1,live_inbound=0,live_outbound=0 WHERE singleton=1", [epoch])?;
        Ok(epoch)
    })
}

#[derive(Clone, Copy)]
pub(crate) enum Direction {
    Inbound,
    Outbound,
}
impl Direction {
    fn code(self) -> i64 {
        match self {
            Self::Inbound => 0,
            Self::Outbound => 1,
        }
    }
}
struct SessionAuthority {
    live: AtomicBool,
    deadline: Instant,
}
/// The only authority that can keep a session alive. Leases cannot prolong it.
pub(crate) struct SessionOwner {
    lease: SessionLease,
}
#[derive(Clone)]
pub(crate) struct SessionLease {
    profile: Arc<ProfileStorage>,
    authority: Arc<SessionAuthority>,
    owner_epoch: i64,
    session_id: i64,
    session_epoch: i64,
}
impl Drop for SessionOwner {
    fn drop(&mut self) {
        self.lease.authority.live.store(false, Ordering::Release);
        self.lease
            .profile
            .federation_cancellations()
            .push((self.lease.session_id, self.lease.owner_epoch));
    }
}
impl SessionOwner {
    pub(crate) fn begin(
        profile: Arc<ProfileStorage>,
        direction: Direction,
        now: u64,
        started: Instant,
    ) -> Result<Self, LogError> {
        let owner_epoch = profile.federation_owner_epoch()?;
        let deadline = started
            .checked_add(SESSION_TIMEOUT)
            .ok_or(LogError::ClockExhausted)?;
        let utc_deadline = now
            .checked_add(SESSION_TIMEOUT.as_secs())
            .ok_or(LogError::ClockExhausted)?;
        let authority = Arc::new(SessionAuthority {
            live: AtomicBool::new(true),
            deadline,
        });
        let mut reserved = None;
        let session_id=profile.database().transaction(|c| {
            super::require_ready(c)?;
            let (actual,next,rows,bytes,inbound,outbound):(i64,i64,i64,i64,i64,i64)=c.prepare_cached("SELECT owner_epoch,next_session,session_rows,staged_bytes,live_inbound,live_outbound FROM relay_federation_runtime WHERE singleton=1")?.query_row( [], |r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
            if actual!=owner_epoch { return Err(LogError::StaleCursor); }
            let following=next.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?;
            let rows=rows.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?;
            let bytes=bytes.checked_add(SESSION_BYTES).ok_or(LogError::RepositoryCounterExhausted)?;
            let (inbound,outbound)=match direction { Direction::Inbound=>(inbound.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?,outbound),Direction::Outbound=>(inbound,outbound.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?) };
            if rows>MAX_SESSIONS || bytes>MAX_BYTES || inbound>256 || outbound>8 { return Err(LogError::RelayCapacityUnavailable); }
            c.execute("INSERT INTO relay_federation_sessions(session_id,owner_epoch,direction,deadline) VALUES(?1,?2,?3,?4)",params![next,owner_epoch,direction.code(),utc_deadline.to_be_bytes().as_slice()])?;
            c.execute("UPDATE relay_federation_runtime SET next_session=?1,session_rows=?2,staged_bytes=?3,live_inbound=?4,live_outbound=?5 WHERE singleton=1",params![following,rows,bytes,inbound,outbound])?;
            #[cfg(test)]
            super::role::fault_cut(c,"commit")?;
            profile.federation_cancellations().register(next,owner_epoch,&authority)?;
            reserved=Some((next,owner_epoch));
            Ok(next)
        });
        let session_id = match session_id {
            Ok(id) => id,
            Err(error) => {
                if let Some(key) = reserved {
                    profile.federation_cancellations().retire(key);
                }
                return Err(error);
            }
        };
        profile.federation_cancellations().notify();
        Ok(Self {
            lease: SessionLease {
                profile,
                authority,
                owner_epoch,
                session_id,
                session_epoch: 1,
            },
        })
    }
    pub(crate) fn lease(&self) -> SessionLease {
        self.lease.clone()
    }
    /// Synchronous authority revocation precedes fallible persisted invalidation.
    #[cfg(test)]
    pub(crate) fn cancel(&self) -> Result<(), LogError> {
        self.lease.authority.live.store(false, Ordering::Release);
        self.lease.profile.federation_cancellations().notify();
        self.lease
            .profile
            .database()
            .transaction(|c| invalidate_on(c, self.lease.session_id, self.lease.owner_epoch))
    }
}
impl SessionLease {
    fn check_live(&self) -> Result<(), LogError> {
        if !self.authority.live.load(Ordering::Acquire) || Instant::now() >= self.authority.deadline
        {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    pub(crate) fn validate_on(&self, c: &Connection, complete: bool) -> Result<(), LogError> {
        self.check_live()?;
        let state:Option<(i64,i64,i64,i64,i64)>=c.prepare_cached("SELECT r.owner_epoch,s.owner_epoch,s.session_epoch,s.live,s.complete FROM relay_federation_runtime r JOIN relay_federation_sessions s ON s.session_id=?1 WHERE r.singleton=1")?.query_row([self.session_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        if state.is_none_or(|(runtime, owner, epoch, live, done)| {
            runtime != self.owner_epoch
                || owner != self.owner_epoch
                || epoch != self.session_epoch
                || live != 1
                || (complete && done != 1)
        }) {
            return Err(LogError::StaleCursor);
        }
        self.check_live()
    }
    /// Preserve authoritative failures. Expected local availability outcomes can
    /// commit policy work, so authority loss must convert those into rollback.
    pub(crate) fn finish_on<T>(
        &self,
        c: &Connection,
        complete: bool,
        result: Result<T, LogError>,
    ) -> Result<T, LogError> {
        if matches!(
            &result,
            Ok(_)
                | Err(LogError::RelayMailboxUnavailable
                    | LogError::ConversationRetiring
                    | LogError::StaleCursor
                    | LogError::RelayCapacityUnavailable
                    | LogError::RelayEventTooLarge)
        ) {
            self.validate_on(c, complete).map_err(|error| match error {
                LogError::StaleCursor => LogError::AdmissionRejected(
                    "federation session lost authority during operation".into(),
                ),
                error => error,
            })?;
        }
        result
    }
    pub(crate) fn same_session(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.authority, &other.authority) && Arc::ptr_eq(&self.profile, &other.profile)
    }
    pub(crate) fn belongs_to(&self, profile: &Arc<ProfileStorage>) -> bool {
        Arc::ptr_eq(&self.profile, profile)
    }
    pub(crate) fn origin_on(
        &self,
        c: &Connection,
        conversation: &ConversationId,
        now: u64,
    ) -> Result<Option<u64>, LogError> {
        self.validate_on(c, true)?;
        let origin:Option<Vec<u8>>=c.prepare_cached("SELECT first_seen FROM relay_federation_entries WHERE session_id=?1 AND conversation=?2")?.query_row(params![self.session_id,conversation.as_bytes().as_slice()],|r|r.get(0)).optional()?;
        let origin = origin.map(super::role::decode_time).transpose()?;
        if let Some(first) = origin {
            validate_origin(conversation, first, now)?;
        }
        Ok(origin)
    }
    #[cfg(test)]
    fn origin(&self, conversation: &ConversationId, now: u64) -> Result<Option<u64>, LogError> {
        self.profile
            .database()
            .read(|c| self.origin_on(c, conversation, now))
    }
    /// Every raw received entry counts against the per-exchange ceiling, even
    /// when an indexed duplicate consumes no extra durable row reservation.
    pub(crate) fn receive_page(
        &self,
        entries: &[FederatedMailbox],
        last: bool,
        now: u64,
    ) -> Result<(), LogError> {
        if entries.len() > PAGE_SIZE {
            return Err(LogError::InvalidPageBudget);
        }
        self.profile.database().transaction(|c| {
            self.validate_on(c,false)?;
            let (pages,received,complete,rows,bytes):(i64,i64,i64,i64,i64)=c.prepare_cached("SELECT receive_pages,receive_entries,complete,row_charge,byte_charge FROM relay_federation_sessions WHERE session_id=?1")?.query_row([self.session_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
            let pages=pages.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?;
            let received=received.checked_add(i64::try_from(entries.len()).map_err(|_|LogError::InvalidPageBudget)?).ok_or(LogError::RepositoryCounterExhausted)?;
            if complete!=0 || pages>MAX_PAGES || received>PAGE_SIZE as i64*MAX_PAGES || (!last && pages==MAX_PAGES) { return Err(LogError::AdmissionRejected("incomplete or oversized federation inventory".into())); }
            // One bounded page deduplicates before any reservation or mutation.
            let mut unique=std::collections::BTreeMap::new();
            for entry in entries {
                validate_origin(&entry.id,entry.first_seen_secs,now)?;
                match unique.entry(*entry.id.as_bytes()) {
                    std::collections::btree_map::Entry::Vacant(slot)=>{slot.insert((entry.digest,entry.first_seen_secs));},
                    std::collections::btree_map::Entry::Occupied(mut slot)=>{
                        if slot.get().0!=entry.digest { return Err(LogError::AdmissionRejected("conflicting federation digest".into())); }
                        slot.get_mut().1=slot.get().1.min(entry.first_seen_secs);
                    }
                }
            }
            let mut additional=0i64;
            if !unique.is_empty() {
                let mut prior=c.prepare("SELECT digest FROM relay_federation_entries WHERE session_id=?1 AND conversation=?2")?;
                for (id,(digest,_)) in &unique {
                    let existing:Option<Vec<u8>>=prior.query_row(params![self.session_id,id.as_slice()],|r|r.get(0)).optional()?;
                    match existing { Some(existing) if existing.as_slice()!=digest=>return Err(LogError::AdmissionRejected("conflicting federation digest".into())),None=>additional+=1,_=>{} }
                }
            }
            let cost=additional.checked_mul(ENTRY_BYTES).ok_or(LogError::RepositoryCounterExhausted)?;
            let (global_rows,global_bytes):(i64,i64)=c.prepare_cached("SELECT staged_rows,staged_bytes FROM relay_federation_runtime WHERE singleton=1")?.query_row([],|r|Ok((r.get(0)?,r.get(1)?)))?;
            let global_rows=global_rows.checked_add(additional).ok_or(LogError::RepositoryCounterExhausted)?;
            let global_bytes=global_bytes.checked_add(cost).ok_or(LogError::RepositoryCounterExhausted)?;
            let rows=rows.checked_add(additional).ok_or(LogError::RepositoryCounterExhausted)?;
            let bytes=bytes.checked_add(cost).ok_or(LogError::RepositoryCounterExhausted)?;
            if global_rows>MAX_ROWS || global_bytes>MAX_BYTES { return Err(LogError::RelayCapacityUnavailable); }
            if !unique.is_empty() {
                let mut insert=c.prepare("INSERT INTO relay_federation_entries(session_id,conversation,digest,first_seen) VALUES(?1,?2,?3,?4) ON CONFLICT(session_id,conversation) DO UPDATE SET first_seen=min(first_seen,excluded.first_seen)")?;
                for (id,(digest,first)) in unique {
                    insert.execute(params![self.session_id,id.as_slice(),digest.as_slice(),first.to_be_bytes().as_slice()])?;
                }
            }
            c.execute("UPDATE relay_federation_sessions SET receive_pages=?2,receive_entries=?3,complete=?4,row_charge=?5,byte_charge=?6 WHERE session_id=?1",params![self.session_id,pages,received,i64::from(last),rows,bytes])?;
            c.execute("UPDATE relay_federation_runtime SET staged_rows=?1,staged_bytes=?2 WHERE singleton=1",params![global_rows,global_bytes])?;
            self.validate_on(c,false)
        })
    }
}
fn validate_origin(conversation: &ConversationId, first: u64, now: u64) -> Result<(), LogError> {
    if !crate::postoffice::opaque::RelayMailbox::is_federated_id(conversation)
        || first
            .checked_add(MAX_RELAY_AGE.as_secs())
            .is_some_and(|deadline| deadline <= now)
        || now.checked_add(300).is_some_and(|latest| first > latest)
    {
        return Err(LogError::AdmissionRejected(
            "federated mailbox arrival is expired or invalid".into(),
        ));
    }
    Ok(())
}
fn invalidate_on(c: &Connection, id: i64, owner_epoch: i64) -> Result<(), LogError> {
    let row:Option<(i64,i64,i64)>=c.prepare_cached("SELECT live,direction,session_epoch FROM relay_federation_sessions WHERE session_id=?1 AND owner_epoch=?2")?.query_row(params![id,owner_epoch],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((1, direction, epoch)) = row {
        let epoch = epoch
            .checked_add(1)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        c.execute(
            "UPDATE relay_federation_sessions SET live=0,session_epoch=?2 WHERE session_id=?1",
            params![id, epoch],
        )?;
        let current: i64 = c
            .prepare_cached("SELECT owner_epoch FROM relay_federation_runtime WHERE singleton=1")?
            .query_row([], |r| r.get(0))?;
        if current == owner_epoch {
            let sql = if direction == 0 {
                "UPDATE relay_federation_runtime SET live_inbound=live_inbound-1 WHERE singleton=1"
            } else {
                "UPDATE relay_federation_runtime SET live_outbound=live_outbound-1 WHERE singleton=1"
            };
            c.execute(sql, [])?;
        }
    }
    Ok(())
}
/// One indexed stale-session selection and <=512 indexed entry deletions.
/// Accounted bytes are logical reservations, never a claim about file/WAL size.
pub(crate) fn cleanup_step(profile: &ProfileStorage, now: u64) -> Result<bool, LogError> {
    let hint = profile
        .federation_cancellations()
        .reclaim_candidate()
        .or_else(|| profile.federation_cancellations().peek());
    let (progress, consumed)=profile.database().transaction(|c| {
        let hinted=if let Some((id,owner))=hint { c.prepare_cached("SELECT session_id,owner_epoch FROM relay_federation_sessions WHERE session_id=?1 AND owner_epoch=?2")?.query_row(params![id,owner],|r|Ok((r.get(0)?,r.get(1)?))).optional()? } else {None};
        if hint.is_some() && hinted.is_none() { return Ok((true,hint)); }
        let epoch=profile.federation_owner_epoch()?;
        let old:Option<(i64,i64)>=c.prepare_cached("SELECT session_id,owner_epoch FROM relay_federation_sessions INDEXED BY relay_federation_sessions_owner WHERE owner_epoch<?1 ORDER BY owner_epoch,session_id LIMIT 1")?.query_row([epoch],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let selected=if hinted.is_some(){hinted}else if old.is_some(){old}else{
            let cancelled=c.prepare_cached("SELECT session_id,owner_epoch FROM relay_federation_sessions INDEXED BY relay_federation_sessions_deadline WHERE live=0 ORDER BY deadline,session_id LIMIT 1")?.query_row([],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if cancelled.is_some(){cancelled}else{c.prepare_cached("SELECT session_id,owner_epoch FROM relay_federation_sessions INDEXED BY relay_federation_sessions_deadline WHERE live=1 AND deadline<=?1 ORDER BY deadline,session_id LIMIT 1")?.query_row([now.to_be_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?}
        };
        let Some((id,owner))=selected else{return Ok((false,None))};
        invalidate_on(c,id,owner)?;
        let keys={let mut stmt=c.prepare("SELECT conversation FROM relay_federation_entries WHERE session_id=?1 ORDER BY conversation LIMIT 512")?;stmt.query_map([id],|r|r.get::<_,Vec<u8>>(0))?.collect::<Result<Vec<_>,_>>()?};
        if !keys.is_empty() {
            let mut delete=c.prepare("DELETE FROM relay_federation_entries WHERE session_id=?1 AND conversation=?2")?;
            for key in &keys {delete.execute(params![id,key])?;}
        }
        let local_keys={let mut stmt=c.prepare("SELECT conversation FROM relay_federation_local_entries WHERE session_id=?1 ORDER BY conversation LIMIT ?2")?;stmt.query_map(params![id,(PAGE_SIZE-keys.len()) as i64],|r|r.get::<_,Vec<u8>>(0))?.collect::<Result<Vec<_>,_>>()?};
        if !local_keys.is_empty() {
            let mut delete=c.prepare("DELETE FROM relay_federation_local_entries WHERE session_id=?1 AND conversation=?2")?;
            for key in &local_keys {delete.execute(params![id,key])?;}
        }
        let selected=keys.len()+local_keys.len();
        let rows=i64::try_from(selected).map_err(|_|LogError::RepositoryCounterExhausted)?;
        let bytes=rows.checked_mul(ENTRY_BYTES).ok_or(LogError::RepositoryCounterExhausted)?;
        c.execute("UPDATE relay_federation_sessions SET row_charge=row_charge-?2,byte_charge=byte_charge-?3 WHERE session_id=?1",params![id,rows,bytes])?;
        c.execute("UPDATE relay_federation_runtime SET staged_rows=staged_rows-?1,staged_bytes=staged_bytes-?2 WHERE singleton=1",params![rows,bytes])?;
        if selected<PAGE_SIZE {
            let charge:(i64,i64)=c.prepare_cached("SELECT row_charge,byte_charge FROM relay_federation_sessions WHERE session_id=?1")?.query_row([id],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if charge!=(0,SESSION_BYTES){return Err(invalid("federation staging charge differs from retained rows"))}
            c.execute("DELETE FROM relay_federation_sessions WHERE session_id=?1",[id])?;
            c.execute("UPDATE relay_federation_runtime SET session_rows=session_rows-1,staged_bytes=staged_bytes-?1 WHERE singleton=1",[SESSION_BYTES])?;
        }
        Ok((true,if selected<PAGE_SIZE {Some((id,owner))} else {None}))
    })?;
    if let Some(key) = consumed {
        profile.federation_cancellations().retire(key);
        profile.federation_cancellations().pop_if(Some(key));
    }
    Ok(progress)
}

/// Bounded cancellation scheduling hints. Drop never enters SQL or spawns work.
/// Persisted deadline/owner indexes remain recovery authority after a crash.
struct RegisteredSession {
    key: (i64, i64),
    deadline: Instant,
    authority: std::sync::Weak<SessionAuthority>,
}
pub(crate) struct CancellationQueue {
    hints: std::sync::Mutex<std::collections::VecDeque<(i64, i64)>>,
    registry: std::sync::Mutex<Vec<RegisteredSession>>,
    changes: tokio::sync::watch::Sender<u64>,
}
impl Default for CancellationQueue {
    fn default() -> Self {
        Self {
            hints: std::sync::Mutex::new(std::collections::VecDeque::with_capacity(
                MAX_SESSIONS as usize,
            )),
            registry: std::sync::Mutex::new(Vec::with_capacity(MAX_SESSIONS as usize)),
            changes: tokio::sync::watch::channel(0).0,
        }
    }
}
impl CancellationQueue {
    fn register(
        &self,
        id: i64,
        owner: i64,
        authority: &Arc<SessionAuthority>,
    ) -> Result<(), LogError> {
        let mut registry = self.registry.lock().unwrap_or_else(|e| e.into_inner());
        if registry.len() >= MAX_SESSIONS as usize {
            return Err(LogError::RelayCapacityUnavailable);
        }
        registry.push(RegisteredSession {
            key: (id, owner),
            deadline: authority.deadline,
            authority: Arc::downgrade(authority),
        });
        Ok(())
    }
    fn retire(&self, key: (i64, i64)) {
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|entry| entry.key != key);
    }
    /// Examine the whole fixed <=512 authority registry, so healthy entries
    /// cannot starve a canceled/expired entry behind them. No SQL lock is held.
    fn reclaim_candidate(&self) -> Option<(i64, i64)> {
        let now = Instant::now();
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .find(|entry| {
                now >= entry.deadline
                    || entry
                        .authority
                        .upgrade()
                        .is_none_or(|a| !a.live.load(Ordering::Acquire))
            })
            .map(|entry| entry.key)
    }
    pub(crate) fn next_deadline(&self) -> Option<Instant> {
        let now = Instant::now();
        self.registry
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|entry| {
                if entry
                    .authority
                    .upgrade()
                    .is_none_or(|a| !a.live.load(Ordering::Acquire))
                {
                    now
                } else {
                    entry.deadline
                }
            })
            .min()
    }
    pub(crate) fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.changes.subscribe()
    }
    fn notify(&self) {
        self.changes
            .send_modify(|generation| *generation = generation.wrapping_add(1));
    }
    fn push(&self, key: (i64, i64)) {
        {
            let mut q = self.hints.lock().unwrap_or_else(|e| e.into_inner());
            // A full scheduling queue never prolongs authority or frees charge.
            // The bounded registry selects cancellation/Instant expiry directly;
            // persisted owner/deadline indexes recover after process restart.
            if q.len() < MAX_SESSIONS as usize {
                q.push_back(key);
            }
        }
        self.notify();
    }
    fn peek(&self) -> Option<(i64, i64)> {
        self.hints
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .front()
            .copied()
    }
    fn pop_if(&self, key: Option<(i64, i64)>) {
        let mut q = self.hints.lock().unwrap_or_else(|e| e.into_inner());
        if q.front().copied() == key {
            q.pop_front();
        }
    }
}

pub(crate) struct OriginCursor {
    session_id: i64,
    owner_epoch: i64,
    after: ConversationId,
}
pub(crate) struct OriginsPage {
    pub entries: Vec<(ConversationId, u64)>,
    pub after: Option<OriginCursor>,
}
impl SessionLease {
    pub(crate) fn origins_page_on(
        &self,
        c: &Connection,
        after: Option<&OriginCursor>,
        examined: usize,
        now: u64,
    ) -> Result<OriginsPage, LogError> {
        if examined == 0 || examined > PAGE_SIZE {
            return Err(LogError::InvalidPageBudget);
        }
        self.validate_on(c, true)?;
        if after
            .is_some_and(|v| v.session_id != self.session_id || v.owner_epoch != self.owner_epoch)
        {
            return Err(LogError::StaleCursor);
        }
        let key = after.map(|v| v.after.as_bytes().as_slice()).unwrap_or(&[]);
        let mut stmt=c.prepare("SELECT conversation,first_seen FROM relay_federation_entries WHERE session_id=?1 AND conversation>?2 ORDER BY conversation LIMIT ?3")?;
        let mut rows = stmt.query(params![self.session_id, key, examined as i64])?;
        let mut entries = Vec::with_capacity(examined);
        while let Some(row) = rows.next()? {
            let key: Vec<u8> = row.get(0)?;
            let id = ConversationId::new(
                key.try_into()
                    .map_err(|_| invalid("invalid staged federation key"))?,
            );
            let first = super::role::decode_time(row.get(1)?)?;
            validate_origin(&id, first, now)?;
            entries.push((id, first));
        }
        let after = if entries.len() == examined {
            entries.last().map(|(id, _)| OriginCursor {
                session_id: self.session_id,
                owner_epoch: self.owner_epoch,
                after: *id,
            })
        } else {
            None
        };
        Ok(OriginsPage { entries, after })
    }
}

#[cfg(test)]
mod tests;

pub(crate) mod local;
