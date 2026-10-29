//! Derived bounded DM receive work. Exact transcript evidence survives erasure;
//! this queue only schedules retained signed events and is not a delivery journal.
use super::*;

const RECEIVE_SCHEMA: &str = "
CREATE TABLE participant_receive_scan(domain BLOB PRIMARY KEY CHECK(length(domain)=16), owner BLOB NOT NULL CHECK(length(owner)=32), after_commit INTEGER NOT NULL CHECK(after_commit>=-1), next_ticket INTEGER NOT NULL CHECK(typeof(next_ticket)='integer' AND next_ticket>0),initialized INTEGER NOT NULL CHECK(initialized IN(0,1)),controls_after INTEGER NOT NULL CHECK(controls_after>=-1),controls_through INTEGER NOT NULL CHECK(controls_through>=-1)) WITHOUT ROWID;
CREATE TABLE participant_receive_pending(domain BLOB NOT NULL CHECK(length(domain)=16), id BLOB NOT NULL CHECK(length(id)=32), conversation BLOB NOT NULL CHECK(length(conversation)=32), ticket INTEGER NOT NULL CHECK(ticket>0), purpose INTEGER NOT NULL CHECK(purpose IN(0,1)), PRIMARY KEY(domain,id), FOREIGN KEY(domain,id) REFERENCES retained_events(domain,id) ON DELETE CASCADE) WITHOUT ROWID;
CREATE TABLE participant_control_processed(domain BLOB NOT NULL CHECK(length(domain)=16),id BLOB NOT NULL CHECK(length(id)=32),PRIMARY KEY(domain,id),FOREIGN KEY(domain,id) REFERENCES retained_events(domain,id) ON DELETE CASCADE) WITHOUT ROWID;
CREATE INDEX participant_receive_turn ON participant_receive_pending(domain,conversation,purpose,ticket,id);
";
impl EventRepository {
    /// Only first adoption discovers existing retained metadata. Warm open checks
    /// fixed authority and leaves ordinary discovery to bounded receive passes.
    pub(crate) fn initialize_receiving(&self, owner: Author) -> Result<(), LogError> {
        let fresh = self.database.transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='participant_receive_scan')",
                [],
                |r| r.get(0),
            )?;
            if !present {
                c.execute_batch(RECEIVE_SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, RECEIVE_SCHEMA)?;
            let fresh = c.execute(
                "INSERT OR IGNORE INTO participant_receive_scan VALUES(?1,?2,-1,1,0,-1,-1)",
                params![self.domain.0.as_slice(), owner.ed25519_pub().as_slice()],
            )? != 0;
            let bound: Vec<u8> = c.query_row(
                "SELECT owner FROM participant_receive_scan WHERE domain=?1",
                [self.domain.0.as_slice()],
                |r| r.get(0),
            )?;
            if bound != owner.ed25519_pub() {
                return Err(corrupt("receive discovery owner changed"));
            }
            let initialized: bool = c.query_row(
                "SELECT initialized FROM participant_receive_scan WHERE domain=?1",
                [self.domain.0.as_slice()],
                |r| r.get(0),
            )?;
            Ok(fresh || !initialized)
        })?;
        if fresh {
            while self.discover_receives()? {}
            self.database.transaction(|c|{c.execute("UPDATE participant_receive_scan SET initialized=1,controls_through=after_commit WHERE domain=?1",[self.domain.0.as_slice()])?;Ok(())})?;
        }
        Ok(())
    }
    pub(crate) fn bootstrap_control_position(&self) -> Result<(i64, i64), LogError> {
        self.database.read(|c|Ok(c.query_row("SELECT controls_after,controls_through FROM participant_receive_scan WHERE domain=?1",[self.domain.0.as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?))
    }
    pub(crate) fn advance_control_bootstrap(&self, ordinal: i64) -> Result<(), LogError> {
        self.database.transaction(|c|{c.execute("UPDATE participant_receive_scan SET controls_after=?2 WHERE domain=?1 AND controls_after<=?2 AND controls_through>=?2",params![self.domain.0.as_slice(),ordinal])?;Ok(())})
    }
    pub(crate) fn discover_receives(&self) -> Result<bool, LogError> {
        self.database.transaction(|c| {
            let (owner,after,mut ticket):(Vec<u8>,i64,i64)=c.query_row("SELECT owner,after_commit,next_ticket FROM participant_receive_scan WHERE domain=?1",[self.domain.0.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            let mut statement=c.prepare("SELECT id,conversation,author,kind,commit_ordinal FROM retained_events WHERE domain=?1 AND commit_ordinal>?2 ORDER BY commit_ordinal LIMIT 128")?;
            let mut rows=statement.query(params![self.domain.0.as_slice(),after])?;
            let mut last=after;let mut scanned=0;
            while let Some(row)=rows.next()? {
                let id=blob::<32>(row,0)?;
                let conversation=blob::<32>(row,1)?;
                let author=blob::<32>(row,2)?;
                let kind:i64=row.get(3)?;
                last=nonnegative(row.get(4)?)?;scanned+=1;
                if ![0,2,3].contains(&kind) || author.as_slice()==owner.as_slice(){continue;}
                let seen:bool=if kind==0 {c.query_row("SELECT EXISTS(SELECT 1 FROM transcript_received_evidence WHERE id=?1)",[id.as_slice()],|r|r.get(0))?}else{c.query_row("SELECT EXISTS(SELECT 1 FROM participant_control_processed WHERE domain=?1 AND id=?2)",params![self.domain.0.as_slice(),id.as_slice()],|r|r.get(0))?};
                if !seen {
                    let inserted=c.execute("INSERT OR IGNORE INTO participant_receive_pending VALUES(?1,?2,?3,?4,?5)",params![self.domain.0.as_slice(),id.as_slice(),conversation.as_slice(),ticket,kind!=0])?;
                    if inserted!=0 {ticket=advance(ticket)?;}
                }
            }
            if last!=after {c.execute("UPDATE participant_receive_scan SET after_commit=?2,next_ticket=?3 WHERE domain=?1",params![self.domain.0.as_slice(),last,ticket])?;}
            Ok(scanned==128)
        })
    }
    /// Advances attempt turns even when author/key prerequisites are unavailable.
    /// Completed entries are removed through exact evidence, never a TTL/Bloom.
    pub(crate) fn pending_receive_ids(
        &self,
        conversation: ConversationId,
        limit: usize,
        bytes: usize,
    ) -> Result<Vec<EventId>, LogError> {
        self.pending_receive_work(conversation, limit, bytes, false)
    }
    pub(crate) fn pending_control_ids(
        &self,
        conversation: ConversationId,
        limit: usize,
        bytes: usize,
    ) -> Result<Vec<EventId>, LogError> {
        self.pending_receive_work(conversation, limit, bytes, true)
    }
    pub(crate) fn complete_control(&self, id: EventId) -> Result<(), LogError> {
        self.database.transaction(|c| {
            c.execute(
                "INSERT OR IGNORE INTO participant_control_processed VALUES(?1,?2)",
                params![self.domain.0.as_slice(), id.as_bytes().as_slice()],
            )?;
            c.execute(
                "DELETE FROM participant_receive_pending WHERE domain=?1 AND id=?2",
                params![self.domain.0.as_slice(), id.as_bytes().as_slice()],
            )?;
            Ok(())
        })
    }
    fn pending_receive_work(
        &self,
        conversation: ConversationId,
        limit: usize,
        bytes: usize,
        controls: bool,
    ) -> Result<Vec<EventId>, LogError> {
        if limit == 0 || limit > 128 || bytes == 0 || bytes > 8 * 1024 * 1024 {
            return Err(LogError::InvalidPageBudget);
        }
        self.discover_receives()?;
        self.database.transaction(|c| {
            let mut ticket:i64=c.query_row("SELECT next_ticket FROM participant_receive_scan WHERE domain=?1",[self.domain.0.as_slice()],|r|r.get(0))?;
            let mut statement=c.prepare("SELECT p.id,e.stored_bytes,(s.lifecycle<>0 OR e.ciphertext_bytes>262144 OR e.parent_count>1024 OR (e.kind=0 AND EXISTS(SELECT 1 FROM transcript_received_evidence d WHERE d.id=p.id)) OR (e.kind<>0 AND EXISTS(SELECT 1 FROM participant_control_processed d WHERE d.domain=p.domain AND d.id=p.id))) FROM participant_receive_pending p JOIN retained_events e ON e.domain=p.domain AND e.id=p.id JOIN event_conversations s ON s.domain=e.domain AND s.conversation=e.conversation WHERE p.domain=?1 AND p.conversation=?2 AND p.purpose=?4 ORDER BY p.ticket,p.id LIMIT ?3")?;
            let mut rows=statement.query(params![self.domain.0.as_slice(),conversation.as_bytes().as_slice(),integer(limit)?,controls])?;
            // Keep only fixed keys before mutating the queue's ordering index.
            let mut selected=Vec::new();let mut charged=0usize;
            while let Some(row)=rows.next()? {
                let id=blob::<32>(row,0)?;
                let done:bool=row.get(2)?;
                let cost=if done {0}else{usize::try_from(nonnegative(row.get(1)?)?).ok().and_then(|n|n.checked_mul(3)).and_then(|n|n.checked_add(std::mem::size_of::<Event>())).ok_or(LogError::RepositoryCounterExhausted)?};
                if charged.checked_add(cost).is_none_or(|total|total>bytes) {
                    if selected.is_empty(){return Err(LogError::PageBudgetTooSmall{required:cost});}
                    break;
                }
                charged+=cost;selected.push((id,done));
            }
            drop(rows);drop(statement);
            let mut ids=Vec::new();
            for (id,done) in selected {
                if done {c.execute("DELETE FROM participant_receive_pending WHERE domain=?1 AND id=?2",params![self.domain.0.as_slice(),id.as_slice()])?;} else {
                    c.execute("UPDATE participant_receive_pending SET ticket=?3 WHERE domain=?1 AND id=?2",params![self.domain.0.as_slice(),id.as_slice(),ticket])?;
                    ticket=advance(ticket)?;ids.push(EventId::new(id));
                }
            }
            c.execute("UPDATE participant_receive_scan SET next_ticket=?2 WHERE domain=?1",params![self.domain.0.as_slice(),ticket])?;
            Ok(ids)
        })
    }
}
