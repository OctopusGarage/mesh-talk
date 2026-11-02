//! Disposable selected-query state and authenticated decoded reaction facts.
//! Source rows stay authoritative; every query checks its captured account epoch.
use super::*;
use crate::storage::profile::{
    context::ProfileStorage,
    participant::event_ownership::{ParticipantEventOwner, check_event_owner_on},
};
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Arc;

#[cfg(test)]
thread_local! {static METADATA_READS:std::cell::Cell<usize>=const{std::cell::Cell::new(0)};}
#[cfg(test)]
pub(super) fn take_metadata_reads() -> usize {
    METADATA_READS.with(|value| value.replace(0))
}
const SCHEMA: &str = "
CREATE TABLE reaction_query_state(id BLOB PRIMARY KEY CHECK(length(id)=16),account BLOB NOT NULL CHECK(length(account)=32),tag BLOB NOT NULL CHECK(length(tag)=32),phase INTEGER NOT NULL DEFAULT 0,after_row INTEGER NOT NULL DEFAULT -1,target_position INTEGER NOT NULL DEFAULT 0,document_cursor BLOB,revision INTEGER NOT NULL DEFAULT 0,retired INTEGER NOT NULL DEFAULT 0 CHECK(retired IN(0,1))) WITHOUT ROWID;
CREATE UNIQUE INDEX reaction_query_active ON reaction_query_state(account,tag) WHERE retired=0;
CREATE TABLE reaction_query_targets(query BLOB NOT NULL REFERENCES reaction_query_state(id),target BLOB NOT NULL CHECK(length(target)=32),PRIMARY KEY(query,target)) WITHOUT ROWID;
CREATE TABLE reaction_query_latest(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,query BLOB NOT NULL REFERENCES reaction_query_state(id),target BLOB NOT NULL,emoji TEXT NOT NULL,principal TEXT NOT NULL,clock BLOB NOT NULL CHECK(length(clock)=8),removed INTEGER NOT NULL,mine INTEGER NOT NULL,UNIQUE(query,target,emoji,principal));
CREATE TABLE reaction_query_summary(ordinal INTEGER PRIMARY KEY AUTOINCREMENT,query BLOB NOT NULL REFERENCES reaction_query_state(id),target BLOB NOT NULL,emoji TEXT NOT NULL,emoji_bytes INTEGER NOT NULL CHECK(emoji_bytes>=0),count INTEGER NOT NULL CHECK(count>=0),mine INTEGER NOT NULL CHECK(mine>=0),UNIQUE(query,target,emoji));
CREATE TABLE reaction_decode_scan(account BLOB NOT NULL,scope BLOB NOT NULL CHECK(length(scope) IN(0,32)),stamp BLOB NOT NULL CHECK(length(stamp)=32),after_commit INTEGER NOT NULL,PRIMARY KEY(account,scope)) WITHOUT ROWID;
CREATE TABLE reaction_decoded(account BLOB NOT NULL CHECK(length(account)=32),scope BLOB NOT NULL CHECK(length(scope) IN(0,32)),domain BLOB NOT NULL CHECK(length(domain)=16),event BLOB NOT NULL CHECK(length(event)=32),ordinal INTEGER NOT NULL,conversation BLOB NOT NULL CHECK(length(conversation)=32),stamp BLOB NOT NULL CHECK(length(stamp)=32),target BLOB,emoji_bytes INTEGER NOT NULL CHECK(emoji_bytes>=0),emoji TEXT,source BLOB,principal TEXT,clock BLOB,removed INTEGER,route_sender TEXT,route_recipient TEXT,PRIMARY KEY(account,scope,event),FOREIGN KEY(domain,event) REFERENCES retained_events(domain,id) ON DELETE CASCADE);
CREATE INDEX reaction_decoded_scan ON reaction_decoded(account,scope,stamp,target,ordinal);
";
const DOMAIN: &[u8; 16] = b"MeshPartEventsV1";
pub(super) struct Store {
    profile: Arc<ProfileStorage>,
    owner: ParticipantEventOwner,
}
pub(super) struct State {
    pub id: [u8; 16],
    pub phase: i64,
    pub after: i64,
    pub target: usize,
    pub document_cursor: Option<Vec<u8>>,
    pub revision: i64,
}
pub(super) struct Candidate {
    pub id: EventId,
    pub conversation: ConversationId,
    pub ordinal: i64,
    pub cached: bool,
}
pub(super) struct Decoded {
    pub id: EventId,
    pub ordinal: i64,
    pub conversation: ConversationId,
    pub payload: Option<ReactionPayload>,
    pub source: Option<PublicIdentity>,
    pub principal: Option<String>,
    pub wall: u64,
    pub route: Option<(String, String)>,
}
pub(super) struct Fact {
    pub target: EventId,
    pub emoji: String,
    pub principal: String,
    pub wall: u64,
    pub removed: bool,
}
fn invalid(s: &str) -> LogError {
    LogError::CorruptFile(s.into())
}
fn id(bytes: Vec<u8>) -> Result<EventId, LogError> {
    Ok(EventId::new(bytes.try_into().map_err(|_| {
        invalid("invalid selected reaction identity")
    })?))
}
fn clock(bytes: Vec<u8>) -> Result<u64, LogError> {
    Ok(u64::from_be_bytes(
        bytes
            .try_into()
            .map_err(|_| invalid("invalid selected reaction clock"))?,
    ))
}
impl Store {
    pub fn open(
        profile: Arc<ProfileStorage>,
        owner: ParticipantEventOwner,
    ) -> Result<Self, LogError> {
        profile.database().transaction(|c| {
            check_event_owner_on(c, &owner)?;
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='reaction_query_state')",
                [],
                |r| r.get(0),
            )?;
            if !present {
                c.execute_batch(SCHEMA)?;
            }
            crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
            Ok(())
        })?;
        Ok(Self { profile, owner })
    }
    fn checked<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        self.profile.database().read(|c| {
            check_event_owner_on(c, &self.owner)?;
            f(c)
        })
    }
    fn transaction<T>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, LogError>,
    ) -> Result<T, LogError> {
        self.profile.database().transaction(|c| {
            check_event_owner_on(c, &self.owner)?;
            f(c)
        })
    }
    pub fn source(&self) -> Result<(i64, i64), LogError> {
        self.checked(|c| {
            let epoch: i64 = c.query_row(
                "SELECT generation FROM participant_event_ownership WHERE singleton=1",
                [],
                |r| r.get(0),
            )?;
            let authored: i64 = c.query_row(
                "SELECT revision FROM authored_reaction_state WHERE account=?1",
                [self.owner.account().as_slice()],
                |r| r.get(0),
            )?;
            Ok((epoch, authored))
        })
    }
    pub fn begin(
        &self,
        tag: [u8; 32],
        targets: &[EventId],
        cursor: Option<&str>,
    ) -> Result<(State, i64), LogError> {
        self.transaction(|c|{
        cleanup(c)?;
        let (selected,after)=if let Some(cursor)=cursor{
            if cursor.len()>64{return Err(LogError::StaleCursor);}
            let (key,position)=cursor.split_once(':').ok_or(LogError::StaleCursor)?;
            let key:[u8;16]=hex::decode(key).ok().and_then(|v|v.try_into().ok()).ok_or(LogError::StaleCursor)?;
            let position:i64=position.parse().map_err(|_|LogError::StaleCursor)?;
            if position<0{return Err(LogError::StaleCursor);}
            (key,position)
        }else{
            let old:Option<Vec<u8>>=c.query_row("SELECT id FROM reaction_query_state WHERE account=?1 AND tag=?2 AND retired=0",params![self.owner.account().as_slice(),tag.as_slice()],|r|r.get(0)).optional()?;
            if let Some(old)=old{(old.try_into().map_err(|_|invalid("invalid reaction query"))?,0)}else{
                let active:i64=c.query_row("SELECT count(*) FROM (SELECT 1 FROM reaction_query_state WHERE retired=0 LIMIT 64)",[],|r|r.get(0))?;
                if active>=64 {c.execute("UPDATE reaction_query_state SET retired=1 WHERE id=(SELECT id FROM reaction_query_state WHERE retired=0 ORDER BY id LIMIT 1)",[])?;}
                let key=*uuid::Uuid::new_v4().as_bytes();
                c.execute("INSERT INTO reaction_query_state(id,account,tag) VALUES(?1,?2,?3)",params![key.as_slice(),self.owner.account().as_slice(),tag.as_slice()])?;
                for target in targets{c.execute("INSERT OR IGNORE INTO reaction_query_targets VALUES(?1,?2)",params![key.as_slice(),target.as_bytes().as_slice()])?;}
                (key,0)
            }
        };
        type QueryStateRow = (i64,i64,i64,Option<Vec<u8>>,i64);
        let row:Option<QueryStateRow>=c.query_row("SELECT phase,after_row,target_position,document_cursor,revision FROM reaction_query_state WHERE id=?1 AND account=?2 AND tag=?3 AND retired=0",params![selected.as_slice(),self.owner.account().as_slice(),tag.as_slice()],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?))).optional()?;
        let (phase,position,target,document_cursor,revision)=row.ok_or(LogError::StaleCursor)?;
        Ok((State{id:selected,phase,after:position,target:usize::try_from(target).map_err(|_|invalid("invalid reaction target cursor"))?,document_cursor,revision},after))
    })
    }
    pub fn cursor(query: [u8; 16], after: i64) -> String {
        format!("{}:{after}", hex::encode(query))
    }
    pub fn discovery(
        &self,
        scope: Option<ConversationId>,
        stamp: [u8; 32],
        through: i64,
    ) -> Result<(Vec<Candidate>, Option<i64>), LogError> {
        self.transaction(|c|{
        let scope_key=scope.map_or_else(Vec::new,|v|v.as_bytes().to_vec());
        c.execute("INSERT INTO reaction_decode_scan VALUES(?1,?2,?3,-1) ON CONFLICT(account,scope) DO UPDATE SET stamp=excluded.stamp,after_commit=-1 WHERE stamp<>excluded.stamp",params![self.owner.account().as_slice(),scope_key.as_slice(),stamp.as_slice()])?;
        let after:i64=c.query_row("SELECT after_commit FROM reaction_decode_scan WHERE account=?1 AND scope=?2",params![self.owner.account().as_slice(),scope_key.as_slice()],|r|r.get(0))?;
        // Seek one exact account-owned interval. A returning account retains
        // its earlier undiscovered work without walking intervening accounts.
        let range:Option<(i64,Option<i64>)>=c.query_row("SELECT start_commit,end_commit FROM participant_event_ranges WHERE account_id=?1 AND start_commit<=?2 ORDER BY start_commit DESC LIMIT 1",params![self.owner.namespace(),after.saturating_add(1)],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let range=match range {
            Some((start,end)) if end.is_none_or(|end|end>after.saturating_add(1))=>Some((start,end)),
            _=>c.query_row("SELECT start_commit,end_commit FROM participant_event_ranges WHERE account_id=?1 AND start_commit>?2 ORDER BY start_commit LIMIT 1",params![self.owner.namespace(),after],|r|Ok((r.get(0)?,r.get(1)?))).optional()?,
        };
        let Some((start,end))=range.filter(|(start,_)|*start<=through) else{return Ok((Vec::new(),None));};
        let lower=after.max(start-1);
        let upper=end.map_or(through,|end|through.min(end-1));
        let (index,scope_filter)=if scope.is_some(){("retained_events_commit","AND e.conversation=?2")}else{("retained_events_domain_commits","AND ?2 IS NULL")};
        let mut statement=c.prepare(&format!("SELECT e.id,e.conversation,e.commit_ordinal,e.stored_bytes,(e.kind<>3 OR (?2 IS NOT NULL AND e.conversation<>?2) OR s.lifecycle<>0 OR coalesce(d.stamp=?5,0)) FROM retained_events e INDEXED BY {index} JOIN event_conversations s ON s.domain=e.domain AND s.conversation=e.conversation LEFT JOIN reaction_decoded d ON d.account=?6 AND d.scope=?7 AND d.event=e.id WHERE e.domain=?1 AND e.commit_ordinal>?3 AND e.commit_ordinal<=?4 {scope_filter} ORDER BY e.commit_ordinal LIMIT 64"))?;
        let mut rows=statement.query(params![DOMAIN.as_slice(),scope.as_ref().map(|v|v.as_bytes().as_slice()),lower,upper,stamp.as_slice(),self.owner.account().as_slice(),scope_key.as_slice()])?;
        let mut out=Vec::new();let mut charged=0usize;
        while let Some(row)=rows.next()? {
            #[cfg(test)] METADATA_READS.with(|value|value.set(value.get()+1));
            let cached:bool=row.get(4)?;let stored=usize::try_from(row.get::<_,i64>(3)?).map_err(|_|invalid("invalid reaction body size"))?;
            let cost=if cached{128}else{stored.checked_mul(3).and_then(|n|n.checked_add(std::mem::size_of::<Event>())).ok_or(LogError::RepositoryCounterExhausted)?};
            if charged+cost>8*1024*1024{if out.is_empty(){return Err(LogError::PageBudgetTooSmall{required:cost});}break;}
            charged+=cost;
            out.push(Candidate{id:id(row.get(0)?)?,conversation:ConversationId::new(row.get::<_,Vec<u8>>(1)?.try_into().map_err(|_|invalid("invalid reaction conversation"))?),ordinal:row.get(2)?,cached});
        }
        let next=out.last().map(|candidate|candidate.ordinal).or_else(||(upper>after).then_some(upper));
        Ok((out,next))
    })
    }
    pub fn decoded(
        &self,
        scope: Option<ConversationId>,
        stamp: [u8; 32],
        after: i64,
        rows: &[Decoded],
    ) -> Result<(), LogError> {
        self.transaction(|c|{
        let scope_key=scope.map_or_else(Vec::new,|v|v.as_bytes().to_vec());
        let current:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM reaction_decode_scan WHERE account=?1 AND scope=?2 AND stamp=?3)",params![self.owner.account().as_slice(),scope_key.as_slice(),stamp.as_slice()],|r|r.get(0))?;
        if !current{return Err(LogError::StaleCursor);}
        for row in rows {
            let public=row.source.as_ref().map(|v|[v.ed25519_pub.as_slice(),v.x25519_pub.as_slice()].concat());
            c.execute("INSERT INTO reaction_decoded VALUES(?1,?15,?2,?3,?14,?4,?5,?6,?16,?7,?8,?9,?10,?11,?12,?13) ON CONFLICT(account,scope,event) DO UPDATE SET stamp=excluded.stamp,target=excluded.target,emoji_bytes=excluded.emoji_bytes,emoji=excluded.emoji,source=excluded.source,principal=excluded.principal,clock=excluded.clock,removed=excluded.removed,route_sender=excluded.route_sender,route_recipient=excluded.route_recipient",params![self.owner.account().as_slice(),DOMAIN.as_slice(),row.id.as_bytes().as_slice(),row.conversation.as_bytes().as_slice(),stamp.as_slice(),row.payload.as_ref().map(|p|p.target.as_bytes().as_slice()),row.payload.as_ref().map(|p|p.emoji.as_str()),public,row.principal,row.wall.to_be_bytes().as_slice(),row.payload.as_ref().map(|p|p.remove),row.route.as_ref().map(|r|r.0.as_str()),row.route.as_ref().map(|r|r.1.as_str()),row.ordinal,scope_key.as_slice(),row.payload.as_ref().map_or(0,|p|p.emoji.len()) as i64])?;
        }
        c.execute("UPDATE reaction_decode_scan SET after_commit=?4 WHERE account=?1 AND scope=?2 AND stamp=?3 AND after_commit<=?4",params![self.owner.account().as_slice(),scope_key.as_slice(),stamp.as_slice(),after])?;
        Ok(())
    })
    }
    pub fn advance(
        &self,
        state: &State,
        phase: i64,
        after: i64,
        target: usize,
        document: Option<&[u8]>,
        facts: &[Fact],
        own: &[String; 2],
    ) -> Result<(), LogError> {
        self.transaction(|c|{
        let matches:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM reaction_query_state WHERE id=?1 AND phase=?2 AND after_row=?3 AND target_position=?4 AND revision=?5 AND retired=0)",params![state.id.as_slice(),state.phase,state.after,state.target as i64,state.revision],|r|r.get(0))?;
        if !matches{return Err(LogError::StaleCursor);}
        for fact in facts{reduce(c,state.id,fact,own)?;}
        c.execute("UPDATE reaction_query_state SET phase=?2,after_row=?3,target_position=?4,document_cursor=?5,revision=revision+1 WHERE id=?1",params![state.id.as_slice(),phase,after,target as i64,document])?;
        Ok(())
    })
    }
    pub fn summaries(&self, state: &State, after: i64) -> Result<ReactionSummaryPage, LogError> {
        self.checked(|c|{
        if state.phase!=5{return Ok(ReactionSummaryPage{items:vec![],next:Some(Self::cursor(state.id,0)),pending:true});}
        let live:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM reaction_query_state WHERE id=?1 AND revision=?2 AND phase=5 AND retired=0)",params![state.id.as_slice(),state.revision],|r|r.get(0))?;
        if !live{return Err(LogError::StaleCursor);}
        let prior:Option<(Vec<u8>,String)>=if after==0 {None}else{
            let bytes:i64=c.query_row("SELECT emoji_bytes FROM reaction_query_summary WHERE query=?1 AND ordinal=?2",params![state.id.as_slice(),after],|r|r.get(0)).optional()?.ok_or(LogError::StaleCursor)?;
            let required=usize::try_from(bytes).ok().and_then(|n|n.checked_mul(6)).and_then(|n|n.checked_add(256)).ok_or(LogError::RepositoryCounterExhausted)?;
            if required>1024*1024{return Err(LogError::PageBudgetTooSmall{required});}
            c.query_row("SELECT target,emoji FROM reaction_query_summary WHERE query=?1 AND ordinal=?2",params![state.id.as_slice(),after],|r|Ok((r.get(0)?,r.get(1)?))).optional()?.ok_or(LogError::StaleCursor).map(Some)?
        };
        let seek=if prior.is_some(){"AND (target,emoji)>(?2,?3)"}else{"AND ?2 IS NULL AND ?3 IS NULL"};
        let mut statement=c.prepare(&format!("SELECT ordinal,emoji_bytes,count FROM reaction_query_summary WHERE query=?1 {seek} ORDER BY target,emoji LIMIT 129"))?;
        let headers=statement.query_map(params![state.id.as_slice(),prior.as_ref().map(|p|p.0.as_slice()),prior.as_ref().map(|p|p.1.as_str())],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
        let mut selected=Vec::new();let mut used=0usize;let mut more=false;
        for (ordinal,bytes,count) in headers{
            let bytes=usize::try_from(bytes).map_err(|_|invalid("invalid reaction summary size"))?;
            let count=u64::try_from(count).map_err(|_|invalid("invalid reaction summary count"))?;
            let charge=bytes.checked_mul(6).and_then(|v|v.checked_add(256)).ok_or(LogError::RepositoryCounterExhausted)?;
            if selected.len()==128 || used+charge>1024*1024{if selected.is_empty(){return Err(LogError::PageBudgetTooSmall{required:charge});}more=true;break;}
            selected.push((ordinal,count));used+=charge;
        }
        let mut items=Vec::new();for (ordinal,count) in &selected{
            if *count==0 {continue;}
            let (target,emoji,count,mine):(Vec<u8>,String,i64,i64)=c.query_row("SELECT target,emoji,count,mine FROM reaction_query_summary WHERE ordinal=?1",[ordinal],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
            items.push(ReactionSummary{target:hex::encode(target),emoji,count:u64::try_from(count).map_err(|_|invalid("invalid reaction summary count"))?,from_me:mine>0});
        }
        Ok(ReactionSummaryPage{items,next:more.then(||Self::cursor(state.id,selected.last().expect("nonempty summary page").0)),pending:false})
    })
    }
}
fn reduce(c: &Connection, query: [u8; 16], fact: &Fact, own: &[String; 2]) -> Result<(), LogError> {
    let old:Option<(Vec<u8>,bool,bool)>=c.query_row("SELECT clock,removed,mine FROM reaction_query_latest WHERE query=?1 AND target=?2 AND emoji=?3 AND principal=?4",params![query.as_slice(),fact.target.as_bytes().as_slice(),fact.emoji,fact.principal],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?))).optional()?;
    if let Some((wall, removed, _)) = &old {
        let prior = clock(wall.clone())?;
        if fact.wall < prior || (fact.wall == prior && (*removed || !fact.removed)) {
            return Ok(());
        }
    }
    let prior = old.as_ref().map_or(0, |r| i64::from(!r.1));
    let prior_mine = old.as_ref().map_or(0, |r| i64::from(!r.1 && r.2));
    let mine = own.contains(&fact.principal);
    let active = i64::from(!fact.removed);
    let mine_active = i64::from(mine && !fact.removed);
    c.execute("INSERT INTO reaction_query_latest(query,target,emoji,principal,clock,removed,mine) VALUES(?1,?2,?3,?4,?5,?6,?7) ON CONFLICT(query,target,emoji,principal) DO UPDATE SET clock=excluded.clock,removed=excluded.removed,mine=excluded.mine",params![query.as_slice(),fact.target.as_bytes().as_slice(),fact.emoji,fact.principal,fact.wall.to_be_bytes().as_slice(),fact.removed,mine])?;
    c.execute("INSERT INTO reaction_query_summary(query,target,emoji,emoji_bytes,count,mine) VALUES(?1,?2,?3,?8,?4,?5) ON CONFLICT(query,target,emoji) DO UPDATE SET count=count+?6,mine=mine+?7",params![query.as_slice(),fact.target.as_bytes().as_slice(),fact.emoji,active,mine_active,active-prior,mine_active-prior_mine,fact.emoji.len() as i64])?;
    Ok(())
}
fn cleanup(c: &Connection) -> Result<(), LogError> {
    let query: Option<Vec<u8>> = c
        .query_row(
            "SELECT id FROM reaction_query_state WHERE retired=1 LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    let Some(query) = query else {
        return Ok(());
    };
    for (table, key) in [
        ("reaction_query_latest", "ordinal"),
        ("reaction_query_summary", "ordinal"),
        ("reaction_query_targets", "target"),
    ] {
        let sql = format!(
            "DELETE FROM {table} WHERE query=?1 AND ({key}) IN (SELECT {key} FROM {table} WHERE query=?1 LIMIT 128)"
        );
        if c.execute(&sql, [&query])? > 0 {
            return Ok(());
        }
    }
    c.execute("DELETE FROM reaction_query_state WHERE id=?1", [query])?;
    Ok(())
}

