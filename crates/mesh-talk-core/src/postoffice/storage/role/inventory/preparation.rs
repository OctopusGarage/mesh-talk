//! One durable producer, one caller-owned active hash state, bounded raw slices.
use super::*;
#[derive(Default, Debug)]
pub(crate) struct PreparationProgress {
    pub examined: usize,
    pub hashed: usize,
    pub published: bool,
    pub ready: bool,
    pub pending: bool,
}
#[derive(Clone)]
struct HashJob {
    conversation: ConversationId,
    owner: Arc<ProfileStorage>,
    binding: Binding,
    after: Option<[u8; 32]>,
    count: u64,
    federation: Sha256,
    wire: Sha256,
}
#[derive(Default)]
pub(crate) struct DigestWorker {
    active: Option<HashJob>,
}
impl IndexedRelayState {
    pub(in super::super) fn demand_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        cutoff: Option<u64>,
    ) -> Result<bool, LogError> {
        let Some(binding) = self.digest_binding_on(c, conv, cutoff)? else {
            return Ok(true);
        };
        if self.cached_digest_on(c, conv, binding)?.is_some() {
            return Ok(true);
        }
        let present: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM relay_digest_demands WHERE conversation=?1)",
            [conv.as_bytes().as_slice()],
            |r| r.get(0),
        )?;
        if present {
            return Ok(true);
        }
        let (queued, ticket): (i64, i64) = c.query_row(
            "SELECT queued,next_ticket FROM relay_digest_preparation WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if queued >= MAX_DEMANDS {
            // Scoped probes retry their optional cache demand after capacity
            // frees. Only an explicit federation request starts the global walk.
            return Ok(false);
        }
        let following = next(ticket)?;
        c.execute(
            "INSERT INTO relay_digest_demands(conversation,ticket) VALUES(?1,?2)",
            params![conv.as_bytes().as_slice(), ticket],
        )?;
        c.execute(
            "UPDATE relay_digest_preparation SET queued=queued+1,next_ticket=?1 WHERE singleton=1",
            [following],
        )?;
        Ok(true)
    }
    pub(super) fn start_preparation_on(&self, c: &Connection) -> Result<(), LogError> {
        let phase: i64 = c.query_row(
            "SELECT phase FROM relay_digest_preparation WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        if phase != 0 {
            return Ok(());
        }
        let upper:Option<Vec<u8>>=c.query_row("SELECT conversation FROM relay_mailboxes WHERE domain=?1 ORDER BY conversation DESC LIMIT 1",[RELAY_ARRIVALS.as_slice()],|r|r.get(0)).optional()?;
        let commit: i64 = c.query_row(
            "SELECT next_commit FROM event_domains WHERE domain=?1",
            [RELAY_EVENTS.0.as_slice()],
            |r| r.get(0),
        )?;
        c.execute("UPDATE relay_digest_preparation SET generation=1,phase=1,scan_upper=?1,tail_after=?2 WHERE singleton=1",params![upper,commit])?;
        Ok(())
    }
    /// Scalar readiness only: ordinary scoped demands do not start global work.
    pub(crate) fn digest_work_pending(&self) -> Result<bool, LogError> {
        self.profile.database().read(|c| {
            Ok(c.query_row("SELECT p.queued>0 OR (p.phase<>0 AND (p.phase=1 OR p.tail_after<d.next_commit OR p.ready_generation<>p.generation)) FROM relay_digest_preparation p JOIN event_domains d ON d.domain=?1 WHERE p.singleton=1", [RELAY_EVENTS.0.as_slice()], |r| r.get(0))?)
        })
    }
    /// Called by an actual inventory request. Constructor and ordinary idle never
    /// start discovery or hash retained IDs.
    pub(crate) fn request_preparation_at(&self, now: u64) -> Result<(), LogError> {
        self.derived_transaction(now, |c, _| self.start_preparation_on(c))
    }
    pub(crate) fn preparation_ready_at(&self, now: u64) -> Result<bool, LogError> {
        self.policy_transaction(now,|c,_|{
            Ok(c.query_row("SELECT p.phase=2 AND p.queued=0 AND p.ready_generation=p.generation AND p.tail_after=d.next_commit FROM relay_digest_preparation p JOIN event_domains d ON d.domain=?1 WHERE p.singleton=1",[RELAY_EVENTS.0.as_slice()],|r|r.get(0))?)
        })
    }
    pub(super) fn produce_on(
        &self,
        c: &Connection,
        cutoff: Option<u64>,
        p: &mut PreparationProgress,
    ) -> Result<(), LogError> {
        type ProducerCursorRow = (i64, Option<Vec<u8>>, Option<Vec<u8>>, i64, i64, i64);
        let (phase,after,upper,tail,generation,ready):ProducerCursorRow=c.query_row("SELECT phase,scan_after,scan_upper,tail_after,generation,ready_generation FROM relay_digest_preparation WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?;
        if phase == 0 {
            return Ok(());
        }
        let commit: i64 = c.query_row(
            "SELECT next_commit FROM event_domains WHERE domain=?1",
            [RELAY_EVENTS.0.as_slice()],
            |r| r.get(0),
        )?;
        if phase == 2 && tail < commit && ready == generation {
            c.execute(
                "UPDATE relay_digest_preparation SET generation=?1 WHERE singleton=1",
                [next(generation)?],
            )?;
        }
        let queued: i64 = c.query_row(
            "SELECT queued FROM relay_digest_preparation WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        let limit = (MAX_DEMANDS - queued).min(PAGE as i64);
        if limit == 0 {
            return Ok(());
        }
        if phase == 1 {
            let keys = {
                let mut q=c.prepare("SELECT conversation FROM relay_mailboxes WHERE domain=?1 AND conversation>?2 AND conversation<=?3 ORDER BY conversation LIMIT ?4")?;
                q.query_map(
                    params![
                        RELAY_ARRIVALS.as_slice(),
                        after.unwrap_or_default(),
                        upper,
                        limit
                    ],
                    |r| r.get::<_, Vec<u8>>(0),
                )?
                .collect::<Result<Vec<_>, _>>()?
            };
            let mut stopped = false;
            for key in &keys {
                p.examined += 1;
                if !self.demand_on(c, &id(key.clone())?, cutoff)? {
                    stopped = true;
                    break;
                }
                c.execute(
                    "UPDATE relay_digest_preparation SET scan_after=?1 WHERE singleton=1",
                    [key],
                )?;
            }
            if !stopped && keys.len() < limit as usize {
                c.execute(
                    "UPDATE relay_digest_preparation SET phase=2 WHERE singleton=1",
                    [],
                )?;
            }
        } else {
            // Raw commit rows are bounded BEFORE conversation dedup. The covering
            // index avoids body reads and no DISTINCT can hide an unbounded scan.
            let rows = {
                let mut q=c.prepare("SELECT commit_ordinal,conversation FROM retained_events INDEXED BY retained_events_domain_commits WHERE domain=?1 AND commit_ordinal>=?2 AND commit_ordinal<?3 ORDER BY commit_ordinal LIMIT ?4")?;
                q.query_map(
                    params![RELAY_EVENTS.0.as_slice(), tail, commit, limit],
                    |r| Ok((r.get::<_, i64>(0)?, r.get::<_, Vec<u8>>(1)?)),
                )?
                .collect::<Result<Vec<_>, _>>()?
            };
            let mut stopped = false;
            let mut seen = std::collections::BTreeSet::new();
            for (ordinal, key) in &rows {
                p.examined += 1;
                if seen.insert(key.clone()) && !self.demand_on(c, &id(key.clone())?, cutoff)? {
                    stopped = true;
                    break;
                }
                c.execute(
                    "UPDATE relay_digest_preparation SET tail_after=?1 WHERE singleton=1",
                    [next(*ordinal)?],
                )?;
            }
            if !stopped && rows.len() < limit as usize {
                // Ordinal gaps from GC are safe. All retained rows before this
                // scalar were examined in this or earlier committed slices.
                c.execute("UPDATE relay_digest_preparation SET tail_after=?1 WHERE singleton=1 AND tail_after<>?1",[commit])?;
            }
        }
        Ok(())
    }
}
impl DigestWorker {
    /// Takes at most one <=512-raw producer slice and one <=512-ID hash slice.
    /// SQL/hash are synchronous; caller must pace/yield outside the mutex.
    pub(crate) fn step_at(
        &mut self,
        state: &IndexedRelayState,
        now: u64,
    ) -> Result<PreparationProgress, LogError> {
        // Work on a clone: a failed transaction/COMMIT must not advance RAM past
        // durable demand/cursor authority. One job only, no mailbox->hasher map.
        if self
            .active
            .as_ref()
            .is_some_and(|job| !Arc::ptr_eq(&job.owner, &state.profile))
        {
            return Err(LogError::StaleCursor);
        }
        let mut active = self.active.clone();
        let p=state.derived_transaction(now,|c,cutoff|{
            let mut p=PreparationProgress::default();
            state.produce_on(c,cutoff,&mut p)?;
            if let Some(job)=&active {
                let demanded:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM relay_digest_demands WHERE conversation=?1)",[job.conversation.as_bytes().as_slice()],|r|r.get(0))?;
                if !demanded || state.digest_binding_on(c,&job.conversation,cutoff)?!=Some(job.binding) {
                    if demanded {
                        let ticket:i64=c.query_row("SELECT next_ticket FROM relay_digest_preparation WHERE singleton=1",[],|r|r.get(0))?;
                        c.execute("UPDATE relay_digest_demands SET ticket=?2 WHERE conversation=?1",params![job.conversation.as_bytes().as_slice(),ticket])?;
                        c.execute("UPDATE relay_digest_preparation SET next_ticket=?1 WHERE singleton=1",[next(ticket)?])?;
                    }
                    active=None;
                }
            }
            if active.is_none(){
                let conv:Option<Vec<u8>>=c.query_row("SELECT conversation FROM relay_digest_demands ORDER BY ticket LIMIT 1",[],|r|r.get(0)).optional()?;
                if let Some(conv)=conv {
                    let conv=id(conv)?;
                    match state.digest_binding_on(c,&conv,cutoff)? {
                        Some(binding) if state.cached_digest_on(c,&conv,binding)?.is_none()=>{
                            let mut wire=Sha256::new();wire.update(binding.count.to_le_bytes());
                            active=Some(HashJob{conversation:conv,owner:state.profile.clone(),binding,after:None,count:0,federation:Sha256::new(),wire});
                        },_=>remove_demand_on(c,&conv)?,
                    }
                }
            }
            if let Some(job)=&mut active {
                let ids={let mut q=c.prepare("SELECT id FROM retained_events INDEXED BY retained_events_ids WHERE domain=?1 AND conversation=?2 AND id>?3 ORDER BY id LIMIT 512")?;
                    q.query_map(params![RELAY_EVENTS.0.as_slice(),job.conversation.as_bytes().as_slice(),job.after.as_ref().map_or(&[][..],|v|v.as_slice())],|r|r.get::<_,Vec<u8>>(0))?.collect::<Result<Vec<_>,_>>()?};
                for bytes in &ids {
                    let key:[u8;32]=bytes.as_slice().try_into().map_err(|_|invalid("invalid hashed event id"))?;
                    job.federation.update(key);job.wire.update(key);job.after=Some(key);job.count=job.count.checked_add(1).ok_or(LogError::RepositoryCounterExhausted)?;
                }
                p.hashed=ids.len();
                if ids.len()<PAGE {
                    if job.count!=job.binding.count{return Err(invalid("digest event count disagrees with repository"))}
                    // Current revision/epoch/eligibility again at publish. This
                    // also makes a busy mailbox defer rather than publish stale.
                    if state.digest_binding_on(c,&job.conversation,cutoff)?!=Some(job.binding){return Err(LogError::StaleCursor)}
                    let digest:[u8;32]=job.federation.clone().finalize().into();
                    let wire:[u8;32]=job.wire.clone().finalize().into();
                    c.execute("INSERT INTO relay_federation_digests(conversation,revision,destructive_epoch,digest,wire_fingerprint) VALUES(?1,?2,?3,?4,?5) ON CONFLICT(conversation) DO UPDATE SET revision=excluded.revision,destructive_epoch=excluded.destructive_epoch,digest=excluded.digest,wire_fingerprint=excluded.wire_fingerprint",params![job.conversation.as_bytes().as_slice(),job.binding.revision,job.binding.epoch,digest.as_slice(),wire.as_slice()])?;
                    // The existing optional probe fingerprint keeps its own wire
                    // definition, and absence still preserves its probe bypass.
                    c.execute("UPDATE event_conversations SET fingerprint_revision=?3,fingerprint=?4 WHERE domain=?1 AND conversation=?2",params![RELAY_EVENTS.0.as_slice(),job.conversation.as_bytes().as_slice(),job.binding.revision,wire.as_slice()])?;
                    remove_demand_on(c,&job.conversation)?;active=None;p.published=true;
                }
            }
            let (phase,queued,tail,generation,ready):(i64,i64,i64,i64,i64)=c.query_row("SELECT phase,queued,tail_after,generation,ready_generation FROM relay_digest_preparation WHERE singleton=1",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?)))?;
            let commit:i64=c.query_row("SELECT next_commit FROM event_domains WHERE domain=?1",[RELAY_EVENTS.0.as_slice()],|r|r.get(0))?;
            p.pending=queued>0 || (phase!=0 && (phase==1 || tail<commit));
            if phase==2 && queued==0 && tail==commit && ready!=generation {
                c.execute("UPDATE relay_digest_preparation SET ready_generation=generation WHERE singleton=1",[])?;
                p.ready=true;
            }
            Ok(p)
        })?;
        self.active = active;
        if p.ready {
            state
                .changes
                .send_modify(|generation| *generation = generation.wrapping_add(1));
        }
        Ok(p)
    }
}
