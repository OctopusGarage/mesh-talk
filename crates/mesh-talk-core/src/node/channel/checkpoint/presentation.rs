//! Last completely validated presentation is separate from fresh admission.
//! Startup may display this current metadata while signed control work resumes.
use super::*;
use bincode::Options;
use serde::{Deserialize, Serialize};
mod preparation;
pub(super) use preparation::PolicyCandidate;

const SCHEMA: &str = "
CREATE TABLE channel_book_metadata(
 channel BLOB PRIMARY KEY CHECK(length(channel)=32),owner_account BLOB NOT NULL CHECK(length(owner_account)=32),generation INTEGER NOT NULL,
 job BLOB NOT NULL CHECK(length(job)=16),stamp INTEGER NOT NULL,replica_stamp INTEGER NOT NULL,
 state BLOB NOT NULL,presentation BLOB NOT NULL
) WITHOUT ROWID;
";

#[derive(Serialize, Deserialize)]
pub(super) struct Presentation {
    pub(super) policy: crate::channel::HistoryPolicy,
    pub(super) preferences: Vec<(String, u64, EventId, bool)>,
    pub(super) departed: bool,
    pub(super) transferred: bool,
}
pub(super) fn initialize(c: &Connection) -> Result<(), LogError> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='channel_book_metadata')",
        [],
        |r| r.get(0),
    )?;
    if !present {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)?;
    preparation::initialize(c)
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, LogError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialize(value)
        .map_err(|e| invalid(&e.to_string()))
}
fn decode(bytes: &[u8]) -> Result<Presentation, LogError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| invalid(&e.to_string()))
}

