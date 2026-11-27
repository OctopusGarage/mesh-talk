//! Compressed canonical summaries in the encrypted participant-work database.
//! Derived rows retire with their authorized selection; no plaintext sidecar.
use super::index::{self, Prefix, RangeRead, Summary, SummaryFrame};
use crate::eventlog::{LogError, repository::RepositoryOwner};
use rusqlite::{Connection, OptionalExtension, params};

pub(crate) struct SqlIndex {
    database: RepositoryOwner,
    view: [u8; 16],
    lane: [u8; 16],
    revision: u64,
}
impl SqlIndex {
    pub(crate) fn same_selection_lane(&self, other: &Self) -> bool {
        self.view == other.view && self.lane[15] == other.lane[15]
    }
    pub(crate) fn new(database: RepositoryOwner, view: [u8; 16], lane: u8) -> Self {
        {
            use rand_core::RngCore;
            let mut generation = [0; 16];
            rand_core::OsRng.fill_bytes(&mut generation);
            generation[15] = lane;
            Self {
                database,
                view,
                lane: generation,
                revision: 0,
            }
        }
    }
    fn validate(&self, c: &Connection) -> Result<(), LogError> {
        let active:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM participant_work.participant_sync_views WHERE id=?1 AND retired=0)",[self.view.as_slice()],|r|r.get(0))?;
        if active {
            Ok(())
        } else {
            Err(LogError::StaleCursor)
        }
    }
    fn summary_on(&self, c: &Connection, p: Prefix) -> Result<Summary, LogError> {
        let direct:Option<(i64,Vec<u8>)>=c.query_row("SELECT count,digest FROM participant_work.participant_sync_range_nodes WHERE view=?1 AND lane=?2 AND bits=?3 AND key=?4",params![self.view.as_slice(),self.lane.as_slice(),p.bits(),p.key().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((n, digest)) = direct {
            return Ok(Summary {
                count: usize::try_from(n).map_err(|_| LogError::StaleCursor)?,
                digest: digest.try_into().map_err(|_| LogError::StaleCursor)?,
            });
        }
        let mut upper = p.key();
        for bit in usize::from(p.bits())..256 {
            upper[bit / 8] |= 128 >> (bit % 8);
        }
        let edge = |order: &str| -> Result<Option<[u8; 32]>, LogError> {
            let id:Option<Vec<u8>>=c.query_row(&format!("SELECT id FROM participant_work.participant_sync_range_ids WHERE view=?1 AND lane=?2 AND id>=?3 AND id<=?4 ORDER BY id {order} LIMIT 1"),params![self.view.as_slice(),self.lane.as_slice(),p.key().as_slice(),upper.as_slice()],|r|r.get(0)).optional()?;
            id.map(|v| v.try_into().map_err(|_| LogError::StaleCursor))
                .transpose()
        };
        let Some(first) = edge("ASC")? else {
            return Ok(index::empty());
        };
        let last = edge("DESC")?.ok_or(LogError::StaleCursor)?;
        let base = Prefix::common(first, last);
        let depth = base.bits();
        let (count,digest):(i64,Vec<u8>)=c.query_row("SELECT count,digest FROM participant_work.participant_sync_range_nodes WHERE view=?1 AND lane=?2 AND bits=?3 AND key=?4",params![self.view.as_slice(),self.lane.as_slice(),depth,base.key().as_slice()],|r|Ok((r.get(0)?,r.get(1)?)))?;
        let summary = Summary {
            count: usize::try_from(count).map_err(|_| LogError::StaleCursor)?,
            digest: digest.try_into().map_err(|_| LogError::StaleCursor)?,
        };
        summary.lift(base, p).ok_or(LogError::StaleCursor)
    }
    /// Reuse a failed builder's namespace only after bounded retirement. A
    /// partial clear is never exposed as an authorized summary.
    pub(crate) fn clear_step(&mut self) -> Result<bool, LogError> {
        let complete=self.database.selection_work_transaction(|tx|{
            self.validate(tx)?;
            let mut remaining=128i64;
            for (table,key) in [("participant_sync_range_ids","id"),("participant_sync_range_nodes","bits,key")] {
                let removed=tx.execute(&format!("DELETE FROM participant_work.{table} WHERE view=?1 AND lane=?2 AND ({key}) IN (SELECT {key} FROM participant_work.{table} WHERE view=?1 AND lane=?2 LIMIT ?3)"),params![self.view.as_slice(),self.lane.as_slice(),remaining])?;
                remaining-=removed as i64;
                if remaining==0 {return Ok(false);}
            }
            Ok(true)
        })?;
        if complete {
            self.revision = 0;
        }
        Ok(complete)
    }
    pub(crate) fn insert_page(&mut self, ids: &[[u8; 32]]) -> Result<(), LogError> {
        if ids.len() > 32 {
            return Err(LogError::InvalidPageBudget);
        }
        let next = self
            .revision
            .checked_add(ids.len() as u64)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        self.database.selection_work_transaction(|tx|{
            self.validate(tx)?;
            for id in ids {
                tx.execute("INSERT INTO participant_work.participant_sync_range_ids(view,lane,id) VALUES(?1,?2,?3)",params![self.view.as_slice(),self.lane.as_slice(),id.as_slice()])?;
                let edge=|cmp:&str,order:&str|->Result<Option<[u8;32]>,LogError>{let bytes:Option<Vec<u8>>=tx.query_row(&format!("SELECT id FROM participant_work.participant_sync_range_ids WHERE view=?1 AND lane=?2 AND id {cmp} ?3 ORDER BY id {order} LIMIT 1"),params![self.view.as_slice(),self.lane.as_slice(),id.as_slice()],|r|r.get(0)).optional()?;bytes.map(|v|v.try_into().map_err(|_|LogError::StaleCursor)).transpose()};
                let neighbors=[edge("<","DESC")?,edge(">","ASC")?];
                let closest=neighbors.into_iter().flatten().map(|n|Prefix::common(*id,n).bits()).max();
                let first:[u8;32]=tx.query_row("SELECT id FROM participant_work.participant_sync_range_ids WHERE view=?1 AND lane=?2 ORDER BY id LIMIT 1",params![self.view.as_slice(),self.lane.as_slice()],|r|r.get::<_,Vec<u8>>(0))?.try_into().map_err(|_|LogError::StaleCursor)?;
                let last:[u8;32]=tx.query_row("SELECT id FROM participant_work.participant_sync_range_ids WHERE view=?1 AND lane=?2 ORDER BY id DESC LIMIT 1",params![self.view.as_slice(),self.lane.as_slice()],|r|r.get::<_,Vec<u8>>(0))?.try_into().map_err(|_|LogError::StaleCursor)?;
                let shared=Prefix::common(first,last).bits();
                let mut carry=Summary::leaf(*id);
                self.put(tx,Prefix::new(256,*id).unwrap(),carry)?;
                for bits in (0..256).rev(){
                    if bits<shared || closest.is_none_or(|depth| bits>depth) {carry=if id[usize::from(bits)/8]&(128>>(bits%8))!=0 {index::combine(bits,index::empty(),carry)}else{index::combine(bits,carry,index::empty())};continue;}
                    let p=Prefix::new(bits,*id).unwrap();let [left,right]=p.children().unwrap();let(a,b)=if id[usize::from(bits)/8]&(128>>(bits%8))!=0 {(self.summary_on(tx,left)?,carry)}else{(carry,self.summary_on(tx,right)?)};
                    carry=index::combine(bits,a,b);
                    if a.count==0 || b.count==0 {tx.execute("DELETE FROM participant_work.participant_sync_range_nodes WHERE view=?1 AND lane=?2 AND bits=?3 AND key=?4",params![self.view.as_slice(),self.lane.as_slice(),bits,p.key().as_slice()])?;}else{self.put(tx,p,carry)?;}
                }
            } Ok(())
        })?;
        self.revision = next;
        Ok(())
    }
    fn put(&self, c: &Connection, p: Prefix, s: Summary) -> Result<(), LogError> {
        c.execute("INSERT OR REPLACE INTO participant_work.participant_sync_range_nodes(view,lane,bits,key,count,digest) VALUES(?1,?2,?3,?4,?5,?6)",params![self.view.as_slice(),self.lane.as_slice(),p.bits(),p.key().as_slice(),s.count as i64,s.digest.as_slice()])?;
        Ok(())
    }
    pub(crate) fn frame(
        &self,
        scope: [u8; 32],
        view: [u8; 32],
        prefix: Prefix,
    ) -> Result<SummaryFrame, LogError> {
        self.database.read(|c|{self.validate(c)?;let summary=self.summary_on(c,prefix)?;
        let ids=if summary.count<=32 {let mut upper=prefix.key();for bit in usize::from(prefix.bits())..256 {upper[bit/8]|=128>>(bit%8);}let mut stmt=c.prepare_cached("SELECT id FROM participant_work.participant_sync_range_ids WHERE view=?1 AND lane=?2 AND id>=?3 AND id<=?4 ORDER BY id LIMIT 32")?;let rows=stmt.query_map(params![self.view.as_slice(),self.lane.as_slice(),prefix.key().as_slice(),upper.as_slice()],|r|r.get::<_,Vec<u8>>(0))?;rows.map(|r|r.map_err(LogError::from).and_then(|v|v.try_into().map_err(|_|LogError::StaleCursor))).collect::<Result<Vec<_>,_>>()?}else{vec![]};
        {let frame=SummaryFrame{scope,view,prefix,summary,singleton:(ids.len()==1).then(||ids[0]),leaves:if ids.len()>1 {ids}else{vec![]}};if !frame.valid_leaves(){return Err(LogError::StaleCursor);}Ok(frame)}
    })
    }
}
impl RangeRead for SqlIndex {
    type Error = LogError;
    fn revision(&self) -> u64 {
        self.revision
    }
    fn summary(&self, p: Prefix) -> Result<Summary, LogError> {
        self.database.read(|c| {
            self.validate(c)?;
            self.summary_on(c, p)
        })
    }
    fn contains(&self, id: &[u8; 32]) -> Result<bool, LogError> {
        self.database.read(|c|{self.validate(c)?;c.query_row("SELECT EXISTS(SELECT 1 FROM participant_work.participant_sync_range_ids WHERE view=?1 AND lane=?2 AND id=?3)",params![self.view.as_slice(),self.lane.as_slice(),id.as_slice()],|r|r.get(0)).map_err(Into::into)})
    }
}
