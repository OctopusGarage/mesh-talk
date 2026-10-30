//! File receive discovery and fair retries. Signed bodies remain in the event
//! repository; committed file evidence is the sole completion authority.
use super::indexed::{IndexedFileBook, blob, invalid};
use crate::eventlog::{
    Author, ConversationId, Event, EventId, EventKind, LogError, repository::CommittedHeader,
};
use crate::storage::profile::participant::event_ownership::{self, ParticipantEventOwner};
use rusqlite::{Connection, params};

const SCHEMA: &str = "
CREATE TABLE file_receive_scan(account BLOB PRIMARY KEY CHECK(length(account)=32),after_commit INTEGER CHECK(after_commit IS NULL OR after_commit>=0),next_ticket INTEGER NOT NULL CHECK(typeof(next_ticket)='integer' AND next_ticket>0)) WITHOUT ROWID;
CREATE TABLE file_receive_pending(account BLOB NOT NULL CHECK(length(account)=32),event BLOB NOT NULL CHECK(length(event)=32),conversation BLOB NOT NULL CHECK(length(conversation)=32),bytes INTEGER NOT NULL CHECK(bytes>=0),ticket INTEGER NOT NULL CHECK(typeof(ticket)='integer' AND ticket>0),PRIMARY KEY(account,event)) WITHOUT ROWID;
CREATE INDEX file_receive_turn ON file_receive_pending(account,ticket,event);
CREATE INDEX file_receive_conversation_turn ON file_receive_pending(account,conversation,ticket,event);
";

pub(super) fn initialize_on(c: &Connection, account: &[u8; 32]) -> Result<(), LogError> {
    let exists: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_receive_scan')",
        [],
        |r| r.get(0),
    )?;
    if !exists {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    c.execute(
        "INSERT OR IGNORE INTO file_receive_scan VALUES(?1,NULL,1)",
        [account.as_slice()],
    )?;
    Ok(())
}
impl IndexedFileBook {
    fn check_receive_owner(&self, c: &Connection) -> Result<&ParticipantEventOwner, LogError> {
        let owner = self.receive_owner.as_ref().ok_or(LogError::StaleCursor)?;
        event_ownership::check_event_owner_on(c, owner)?;
        Ok(owner)
    }

