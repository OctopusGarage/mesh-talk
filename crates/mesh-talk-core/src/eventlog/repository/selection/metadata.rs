//! Raw kind revisions are independent of admitted channel authority: rejected
//! competing controls still invalidate policy preparation and raw frontiers.
use super::*;
use crate::eventlog::sync::network::{PreparationBudget, PreparationStep};

pub(super) const DDL: &str = "
CREATE TABLE IF NOT EXISTS participant_kind_scan(
 domain BLOB PRIMARY KEY CHECK(length(domain)=16),ordinal INTEGER NOT NULL CHECK(ordinal>=-1)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS participant_kind_revisions(
 domain BLOB NOT NULL,conversation BLOB NOT NULL,kind INTEGER NOT NULL,ordinal INTEGER NOT NULL,
 PRIMARY KEY(domain,conversation,kind)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS participant_policy_revisions(
 domain BLOB NOT NULL,kind INTEGER NOT NULL,ordinal INTEGER NOT NULL,PRIMARY KEY(domain,kind)
) WITHOUT ROWID;
CREATE TABLE IF NOT EXISTS participant_destructive_revisions(
 domain BLOB PRIMARY KEY CHECK(length(domain)=16),
 revision INTEGER NOT NULL CHECK(typeof(revision)='integer' AND revision>=0)
) WITHOUT ROWID;
CREATE TRIGGER IF NOT EXISTS participant_policy_destructive_update
AFTER UPDATE OF destructive_epoch,lifecycle ON event_conversations
WHEN NEW.destructive_epoch<>OLD.destructive_epoch OR NEW.lifecycle<>OLD.lifecycle BEGIN
 INSERT INTO participant_destructive_revisions VALUES(NEW.domain,1)
 ON CONFLICT(domain) DO UPDATE SET revision=participant_destructive_revisions.revision+1;
END;
CREATE TRIGGER IF NOT EXISTS participant_kind_revision_insert AFTER INSERT ON retained_events BEGIN
 INSERT INTO participant_kind_revisions(domain,conversation,kind,ordinal) VALUES(NEW.domain,NEW.conversation,NEW.kind,NEW.commit_ordinal)
 ON CONFLICT(domain,conversation,kind) DO UPDATE SET ordinal=max(ordinal,excluded.ordinal);
 INSERT INTO participant_policy_revisions VALUES(NEW.domain,NEW.kind,NEW.commit_ordinal)
 ON CONFLICT(domain,kind) DO UPDATE SET ordinal=max(ordinal,excluded.ordinal);
 UPDATE participant_kind_scan SET ordinal=NEW.commit_ordinal WHERE domain=NEW.domain AND ordinal=NEW.commit_ordinal-1;
END;";
impl EventRepository {
    /// A file's authorization can depend on another conversation. The source
    /// owner advances this stamp at logical retirement or destructive rewrite,
    /// before physical deletion; ordinary appends leave finite views usable.
    pub(crate) fn destructive_policy_revision(&self) -> Result<i64, LogError> {
        self.database.read(|c| {
            let revision: Option<i64> = c
                .query_row(
                    "SELECT revision FROM participant_destructive_revisions WHERE domain=?1",
                    [self.domain.0.as_slice()],
                    |row| row.get(0),
                )
                .optional()?;
            nonnegative(revision.unwrap_or(0))
        })
    }
    pub(crate) fn prepare_kind_metadata_step(
        &self,
        budget: PreparationBudget,
    ) -> Result<PreparationStep, LogError> {
        budget.validate()?;
        let row_bytes = std::mem::size_of::<CommittedHeader>();
        let limit = budget.rows.min(budget.bytes / row_bytes);
        if limit == 0 {
            return Err(LogError::PageBudgetTooSmall {
                required: row_bytes,
            });
        }
        self.database.transaction(|c| {
            let after:i64=c.query_row("SELECT ordinal FROM participant_kind_scan WHERE domain=?1",[self.domain.0.as_slice()],|r|r.get(0))?;
            let cutoff:i64=c.query_row("SELECT next_commit FROM event_domains WHERE domain=?1",[self.domain.0.as_slice()],|r|r.get(0))?;
            let mut statement=c.prepare("SELECT commit_ordinal,conversation,kind FROM retained_events INDEXED BY retained_events_domain_commits WHERE domain=?1 AND commit_ordinal>?2 ORDER BY commit_ordinal LIMIT ?3")?;
            let rows=statement.query_map(params![self.domain.0.as_slice(),after,limit as i64],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,i64>(2)?)))?.collect::<Result<Vec<_>,_>>()?;
            for (ordinal,conversation,kind) in &rows {
                c.execute("INSERT INTO participant_kind_revisions VALUES(?1,?2,?3,?4) ON CONFLICT(domain,conversation,kind) DO UPDATE SET ordinal=max(ordinal,excluded.ordinal)",params![self.domain.0.as_slice(),conversation,kind,ordinal])?;
                c.execute("INSERT INTO participant_policy_revisions VALUES(?1,?2,?3) ON CONFLICT(domain,kind) DO UPDATE SET ordinal=max(ordinal,excluded.ordinal)",params![self.domain.0.as_slice(),kind,ordinal])?;
            }
            let next=if rows.len()<limit {cutoff-1} else {rows.last().expect("nonempty metadata page").0};
            c.execute("UPDATE participant_kind_scan SET ordinal=?2 WHERE domain=?1",params![self.domain.0.as_slice(),next])?;
            Ok(PreparationStep{ready:next>=cutoff-1,rows:rows.len(),bytes:rows.len()*row_bytes,edges:0})
        })
    }
    /// Fixed size and bodyless. A cold derived index must first advance through
    /// bounded metadata turns; it never masquerades as an empty control history.
    pub(crate) fn raw_kind_revisions(
        &self,
        conversation: ConversationId,
    ) -> Result<Option<[i64; 32]>, LogError> {
        self.database.read(|c| {
            let ready:bool=c.query_row("SELECT s.ordinal>=d.next_commit-1 FROM participant_kind_scan s JOIN event_domains d ON d.domain=s.domain WHERE s.domain=?1",[self.domain.0.as_slice()],|r|r.get(0))?;
            if !ready { return Ok(None); }
            let mut revisions=[-1;32];
            let mut stmt=c.prepare("SELECT kind,ordinal FROM participant_kind_revisions WHERE domain=?1 AND conversation=?2 ORDER BY kind")?;
            let mut rows=stmt.query(params![self.domain.0.as_slice(),conversation.as_bytes().as_slice()])?;
            while let Some(row)=rows.next()? {
                let kind=usize::try_from(row.get::<_,i64>(0)?).map_err(|_|corrupt("invalid event kind revision"))?;
                *revisions.get_mut(kind).ok_or_else(||corrupt("unsupported event kind revision"))?=nonnegative(row.get(1)?)?;
            }
            Ok(Some(revisions))
        })
    }
    pub(crate) fn raw_policy_revisions(&self) -> Result<Option<[i64; 32]>, LogError> {
        self.database.read(|c| {
            let ready:bool=c.query_row("SELECT s.ordinal>=d.next_commit-1 FROM participant_kind_scan s JOIN event_domains d ON d.domain=s.domain WHERE s.domain=?1",[self.domain.0.as_slice()],|r|r.get(0))?;
            if !ready{return Ok(None);}
            let mut revisions=[-1;32];let mut stmt=c.prepare("SELECT kind,ordinal FROM participant_policy_revisions WHERE domain=?1 ORDER BY kind")?;let mut rows=stmt.query([self.domain.0.as_slice()])?;
            while let Some(row)=rows.next()?{let kind=usize::try_from(row.get::<_,i64>(0)?).map_err(|_|corrupt("invalid policy kind revision"))?;*revisions.get_mut(kind).ok_or_else(||corrupt("unsupported policy kind revision"))?=nonnegative(row.get(1)?)?;}
            Ok(Some(revisions))
        })
    }
    /// The fixed-int Event layout belongs to the repository codec. This bounded
    /// slice is a classification hint only; a matching control is still fetched
    /// and fully authenticated by get(). No ordinary message body is decoded.
    pub(crate) fn ciphertext_prefix(
        &self,
        header: &EventHeader,
        length: usize,
    ) -> Result<Vec<u8>, LogError> {
        if length > 64 {
            return Err(LogError::InvalidPageBudget);
        }
        self.database.read(|c| {
            self.require_visible_id_on(c, &header.id)?;
            // The codec's fixed-int prefix is 140 bytes plus its parent IDs.
            // sqlite3_blob_read reads only this bounded slice; SQL substr would
            // first materialize the complete overflow BLOB in SQLite's VM.
            let offset = header
                .parent_count
                .checked_mul(32)
                .and_then(|n| n.checked_add(140))
                .ok_or(LogError::RepositoryCounterExhausted)?;
            let rowid: Option<i64> = c
                .query_row(
                    "SELECT rowid FROM retained_events WHERE domain=?1 AND id=?2",
                    params![self.domain.0.as_slice(), header.id.as_bytes().as_slice()],
                    |r| r.get(0),
                )
                .optional()?;
            let body = c.blob_open(
                "main",
                "retained_events",
                "body",
                rowid.ok_or(LogError::StaleCursor)?,
                true,
            )?;
            if body.len() != header.stored_bytes {
                return Err(corrupt("stored event length disagrees with body"));
            }
            let mut prefix = vec![0; length.min(header.ciphertext_bytes)];
            body.read_at_exact(&mut prefix, offset)?;
            Ok(prefix)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn participant_ciphertext_prefix_reads_exact_slice_after_large_parent_vectors() {
        let directory = tempfile::tempdir().unwrap();
        let repository = EventRepository::new(
            super::super::super::tests::database(&directory),
            EventDomainId([1; 16]),
        )
        .unwrap();
        let identity = DeviceIdentity::generate();
        let conversation = ConversationId::new([1; 32]);
        let mut parents = Vec::new();
        for seq in 1..=1025 {
            let event = Event::new(
                &identity,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::Message,
                vec![],
            );
            repository.append(&event).unwrap();
            parents.push(event.id);
        }
        for (index, count) in [0, 257, 1025].into_iter().enumerate() {
            let body = Event::new(
                &identity,
                conversation,
                2000 + index as u64,
                parents[..count].to_vec(),
                2000 + index as u64,
                0,
                EventKind::Message,
                (0..64).collect(),
            );
            repository.append(&body).unwrap();
            let header = repository.header(&body.id).unwrap().unwrap();
            let before = repository.body_read_counts();
            assert_eq!(
                repository.ciphertext_prefix(&header, 40).unwrap(),
                body.ciphertext[..40]
            );
            assert_eq!(
                repository.ciphertext_prefix(&header, 64).unwrap(),
                body.ciphertext
            );
            assert!(matches!(
                repository.ciphertext_prefix(&header, 65),
                Err(LogError::InvalidPageBudget)
            ));
            assert_eq!(
                repository.body_read_counts(),
                before,
                "incremental BLOB reads do not fetch/decode the full Event"
            );
            assert_eq!(
                repository.get(&body.id).unwrap().unwrap(),
                body,
                "classification never mutates the signed source"
            );
        }
    }

    #[test]
    fn participant_kind_metadata_backfill_counts_rejected_controls_without_body_reads() {
        let directory = tempfile::tempdir().unwrap();
        let repository = EventRepository::new(
            super::super::super::tests::database(&directory),
            EventDomainId([1; 16]),
        )
        .unwrap();
        let identity = DeviceIdentity::generate();
        let conversation = ConversationId::new([1; 32]);
        for seq in 1..=33 {
            let event = Event::new(
                &identity,
                conversation,
                seq,
                vec![],
                seq,
                0,
                EventKind::ChannelAccountAuthority,
                vec![255; 2048],
            );
            repository.append(&event).unwrap();
        }
        repository.initialize_selections().unwrap();
        assert!(
            repository
                .raw_kind_revisions(conversation)
                .unwrap()
                .is_none()
        );
        let before = repository.body_read_counts();
        let mut examined = 0;
        loop {
            let step = repository
                .prepare_kind_metadata_step(PreparationBudget {
                    rows: 7,
                    edges: 7,
                    bytes: 4096,
                })
                .unwrap();
            assert!(step.rows <= 7);
            examined += step.rows;
            if step.ready {
                break;
            }
        }
        assert_eq!(examined, 33);
        let stamp = repository
            .raw_kind_revisions(conversation)
            .unwrap()
            .unwrap();
        assert_eq!(
            stamp[EventKind::ChannelAccountAuthority as usize],
            32,
            "raw revisions include semantically rejected signed controls"
        );
        let ordinary = Event::new(
            &identity,
            conversation,
            34,
            vec![],
            34,
            0,
            EventKind::Message,
            vec![1],
        );
        repository.append(&ordinary).unwrap();
        assert_eq!(
            repository
                .raw_kind_revisions(conversation)
                .unwrap()
                .unwrap()[EventKind::ChannelAccountAuthority as usize],
            32
        );
        assert!(
            repository
                .prepare_kind_metadata_step(PreparationBudget::default())
                .unwrap()
                .ready
        );
        assert_eq!(repository.body_read_counts(), before);
    }
}