impl ChannelCheckpointStore {
    /// Avoid recreating completed metadata demand. This is only a scheduling
    /// hint: callers still run canonical, presentation and plaintext admission.
    /// Existing unfinished demand belongs to its original receiver turn.
    pub(in crate::node) fn queue_metadata_runtime(
        &self,
        channel: ConversationId,
    ) -> Result<(), LogError> {
        let queued = self.profile.database().read(|c| {
            self.check_account_on(c)?;
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM channel_runtime_work WHERE channel=?1)",
                [channel.as_bytes().as_slice()],
                |r| r.get::<_, bool>(0),
            )
            .map_err(Into::into)
        })?;
        if queued {
            return Ok(());
        }
        // A complete current projection can prove the local device never joined.
        // That path intentionally has no presentation. Reuse only its exact
        // account/source/count qualification; this conveys no channel admission.
        let prepared = (|| -> Result<bool, LogError> {
            let Some(source) = self.source_binding(channel)? else {
                return Ok(false);
            };
            let (stamp, replica) = self.presentation_stamps(&source)?;
            self.profile.database().read(|c| {
                self.check_source_on(c, &source)?;
                c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM channel_current p JOIN event_conversations e ON e.domain=?1 AND e.conversation=p.channel LEFT JOIN channel_book_metadata m ON m.channel=p.channel WHERE p.channel=?2 AND p.owner_account=?3 AND p.source_job=?4 AND p.source_revision=?5 AND p.next_rank=e.event_count AND (p.known_local=0 OR (m.owner_account=?3 AND m.generation=?6 AND m.job=?4 AND m.stamp=?7 AND m.replica_stamp=?8)))",
                    params![b"MeshPartEventsV1".as_slice(),channel.as_bytes().as_slice(),self.account.as_slice(),source.job.as_slice(),source.revision,self.generation,stamp,replica],
                    |r| r.get(0),
                ).map_err(Into::into)
            })
        })();
        if self.profile.database().work_observation_enabled()
            && matches!(&prepared, Ok(false) | Err(LogError::StaleCursor))
        {
            use crate::storage::database::ReadinessReason as Reason;
            let mut mask = if matches!(&prepared, Err(LogError::StaleCursor)) {
                Reason::Stale.mask()
            } else {
                0
            };
            // Best-effort explanation only; failure cannot affect the original
            // scheduling decision or turn this observation into admission.
            mask |= self
                .metadata_readiness_mask(channel)
                .unwrap_or_else(|_| Reason::Unavailable.mask());
            self.profile.database().observe_readiness(mask);
        }
        match prepared {
            Ok(true) => Ok(()),
            Ok(false) | Err(LogError::StaleCursor) => self.queue_runtime(channel),
            Err(error) => Err(error),
        }
    }
    fn metadata_readiness_mask(&self, channel: ConversationId) -> Result<u16, LogError> {
        use crate::storage::database::ReadinessReason as Reason;
        let base = self.profile.database().read(|c| {
            self.check_account_on(c)?;
            c.query_row(
                "SELECT
                NOT EXISTS(SELECT 1 FROM channel_current WHERE channel=?2),
                EXISTS(SELECT 1 FROM channel_current WHERE channel=?2 AND owner_account<>?3),
                EXISTS(SELECT 1 FROM channel_current p JOIN event_order_current o ON o.domain=?1 AND o.conversation=p.channel AND o.order_kind=0 WHERE p.channel=?2 AND p.source_job<>o.job),
                EXISTS(SELECT 1 FROM channel_current p JOIN event_order_current o ON o.domain=?1 AND o.conversation=p.channel AND o.order_kind=0 WHERE p.channel=?2 AND p.source_revision<>o.source_revision),
                EXISTS(SELECT 1 FROM channel_current p JOIN event_conversations e ON e.domain=?1 AND e.conversation=p.channel WHERE p.channel=?2 AND p.next_rank<>e.event_count),
                NOT EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?2),
                EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?2 AND owner_account<>?3),
                EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?2 AND generation<>?4),
                EXISTS(SELECT 1 FROM channel_book_metadata m JOIN event_order_current o ON o.domain=?1 AND o.conversation=m.channel AND o.order_kind=0 WHERE m.channel=?2 AND m.job<>o.job),
                NOT EXISTS(SELECT 1 FROM event_conversations WHERE domain=?1 AND conversation=?2)",
                params![b"MeshPartEventsV1".as_slice(),channel.as_bytes().as_slice(),self.account.as_slice(),self.generation],
                |r| {
                    let reasons = [Reason::CurrentAbsent, Reason::CurrentAccount, Reason::CurrentJob,
                        Reason::CurrentRevision, Reason::CurrentRank, Reason::MetadataAbsent,
                        Reason::MetadataAccount, Reason::MetadataGeneration, Reason::MetadataJob,
                        Reason::ConversationAbsent];
                    let mut mask = 0;
                    for (index, reason) in reasons.into_iter().enumerate() {
                        if r.get::<_, bool>(index)? { mask |= reason.mask(); }
                    }
                    Ok(mask)
                },
            ).map_err(Into::into)
        })?;
        let source = match self.source_binding(channel) {
            Ok(Some(source)) => source,
            Ok(None) => return Ok(base | Reason::SourceAbsent.mask()),
            Err(LogError::StaleCursor) => return Ok(base | Reason::Stale.mask()),
            Err(_) => return Ok(base | Reason::Unavailable.mask()),
        };
        let (stamp, replica) = match self.presentation_stamps(&source) {
            Ok(stamps) => stamps,
            Err(LogError::StaleCursor) => return Ok(base | Reason::Stale.mask()),
            Err(_) => return Ok(base | Reason::Unavailable.mask()),
        };
        self.profile.database().read(|c| {
            self.check_account_on(c)?;
            self.check_source_on(c, &source)?;
            let mask = c.query_row(
                "SELECT
                NOT EXISTS(SELECT 1 FROM channel_current WHERE channel=?2),
                EXISTS(SELECT 1 FROM channel_current WHERE channel=?2 AND owner_account<>?3),
                EXISTS(SELECT 1 FROM channel_current WHERE channel=?2 AND source_job<>?4),
                EXISTS(SELECT 1 FROM channel_current WHERE channel=?2 AND source_revision<>?5),
                EXISTS(SELECT 1 FROM channel_current p JOIN event_conversations e ON e.domain=?1 AND e.conversation=p.channel WHERE p.channel=?2 AND p.next_rank<>e.event_count),
                NOT EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?2),
                EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?2 AND owner_account<>?3),
                EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?2 AND generation<>?6),
                EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?2 AND job<>?4),
                EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?2 AND stamp<>?7),
                EXISTS(SELECT 1 FROM channel_book_metadata WHERE channel=?2 AND replica_stamp<>?8),
                NOT EXISTS(SELECT 1 FROM event_conversations WHERE domain=?1 AND conversation=?2)",
                params![b"MeshPartEventsV1".as_slice(),channel.as_bytes().as_slice(),self.account.as_slice(),source.job.as_slice(),source.revision,self.generation,stamp,replica],
                |r| {
                    let reasons = [Reason::CurrentAbsent, Reason::CurrentAccount, Reason::CurrentJob,
                        Reason::CurrentRevision, Reason::CurrentRank, Reason::MetadataAbsent,
                        Reason::MetadataAccount, Reason::MetadataGeneration, Reason::MetadataJob,
                        Reason::MetadataStamp, Reason::MetadataReplica, Reason::ConversationAbsent];
                    let mut mask = 0;
                    for (index, reason) in reasons.into_iter().enumerate() {
                        if r.get::<_, bool>(index)? { mask |= reason.mask(); }
                    }
                    Ok(mask)
                },
            )?;
            // The source can change between the original decision and this
            // diagnostic snapshot. An all-matching snapshot is not a repair.
            Ok(if mask | base == 0 { Reason::PredicateMoved.mask() } else { mask | base })
        })
    }
    pub(in crate::node) fn prepared_history_policy(
        &self,
        channel: ConversationId,
    ) -> Result<crate::channel::HistoryPolicy, LogError> {
        let source = self.source_binding(channel)?.ok_or(LogError::StaleCursor)?;
        let stamp = self.control_stamp(&source)?;
        self.profile.database().read(|c| {
            self.check_source_on(c, &source)?;
            let bytes: Option<Vec<u8>> = c.query_row("SELECT policy FROM channel_history_policy_cache WHERE job=?1 AND channel=?2 AND control_stamp=?3", params![source.job.as_slice(),channel.as_bytes().as_slice(),stamp], |r| r.get(0)).optional()?;
            bytes.as_deref().and_then(crate::node::join_history::decode_policy).ok_or(LogError::StaleCursor)
        })
    }
    fn presentation_stamps(&self, source: &ChannelSource) -> Result<(i64, i64), LogError> {
        let control = self.control_stamp(source)?;
        let replica=self.profile.database().read(|c| {
            self.check_source_on(c,source)?;
            c.query_row("SELECT COALESCE((SELECT rank FROM channel_event_authority INDEXED BY channel_authority_kind WHERE job=?1 AND channel=?2 AND kind=?3 ORDER BY rank DESC LIMIT 1),-1)",params![source.job.as_slice(),source.channel.as_bytes().as_slice(),crate::eventlog::EventKind::ChannelReplica as i64],|r|r.get(0)).map_err(Into::into)
        })?;
        Ok((control, replica))
    }
    pub(super) fn save_presentation(
        &self,
        source: &ChannelSource,
        state: &ChannelState,
        presentation: &Presentation,
    ) -> Result<(), LogError> {
        let state = state
            .encode_current()
            .map_err(|e| invalid(&e.to_string()))?;
        let encoded = encode(presentation)?;
        let (stamp, replica) = self.presentation_stamps(source)?;
        self.profile.database().transaction(|c| {
            self.check_source_on(c,source)?;
            c.execute("INSERT INTO channel_book_metadata VALUES(?1,?2,?3,?4,?5,?6,?7,?8) ON CONFLICT(channel) DO UPDATE SET owner_account=excluded.owner_account,generation=excluded.generation,job=excluded.job,stamp=excluded.stamp,replica_stamp=excluded.replica_stamp,state=excluded.state,presentation=excluded.presentation",
                params![source.channel.as_bytes().as_slice(),self.account.as_slice(),self.generation,source.job.as_slice(),stamp,replica,state,encoded])?;
            c.execute("DELETE FROM channel_book_work WHERE channel=?1",[source.channel.as_bytes().as_slice()])?;
            c.execute("INSERT INTO channel_history_policy_cache VALUES(?1,?2,?3,?4) ON CONFLICT(job,channel) DO UPDATE SET control_stamp=excluded.control_stamp,policy=excluded.policy",params![source.job.as_slice(),source.channel.as_bytes().as_slice(),stamp,crate::node::join_history::encode_policy(presentation.policy)])?;
            Ok(())
        })
    }
    /// This is presentation only. Fresh source checks continue to own mutation,
    /// sync and plaintext admission. No current metadata default grants access.
    pub(in crate::node) fn restore_presentation_page(
        &self,
        book: &mut super::super::ChannelBook,
        own: &str,
        after: Option<ConversationId>,
    ) -> Result<Option<ConversationId>, LogError> {
        let row=self.profile.database().read(|c| {
            self.check_account_on(c)?;
            c.query_row("SELECT m.channel,m.state,m.presentation FROM channel_book_metadata m JOIN event_conversations e ON e.domain=?1 AND e.conversation=m.channel AND e.lifecycle=0 WHERE m.owner_account=?2 AND m.generation=?3 AND (?4 IS NULL OR m.channel>?4) ORDER BY m.channel LIMIT 1",params![b"MeshPartEventsV1".as_slice(),self.account.as_slice(),self.generation,after.as_ref().map(|id|id.as_bytes().as_slice())],|r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,Vec<u8>>(1)?,r.get::<_,Vec<u8>>(2)?))).optional().map_err(Into::into)
        })?;
        let Some((channel, state, presentation)) = row else {
            return Ok(None);
        };
        let channel = ConversationId::new(
            channel
                .try_into()
                .map_err(|_| invalid("invalid presentation channel"))?,
        );
        let state = ChannelState::decode_current(&state).map_err(|e| invalid(&e.to_string()))?;
        if state.id() != channel {
            return Err(invalid("presentation identity mismatch"));
        }
        install(book, own, state, decode(&presentation)?);
        Ok(Some(channel))
    }
    pub(in crate::node) fn install_prepared_presentation(
        &self,
        book: &mut super::super::ChannelBook,
        own: &str,
        channel: ConversationId,
    ) -> Result<bool, LogError> {
        let Some(source) = self.source_binding(channel)? else {
            return Ok(false);
        };
        let (stamp, replica) = self.presentation_stamps(&source)?;
        let row=self.profile.database().read(|c| {
            self.check_source_on(c,&source)?;
            c.query_row("SELECT state,presentation FROM channel_book_metadata WHERE channel=?1 AND owner_account=?2 AND generation=?3 AND job=?4 AND stamp=?5 AND replica_stamp=?6",params![channel.as_bytes().as_slice(),self.account.as_slice(),self.generation,source.job.as_slice(),stamp,replica],|r|Ok((r.get::<_,Vec<u8>>(0)?,r.get::<_,Vec<u8>>(1)?))).optional().map_err(Into::into)
        })?;
        let Some((state, presentation)) = row else {
            return Ok(false);
        };
        let state = ChannelState::decode_current(&state).map_err(|e| invalid(&e.to_string()))?;
        if state.id() != channel {
            return Err(invalid("presentation identity mismatch"));
        }
        install(book, own, state, decode(&presentation)?);
        Ok(true)
    }
}
pub(super) fn install(
    book: &mut super::super::ChannelBook,
    own: &str,
    state: ChannelState,
    presentation: Presentation,
) {
    let channel = state.id();
    book.install_indexed_state(state);
    book.history_policies.insert(channel, presentation.policy);
    book.replica_preferences.retain(|(id, _), _| *id != channel);
    for (member, lamport, event, enabled) in presentation.preferences {
        book.replica_preferences
            .insert((channel, member), (lamport, event, enabled));
    }
    book.departed.retain(|(id, _)| *id != channel);
    book.transferred.retain(|(id, _)| *id != channel);
    if presentation.departed {
        book.departed.insert((channel, own.to_owned()));
    }
    if presentation.transferred {
        book.transferred.insert((channel, own.to_owned()));
    }
}

