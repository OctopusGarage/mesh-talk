//! Derived bounded DM receive work. Exact transcript evidence survives erasure;
//! this queue only schedules retained signed events and is not a delivery journal.
use super::*;
use crate::identity::account::AccountPublic;
use crate::storage::profile::participant::event_ownership::{
    ParticipantEventOwner, capture_event_owner_on, check_event_owner_on,
    participant_event_namespace_on, participant_event_owned_on,
};

pub(super) const LEGACY_RECEIVE_SCHEMA: &str = "
CREATE TABLE participant_receive_scan(domain BLOB PRIMARY KEY CHECK(length(domain)=16), owner BLOB NOT NULL CHECK(length(owner)=32), after_commit INTEGER NOT NULL CHECK(after_commit>=-1), next_ticket INTEGER NOT NULL CHECK(typeof(next_ticket)='integer' AND next_ticket>0),initialized INTEGER NOT NULL CHECK(initialized IN(0,1)),controls_after INTEGER NOT NULL CHECK(controls_after>=-1),controls_through INTEGER NOT NULL CHECK(controls_through>=-1)) WITHOUT ROWID;
CREATE TABLE participant_receive_pending(domain BLOB NOT NULL CHECK(length(domain)=16), id BLOB NOT NULL CHECK(length(id)=32), conversation BLOB NOT NULL CHECK(length(conversation)=32), ticket INTEGER NOT NULL CHECK(ticket>0), purpose INTEGER NOT NULL CHECK(purpose IN(0,1)), PRIMARY KEY(domain,id), FOREIGN KEY(domain,id) REFERENCES retained_events(domain,id) ON DELETE CASCADE) WITHOUT ROWID;
CREATE TABLE participant_control_processed(domain BLOB NOT NULL CHECK(length(domain)=16),id BLOB NOT NULL CHECK(length(id)=32),PRIMARY KEY(domain,id),FOREIGN KEY(domain,id) REFERENCES retained_events(domain,id) ON DELETE CASCADE) WITHOUT ROWID;
CREATE INDEX participant_receive_turn ON participant_receive_pending(domain,conversation,purpose,ticket,id);
";
const RECEIVE_SCHEMA: &str = "
CREATE TABLE participant_receive_scan(domain BLOB PRIMARY KEY CHECK(length(domain)=16), owner BLOB NOT NULL CHECK(length(owner)=32), after_commit INTEGER NOT NULL CHECK(after_commit>=-1), next_ticket INTEGER NOT NULL CHECK(typeof(next_ticket)='integer' AND next_ticket>0),initialized INTEGER NOT NULL CHECK(initialized IN(0,1)),controls_after INTEGER NOT NULL CHECK(controls_after>=-1),controls_through INTEGER NOT NULL CHECK(controls_through>=-1)) WITHOUT ROWID;
CREATE TABLE participant_receive_pending(domain BLOB NOT NULL CHECK(length(domain)=16), id BLOB NOT NULL CHECK(length(id)=32), conversation BLOB NOT NULL CHECK(length(conversation)=32), account TEXT NOT NULL CHECK(length(account)=32), ticket INTEGER NOT NULL CHECK(ticket>0), purpose INTEGER NOT NULL CHECK(purpose IN(0,1)), PRIMARY KEY(domain,id), FOREIGN KEY(domain,id) REFERENCES retained_events(domain,id) ON DELETE CASCADE) WITHOUT ROWID;
CREATE INDEX participant_receive_turn ON participant_receive_pending(domain,account,conversation,purpose,ticket,id);
CREATE TABLE participant_receive_bootstrap(domain BLOB NOT NULL CHECK(length(domain)=16),account TEXT NOT NULL CHECK(length(account)=32),controls_after INTEGER NOT NULL CHECK(controls_after>=-1),controls_through INTEGER NOT NULL CHECK(controls_through>=-1),PRIMARY KEY(domain,account)) WITHOUT ROWID;
";
const PROCESSED_SCHEMA: &str = "CREATE TABLE participant_control_processed(domain BLOB NOT NULL CHECK(length(domain)=16),id BLOB NOT NULL CHECK(length(id)=32),PRIMARY KEY(domain,id),FOREIGN KEY(domain,id) REFERENCES retained_events(domain,id) ON DELETE CASCADE) WITHOUT ROWID;";
impl EventRepository {
    pub(crate) fn receiving_owner(&self) -> Result<ParticipantEventOwner, LogError> {
        self.database.read(|c| self.receive_owner_on(c).cloned())
    }
    fn receive_owner_on(&self, c: &Connection) -> Result<&ParticipantEventOwner, LogError> {
        let owner = self.receive_owner.get().ok_or(LogError::StaleCursor)?;
        check_event_owner_on(c, owner)?;
        Ok(owner)
    }
    pub(crate) fn check_receive_event(&self, id: EventId) -> Result<(), LogError> {
        self.database.read(|c| self.check_receive_event_on(c, id))
    }
    fn check_receive_event_on(&self, c: &Connection, id: EventId) -> Result<(), LogError> {
        let owner = self.receive_owner_on(c)?;
        let ordinal = c.query_row(
            "SELECT commit_ordinal FROM retained_events WHERE domain=?1 AND id=?2",
            params![self.domain.0.as_slice(), id.as_bytes().as_slice()],
            |r| r.get(0),
        )?;
        if !participant_event_owned_on(c, owner, ordinal)? {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    /// Only first adoption discovers existing retained metadata. Warm open checks
    /// fixed authority and leaves ordinary discovery to bounded receive passes.
    pub(crate) fn initialize_receiving(
        &self,
        owner: Author,
        account: &AccountPublic,
    ) -> Result<(), LogError> {
        if self.domain.0 != *b"MeshPartEventsV1" {
            return Err(corrupt("receive work requires participant events"));
        }
        let captured = self.database.read(|c| capture_event_owner_on(c, account))?;
        let bound = self.receive_owner.get_or_init(|| captured.clone());
        if bound != &captured {
            return Err(LogError::StaleCursor);
        }
        let fresh = self.database.transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='participant_receive_scan')",
                [],
                |r| r.get(0),
            )?;
            self.receive_owner_on(c)?;
            if present {
                let scoped: bool = c.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_info('participant_receive_pending') WHERE name='account')", [], |r| r.get(0))?;
                if !scoped {
                    crate::storage::profile::manifest::validate_ddl(c, LEGACY_RECEIVE_SCHEMA)?;
                    // This is a derived queue. Rebuild its metadata from retained
                    // commits once, preserving exact completed-control evidence.
                    c.execute_batch("DROP TABLE participant_receive_pending; DROP TABLE participant_receive_scan;")?;
                }
                if !scoped { c.execute_batch(RECEIVE_SCHEMA)?; }
            } else {
                c.execute_batch(RECEIVE_SCHEMA)?;
                c.execute_batch(PROCESSED_SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, RECEIVE_SCHEMA)?;
            crate::storage::profile::manifest::validate_ddl(c, PROCESSED_SCHEMA)?;
            c.execute("INSERT OR IGNORE INTO participant_receive_bootstrap SELECT ?1,?2,-1,next_commit-1 FROM event_domains WHERE domain=?1", params![self.domain.0.as_slice(),captured.namespace()])?;
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
            self.database.transaction(|c|{self.receive_owner_on(c)?;c.execute("UPDATE participant_receive_scan SET initialized=1 WHERE domain=?1",[self.domain.0.as_slice()])?;c.execute("UPDATE participant_receive_bootstrap SET controls_through=max(controls_through,(SELECT after_commit FROM participant_receive_scan WHERE domain=?1)) WHERE domain=?1 AND account=?2", params![self.domain.0.as_slice(),captured.namespace()])?;Ok(())})?;
        }
        Ok(())
    }
    pub(crate) fn bootstrap_control_position(&self) -> Result<(i64, i64), LogError> {
        self.database.read(|c| { let owner=self.receive_owner_on(c)?;
            Ok(c.query_row("SELECT controls_after,controls_through FROM participant_receive_bootstrap WHERE domain=?1 AND account=?2",params![self.domain.0.as_slice(),owner.namespace()],|r|Ok((r.get(0)?,r.get(1)?)))?) })
    }
    pub(crate) fn advance_control_bootstrap(&self, ordinal: i64) -> Result<(), LogError> {
        self.database.transaction(|c| { let owner=self.receive_owner_on(c)?;
            c.execute("UPDATE participant_receive_bootstrap SET controls_after=?3 WHERE domain=?1 AND account=?2 AND controls_after<=?3 AND controls_through>=?3",params![self.domain.0.as_slice(),owner.namespace(),ordinal])?; Ok(()) })
    }
    /// Account qualification and control kind selection happen before LIMIT and
    /// before any body read, including the local author's legacy controls.
    pub(crate) fn bootstrap_control_headers(
        &self,
        after: i64,
        through: i64,
    ) -> Result<Vec<CommittedHeader>, LogError> {
        self.database.read(|c| {
            let owner=self.receive_owner_on(c)?;
            let columns=HEADER_COLUMNS.split(',').map(|s|format!("e.{s}")).collect::<Vec<_>>().join(",");
            let mut statement=c.prepare(&format!("SELECT {columns},e.commit_ordinal,s.lifecycle FROM retained_events e JOIN event_conversations s ON s.domain=e.domain AND s.conversation=e.conversation WHERE e.domain=?1 AND e.commit_ordinal>?2 AND e.commit_ordinal<=?3 AND e.kind IN(2,3) AND EXISTS(SELECT 1 FROM participant_event_ranges r WHERE r.account_id=?4 AND r.start_commit<=e.commit_ordinal AND (r.end_commit IS NULL OR e.commit_ordinal<r.end_commit)) ORDER BY e.commit_ordinal LIMIT 128"))?;
            let mut rows=statement.query(params![self.domain.0.as_slice(),after,through,owner.namespace()])?;
            let mut result=Vec::new();
            while let Some(row)=rows.next()? { let lifecycle:i64=row.get(13)?;
                if !(0..=1).contains(&lifecycle) {return Err(corrupt("invalid discovery lifecycle"));}
                result.push(CommittedHeader{ordinal:nonnegative(row.get(12)?)?,header:read_header(row)?,active:lifecycle==0});
            }
            Ok(result)
        })
    }
    pub(crate) fn discover_receives(&self) -> Result<bool, LogError> {
        self.database.transaction(|c| {
            self.receive_owner_on(c)?;
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
                    let namespace=participant_event_namespace_on(c,last)?;
                    let inserted=c.execute("INSERT OR IGNORE INTO participant_receive_pending VALUES(?1,?2,?3,?4,?5,?6)",params![self.domain.0.as_slice(),id.as_slice(),conversation.as_slice(),namespace,ticket,kind!=0])?;
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
            self.check_receive_event_on(c, id)?;
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
        let (ids, required) = self.database.transaction(|c| {
            let owner=self.receive_owner_on(c)?;
            let mut ticket:i64=c.query_row("SELECT next_ticket FROM participant_receive_scan WHERE domain=?1",[self.domain.0.as_slice()],|r|r.get(0))?;
            let mut statement=c.prepare("SELECT p.id,e.stored_bytes,(s.lifecycle<>0 OR (e.kind=0 AND EXISTS(SELECT 1 FROM transcript_received_evidence d WHERE d.id=p.id)) OR (e.kind<>0 AND EXISTS(SELECT 1 FROM participant_control_processed d WHERE d.domain=p.domain AND d.id=p.id))) FROM participant_receive_pending p JOIN retained_events e ON e.domain=p.domain AND e.id=p.id JOIN event_conversations s ON s.domain=e.domain AND s.conversation=e.conversation WHERE p.domain=?1 AND p.account=?5 AND p.conversation=?2 AND p.purpose=?4 AND EXISTS(SELECT 1 FROM participant_event_ranges r WHERE r.account_id=?5 AND r.start_commit<=e.commit_ordinal AND (r.end_commit IS NULL OR e.commit_ordinal<r.end_commit)) ORDER BY p.ticket,p.id LIMIT ?3")?;
            let mut rows=statement.query(params![self.domain.0.as_slice(),conversation.as_bytes().as_slice(),integer(limit)?,controls,owner.namespace()])?;
            // Keep only fixed keys before mutating the queue's ordering index.
            let mut selected=Vec::new();let mut charged=0usize;let mut required:Option<usize>=None;
            while let Some(row)=rows.next()? {
                let id=blob::<32>(row,0)?;
                let done:bool=row.get(2)?;
                let cost=if done {0}else{usize::try_from(nonnegative(row.get(1)?)?).ok().and_then(|n|n.checked_mul(3)).and_then(|n|n.checked_add(std::mem::size_of::<Event>())).ok_or(LogError::RepositoryCounterExhausted)?};
                let admitted=!done && charged.checked_add(cost).is_some_and(|total|total<=bytes);
                if admitted {charged+=cost;} else if !done {required=Some(required.map_or(cost,|old|old.min(cost)));}
                // Even an oversized attempt advances its fair turn. It remains
                // eligible and never becomes processing evidence merely because
                // this caller supplied a smaller byte budget.
                selected.push((id,done,admitted));
            }
            drop(rows);drop(statement);
            let mut ids=Vec::new();
            for (id,done,admitted) in selected {
                if done {c.execute("DELETE FROM participant_receive_pending WHERE domain=?1 AND id=?2",params![self.domain.0.as_slice(),id.as_slice()])?;} else {
                    c.execute("UPDATE participant_receive_pending SET ticket=?3 WHERE domain=?1 AND id=?2",params![self.domain.0.as_slice(),id.as_slice(),ticket])?;
                    ticket=advance(ticket)?;if admitted {ids.push(EventId::new(id));}
                }
            }
            c.execute("UPDATE participant_receive_scan SET next_ticket=?2 WHERE domain=?1",params![self.domain.0.as_slice(),ticket])?;
            Ok((ids,required))
        })?;
        if ids.is_empty()
            && let Some(required) = required
        {
            return Err(LogError::PageBudgetTooSmall { required });
        }
        Ok(ids)
    }
}