    pub(crate) fn receive_after(&self) -> Result<Option<i64>, LogError> {
        self.profile.database().read(|c| {
            self.check_receive_owner(c)?;
            Ok(c.query_row(
                "SELECT after_commit FROM file_receive_scan WHERE account=?1",
                [self.account.as_slice()],
                |r| r.get(0),
            )?)
        })
    }
    pub(crate) fn discover_receives(
        &self,
        after: Option<i64>,
        rows: &[CommittedHeader],
        own: Author,
    ) -> Result<(), LogError> {
        if rows.len() > 128
            || after.is_some_and(|n| n < 0)
            || rows
                .windows(2)
                .any(|pair| pair[0].ordinal >= pair[1].ordinal)
            || rows
                .first()
                .is_some_and(|row| row.ordinal <= after.unwrap_or(-1))
        {
            return Err(LogError::InvalidPageBudget);
        }
        self.profile.database().transaction(|c| {
            self.check_receive_owner(c)?;
            let (current,mut ticket):(Option<i64>,i64)=c.query_row("SELECT after_commit,next_ticket FROM file_receive_scan WHERE account=?1",[self.account.as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
            if current!=after {return Err(LogError::StaleCursor);}
            for row in rows {
                if !row.active || row.header.kind!=EventKind::FileManifest || row.header.author==own {continue;}
                if !event_ownership::participant_event_owned_on(c, self.check_receive_owner(c)?, row.ordinal)? { continue; }
                let bytes=i64::try_from(row.header.stored_bytes).map_err(|_|LogError::RepositoryCounterExhausted)?;
                let inserted=c.execute("INSERT OR IGNORE INTO file_receive_pending SELECT ?1,?2,?3,?4,?5 WHERE NOT EXISTS(SELECT 1 FROM file_book_processed WHERE account=?1 AND event=?2)",params![self.account.as_slice(),row.header.id.as_bytes().as_slice(),row.header.conversation.as_bytes().as_slice(),bytes,ticket])?;
                if inserted!=0 {ticket=ticket.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?;}
            }
            if let Some(last)=rows.last() {c.execute("UPDATE file_receive_scan SET after_commit=?1,next_ticket=?2 WHERE account=?3",params![last.ordinal,ticket,self.account.as_slice()])?;}
            Ok(())
        })
    }
    /// Admit rows and decoded allocation cost before the first payload read.
    /// An oversized legal event retains its turn and returns an explicit budget
    /// error when no candidate fits; size never becomes processing evidence.
    pub(crate) fn receive_candidates(
        &self,
        conversation: ConversationId,
        rows: usize,
        bytes: usize,
    ) -> Result<Vec<EventId>, LogError> {
        if rows == 0 || rows > 128 || bytes == 0 || bytes > 8 * 1024 * 1024 {
            return Err(LogError::InvalidPageBudget);
        }
        let (selected,required)=self.profile.database().transaction(|c| {
            self.check_receive_owner(c)?;
            let mut statement=c.prepare_cached("SELECT p.event,p.bytes,EXISTS(SELECT 1 FROM file_book_processed d WHERE d.account=p.account AND d.event=p.event) FROM file_receive_pending p WHERE p.account=?1 AND p.conversation=?2 ORDER BY ticket,event LIMIT ?3")?;
            let mut found=statement.query(params![self.account.as_slice(),conversation.as_bytes().as_slice(),rows as i64])?;
            let mut candidates=Vec::new();let mut used=0usize;let mut required:Option<usize>=None;
            while let Some(row)=found.next()? {
                let id=EventId::new(blob(row,0)?);let done:bool=row.get(2)?;
                let size:i64=row.get(1)?;
                let cost=usize::try_from(size).ok().and_then(|n|n.checked_mul(3)).and_then(|n|n.checked_add(std::mem::size_of::<Event>())).ok_or_else(||invalid("invalid file receive allocation cost"))?;
                let admitted=!done && used.checked_add(cost).is_some_and(|n|n<=bytes);
                if admitted {used+=cost;} else if !done {required=Some(required.map_or(cost,|old|old.min(cost)));}
                candidates.push((id,done,admitted));
            }
            drop(found);drop(statement);
            let mut ticket:i64=c.query_row("SELECT next_ticket FROM file_receive_scan WHERE account=?1",[self.account.as_slice()],|r|r.get(0))?;
            let mut selected=Vec::new();
            for (id,done,admitted) in candidates {
                if done {c.execute("DELETE FROM file_receive_pending WHERE account=?1 AND event=?2",params![self.account.as_slice(),id.as_bytes().as_slice()])?;} else {
                    c.execute("UPDATE file_receive_pending SET ticket=?1 WHERE account=?2 AND event=?3",params![ticket,self.account.as_slice(),id.as_bytes().as_slice()])?;
                    ticket=ticket.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?;
                    if admitted {selected.push(id);}
                }
            }
            c.execute("UPDATE file_receive_scan SET next_ticket=?1 WHERE account=?2",params![ticket,self.account.as_slice()])?;
            Ok((selected,required))
        })?;
        if selected.is_empty()
            && let Some(required) = required
        {
            return Err(LogError::PageBudgetTooSmall { required });
        }
        Ok(selected)
    }
    /// Select a fixed prefix before deduplicating, so a large pending population
    /// cannot require an all-conversation aggregate or allocation.
    pub(crate) fn receive_conversations(&self) -> Result<Vec<ConversationId>, LogError> {
        self.profile.database().read(|c| {
            self.check_receive_owner(c)?;
            let mut statement=c.prepare_cached("SELECT conversation FROM file_receive_pending WHERE account=?1 ORDER BY ticket,event LIMIT 8")?;
            let mut rows=statement.query([self.account.as_slice()])?;let mut selected=Vec::new();
            while let Some(row)=rows.next()? {let conversation=ConversationId::new(blob(row,0)?);if !selected.contains(&conversation) {selected.push(conversation);}}
            Ok(selected)
        })
    }
    pub(crate) fn forget_receive(&self, id: EventId) -> Result<(), LogError> {
        self.profile.database().transaction(|c| {
            self.check_receive_owner(c)?;
            c.execute(
                "DELETE FROM file_receive_pending WHERE account=?1 AND event=?2",
                params![self.account.as_slice(), id.as_bytes().as_slice()],
            )?;
            Ok(())
        })
    }
}