#[cfg(test)]
mod readiness_tests {
    use super::*;
    use crate::eventlog::sync::network::PreparationBudget;
    use crate::storage::database::{ReadinessReason as Reason, with_work_observation_for_test};
    fn prepare(fixture: &super::super::tests::Fixture) {
        fixture.project();
        for _ in 0..128 {
            if fixture
                .store
                .prepare_presentation_step(
                    fixture.log.repository().unwrap(),
                    &fixture.me,
                    fixture.channel,
                    PreparationBudget::default(),
                )
                .unwrap()
                .ready
            {
                return;
            }
        }
        panic!("presentation fixture did not complete");
    }

    #[test]
    fn readiness_diagnostics_preserve_real_queue_decisions_with_and_without_observation() {
        for enabled in [false, true] {
            let fixture =
                with_work_observation_for_test(enabled, || super::super::tests::Fixture::new(0));
            prepare(&fixture);
            let store = &fixture.store;
            let db = store.profile.database();
            store.finish_runtime(fixture.channel, None).unwrap();
            db.transaction(|c| {
                c.execute(
                    "DELETE FROM channel_book_metadata WHERE channel=?1",
                    [fixture.channel.as_bytes().as_slice()],
                )?;
                Ok(())
            })
            .unwrap();
            let mask = store.metadata_readiness_mask(fixture.channel).unwrap();
            assert_ne!(mask & Reason::MetadataAbsent.mask(), 0);
            assert_eq!(mask & Reason::CurrentAbsent.mask(), 0);
            let before = db.read(|c| Ok(c.total_changes())).unwrap();
            store.queue_metadata_runtime(fixture.channel).unwrap();
            assert_eq!(db.read(|c| Ok(c.total_changes())).unwrap(), before + 1);
            assert!(store.next_runtime(None).unwrap().is_some());
            let counts = db.readiness_counts_for_test();
            assert_eq!(counts.is_some(), enabled);
            if let Some(counts) = counts {
                assert!(counts[Reason::MetadataAbsent as usize] > 0);
            }
            store.finish_runtime(fixture.channel, None).unwrap();
        }
    }