pub(super) struct RawSelected {
    pub id: EventId,
    pub source: PublicIdentity,
    pub principal: Option<String>,
    pub wall: u64,
    pub payload: ReactionPayload,
}
impl Store {
    pub fn own_page(
        &self,
        state: &State,
        conversation: ConversationId,
        physical: EventId,
        logical: EventId,
        principal: &str,
    ) -> Result<(Vec<Fact>, Option<i64>), LogError> {
        self.checked(|c|{
        let mut statement=c.prepare("SELECT ordinal,body_bytes,1 FROM authored_reaction_keys INDEXED BY authored_reaction_target WHERE account=?1 AND conversation=?2 AND target=?3 AND ordinal>?4 ORDER BY ordinal LIMIT 64")?;
        let mut rows=statement.query(params![self.owner.account().as_slice(),conversation.as_bytes().as_slice(),physical.as_bytes().as_slice(),state.after])?;
        let (selected,next)=admit_strings(&mut rows,8*1024*1024)?;drop(rows);drop(statement);
        let mut out=Vec::new();for ordinal in selected{
            let (emoji,wall,removed):(String,Vec<u8>,bool)=c.query_row("SELECT emoji,clock,removed FROM authored_reaction_keys WHERE ordinal=?1",[ordinal],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?;
            out.push(Fact{target:logical,emoji,principal:principal.into(),wall:clock(wall)?,removed});
        }
        Ok((out,next))
    })
    }
    pub fn raw_page(
        &self,
        state: &State,
        conversation: ConversationId,
        peer: Option<&str>,
        physical: EventId,
        logical: EventId,
        stamp: [u8; 32],
        through: i64,
    ) -> Result<(Vec<RawSelected>, Option<i64>), LogError> {
        self.checked(|c|{
        let scope_key=if peer.is_some(){Vec::new()}else{conversation.as_bytes().to_vec()};
        let route=if peer.is_some(){"((d.route_sender=?6 AND d.route_recipient=?5) OR (d.route_sender=?5 AND d.route_recipient=?6))"}else{"d.route_sender IS NULL AND ?5 IS NULL AND ?6 IS NOT NULL"};
        let sql=format!("SELECT d.ordinal,d.emoji_bytes,coalesce((s.lifecycle=0 AND {route}),0) FROM reaction_decoded d INDEXED BY reaction_decoded_scan JOIN event_conversations s ON s.domain=d.domain AND s.conversation=d.conversation WHERE d.account=?1 AND d.scope=?2 AND d.stamp=?3 AND d.ordinal>?4 AND d.ordinal<=?7 AND d.target=?8 ORDER BY d.ordinal LIMIT 64");
        let mut statement=c.prepare(&sql)?;
        let mut rows=statement.query(params![self.owner.account().as_slice(),scope_key.as_slice(),stamp.as_slice(),state.after,peer,self.owner.namespace(),through,physical.as_bytes().as_slice()])?;
        let (selected,next)=admit_strings(&mut rows,8*1024*1024)?;drop(rows);drop(statement);
        let mut out=Vec::new();for ordinal in selected{
            type DecodedReactionRow = (Vec<u8>,String,Vec<u8>,Option<String>,Vec<u8>,bool,Option<String>);
            let row:DecodedReactionRow=c.query_row("SELECT event,emoji,source,principal,clock,removed,route_sender FROM reaction_decoded WHERE account=?1 AND scope=?2 AND stamp=?3 AND target=?4 AND ordinal=?5",params![self.owner.account().as_slice(),scope_key.as_slice(),stamp.as_slice(),physical.as_bytes().as_slice(),ordinal],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?,r.get(6)?)))?;
            let public:[u8;64]=row.2.try_into().map_err(|_|invalid("invalid decoded reaction source"))?;
            out.push(RawSelected{id:id(row.0)?,source:PublicIdentity{ed25519_pub:public[..32].try_into().expect("fixed identity"),x25519_pub:public[32..].try_into().expect("fixed identity")},principal:if peer.is_some(){row.6}else{row.3},wall:clock(row.4)?,payload:ReactionPayload{target:logical,emoji:row.1,remove:row.5}});
        }
        Ok((out,next))
    })
    }
    pub fn bound_account(
        &self,
        event: EventId,
        source: &PublicIdentity,
    ) -> Result<Option<String>, LogError> {
        self.checked(|c|{
        let binding:Option<Vec<u8>>=c.query_row("SELECT binding FROM private_channel_reactions WHERE account=?1 AND effect=?2 AND source=?3 AND length(binding)=32 ORDER BY ordinal DESC LIMIT 1",params![self.owner.account().as_slice(),event.as_bytes().as_slice(),source.ed25519_pub.as_slice()],|r|r.get(0)).optional()?;
        binding.map(|v|{let ed25519_pub=v.try_into().map_err(|_|invalid("invalid private reaction binding"))?;Ok(crate::identity::account::AccountPublic{ed25519_pub}.account_id())}).transpose()
    })
    }
}
fn admit_strings(
    rows: &mut rusqlite::Rows<'_>,
    budget: usize,
) -> Result<(Vec<i64>, Option<i64>), LogError> {
    let mut selected = Vec::new();
    let mut charged = 0usize;
    let mut after = None;
    while let Some(row) = rows.next()? {
        #[cfg(test)]
        METADATA_READS.with(|value| value.set(value.get() + 1));
        let bytes = usize::try_from(row.get::<_, i64>(1)?)
            .map_err(|_| invalid("invalid reaction selector size"))?;
        let wanted: bool = row.get(2)?;
        let required = if wanted {
            bytes
                .checked_mul(3)
                .and_then(|n| n.checked_add(512))
                .ok_or(LogError::RepositoryCounterExhausted)?
        } else {
            128
        };
        if charged.checked_add(required).is_none_or(|n| n > budget) {
            if after.is_none() {
                return Err(LogError::PageBudgetTooSmall { required });
            }
            break;
        }
        charged += required;
        let ordinal = row.get(0)?;
        after = Some(ordinal);
        if wanted {
            selected.push(ordinal);
        }
    }
    Ok((selected, after))
}