    #[test]
    fn readiness_masks_distinguish_current_and_metadata_mismatch_fields() {
        let fixture = super::super::tests::Fixture::new(0);
        prepare(&fixture);
        let store = &fixture.store;
        let db = store.profile.database();
        db.transaction(|c| {
            c.execute("UPDATE channel_current SET next_rank=next_rank+1 WHERE channel=?1", params![fixture.channel.as_bytes().as_slice()])?;
            c.execute("UPDATE channel_book_metadata SET owner_account=?2,generation=generation+1,job=?3,stamp=stamp+1,replica_stamp=replica_stamp+1 WHERE channel=?1", params![fixture.channel.as_bytes().as_slice(),[0xffu8;32].as_slice(),[0xffu8;16].as_slice()])?;
            Ok(())
        }).unwrap();
        let mask = store.metadata_readiness_mask(fixture.channel).unwrap();
        for reason in [
            Reason::CurrentRank,
            Reason::MetadataAccount,
            Reason::MetadataGeneration,
            Reason::MetadataJob,
            Reason::MetadataStamp,
            Reason::MetadataReplica,
        ] {
            assert_ne!(mask & reason.mask(), 0);
        }
        assert_eq!(
            mask & (Reason::CurrentAbsent.mask() | Reason::MetadataAbsent.mask()),
            0
        );
        db.transaction(|c| {
            c.execute("UPDATE channel_current SET owner_account=?2,source_job=?3,source_revision=source_revision+1 WHERE channel=?1", params![fixture.channel.as_bytes().as_slice(),[0xffu8;32].as_slice(),[0xffu8;16].as_slice()])?;
            Ok(())
        }).unwrap();
        let mask = store.metadata_readiness_mask(fixture.channel).unwrap();
        for reason in [
            Reason::CurrentAccount,
            Reason::CurrentJob,
            Reason::CurrentRevision,
            Reason::SourceAbsent,
        ] {
            assert_ne!(mask & reason.mask(), 0);
        }
    }
}
