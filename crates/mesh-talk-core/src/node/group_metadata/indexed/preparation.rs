//! One retained continuation per selected metadata control. Authority remains in
//! IndexedView/checkpoints; this owner only schedules its existing decisions.
use super::*;
use crate::{
    channel::ChannelState,
    eventlog::repository::{AncestorSelection, order::OrderWorkBudget},
    node::{
        channel::checkpoint::causal::{CausalProjectionQuery, CausalProjectionStep},
        join_history::indexed::{OwnerAuthorizationQuery, OwnerAuthorizationStep},
    },
};

const CAPACITY: usize = 64;

#[derive(Default)]
pub(in crate::node::group_metadata) struct PreparationCache {
    active: Vec<SelectedControl>,
}
struct SelectedControl {
    source: ChannelSource,
    job: [u8; 16],
    rank: i64,
    header: EventHeader,
    control: Control,
    phase: Phase,
}
enum Phase {
    Publication(Box<OwnerAuthorizationQuery>),
    ReleaseAncestry(Box<AncestorSelection>),
    ReleaseCausal(Box<CausalProjectionQuery>),
    Canonical(Option<Box<ChannelState>>),
    Done,
}
enum Verdict {
    Reject,
    Publication(PublicIdentity, String),
    Release(PublicIdentity),
}

impl PreparationCache {
    fn prune_stale(&mut self, index: &Index<'_>, budget: &mut Budget) -> Result<(), LogError> {
        let mut position = 0;
        while position < self.active.len() && budget.row() {
            let item = &self.active[position];
            let current = index.node.channel_checkpoints.profile().database().read(|c| {
                match index.node.channel_checkpoints.check_source_on(c, &item.source) {
                    Ok(()) => {}
                    Err(LogError::StaleCursor) => return Ok(false),
                    Err(error) => return Err(error),
                }
                c.query_row("SELECT EXISTS(SELECT 1 FROM group_metadata_index WHERE channel=?1 AND job=?2)",
                    params![item.source.channel.as_bytes().as_slice(), item.job.as_slice()], |r| r.get(0)).map_err(Into::into)
            })?;
            if current {
                position += 1;
            } else {
                self.active.remove(position);
            }
        }
        Ok(())
    }
    pub(super) fn advance(
        &mut self,
        index: &Index<'_>,
        budget: &mut Budget,
    ) -> Result<bool, LogError> {
        // A source change invalidates a continuation, never its durable cursor.
        // Dropping queries retires their disposable SQL work through their owners.
        self.active.retain(|item| {
            item.source.channel != index.source.channel
                || (item.job == index.job
                    && item.source.job == index.source.job
                    && item.source.revision == index.source.revision)
        });
        loop {
            if budget.exhausted() {
                return Ok(false);
            }
            if let Some(position) = self
                .active
                .iter()
                .position(|item| item.source.channel == index.source.channel)
            {
                let mut selected = self.active.remove(position);
                let verdict = selected.step(index, budget)?;
                if let Some(verdict) = verdict {
                    selected.commit(index, verdict)?;
                } else {
                    self.active.push(selected);
                }
                continue;
            }
            let after: i64 = index.read(|c| {
                c.query_row(
                    "SELECT after_rank FROM group_metadata_index WHERE channel=?1",
                    [index.source.channel.as_bytes().as_slice()],
                    |r| r.get(0),
                )
                .map_err(Into::into)
            })?;
            if after >= index.stamp.metadata {
                return Ok(true);
            }
            if self.active.len() == CAPACITY {
                self.prune_stale(index, budget)?;
            }
            if self.active.len() == CAPACITY {
                // Do not evict another channel's progress: a busy UI or worker
                // must exert backpressure rather than restart all 65 queries.
                return Err(LogError::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "metadata preparation capacity reached",
                )));
            }
            let next: Option<(i64, Vec<u8>)> = index.read(|c| c.query_row(
                "SELECT rank,event FROM channel_event_authority WHERE job=?1 AND channel=?2 AND kind=?3 AND rank>?4 ORDER BY rank LIMIT 1",
                params![index.source.job.as_slice(), index.source.channel.as_bytes().as_slice(), EventKind::ChannelMetadata as i64, after],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional().map_err(Into::into))?;
            let Some((rank, id)) = next else {
                return Ok(false);
            };
            let id = event_id(id)?;
            if !budget.row() {
                return Ok(false);
            }
            let header = index.repository.header(&id)?.ok_or(LogError::StaleCursor)?;
            let control = if header.ciphertext_bytes <= MAX_CONTROL {
                let Some(event) = budget.load(index.repository, id)? else {
                    return Ok(false);
                };
                decode(&event)
            } else {
                None
            };
            let prepared = match control {
                Some(control) => SelectedControl::start(index, rank, header, control)?,
                None => None,
            };
            if let Some(selected) = prepared {
                self.active.push(selected);
            } else {
                index.write(|c| {
                    c.execute(
                        "UPDATE group_metadata_index SET after_rank=?2 WHERE channel=?1",
                        params![index.source.channel.as_bytes().as_slice(), rank],
                    )?;
                    Ok(())
                })?;
            }
        }
    }
}
impl SelectedControl {
    fn start(
        index: &Index<'_>,
        rank: i64,
        header: EventHeader,
        control: Control,
    ) -> Result<Option<Self>, LogError> {
        let phase = match &control {
            Control::Publish {
                keys, ciphertext, ..
            } => {
                if keys.sender != header.author.user_id()
                    || keys.entries.is_empty()
                    || keys.entries.len() > MAX_RECIPIENTS
                    || ciphertext.len() > MAX_COVER + 128
                {
                    return Ok(None);
                }
                Phase::Publication(Box::new(
                    index.authority().start_owner_authorization(&header)?,
                ))
            }
            Control::Release { publication, .. } => {
                if !index.release_is_current(&header, *publication)? {
                    return Ok(None);
                }
                let mut selection = index
                    .repository
                    .begin_ancestor_selection(index.source.channel)?
                    .ok_or(LogError::StaleCursor)?;
                if !index
                    .repository
                    .seed_ancestor_selection(&mut selection, &[header.id], true, true)?
                    .0
                {
                    return Err(LogError::StaleCursor);
                }
                Phase::ReleaseAncestry(Box::new(selection))
            }
        };
        Ok(Some(Self {
            source: index.source,
            job: index.job,
            rank,
            header,
            control,
            phase,
        }))
    }
    fn step(
        &mut self,
        index: &Index<'_>,
        budget: &mut Budget,
    ) -> Result<Option<Verdict>, LogError> {
        index.check_source()?;
        let limit = budget.preparation();
        match std::mem::replace(&mut self.phase, Phase::Done) {
            Phase::Publication(mut query) => {
                match index.authority().owner_authorized_step(&mut query, limit)? {
                    OwnerAuthorizationStep::Pending(work) => {
                        budget.charge(work)?;
                        self.phase = Phase::Publication(query);
                    }
                    OwnerAuthorizationStep::Ready {
                        authorized,
                        before,
                        work,
                    } => {
                        budget.charge(work)?;
                        if !authorized {
                            return Ok(Some(Verdict::Reject));
                        }
                        self.phase = Phase::Canonical(before);
                    }
                }
            }
            Phase::ReleaseAncestry(selection) => {
                let work = index.repository.ancestor_selection_step_with_budget(
                    &selection,
                    OrderWorkBudget {
                        vertices: limit.rows,
                        edges: limit.edges,
                        bytes: limit.bytes,
                    },
                )?;
                budget.charge(PreparationStep {
                    rows: work.vertices,
                    edges: work.edges,
                    bytes: work.charged_bytes,
                    ready: work.complete,
                })?;
                if !work.complete {
                    self.phase = Phase::ReleaseAncestry(selection);
                } else {
                    let Control::Release { publication, .. } = &self.control else {
                        return Err(LogError::StaleCursor);
                    };
                    if !index
                        .repository
                        .ancestor_selection_contains(&selection, *publication)?
                    {
                        return Ok(Some(Verdict::Reject));
                    }
                    self.phase = Phase::ReleaseCausal(Box::new(
                        index.node.channel_checkpoints.start_causal_projection(
                            index.source.channel,
                            &[self.header.id],
                            false,
                        )?,
                    ));
                }
            }
            Phase::ReleaseCausal(mut query) => {
                match index.node.channel_checkpoints.causal_projection_step(
                    index.repository,
                    &mut query,
                    limit,
                )? {
                    CausalProjectionStep::Pending(work) => {
                        budget.charge(work)?;
                        self.phase = Phase::ReleaseCausal(query);
                    }
                    CausalProjectionStep::Ready { projection, work } => {
                        budget.charge(work)?;
                        self.phase =
                            Phase::Canonical(projection.and_then(|p| p.state).map(Box::new));
                    }
                }
            }
            Phase::Canonical(before) => {
                let Some(before) = before else {
                    return Ok(Some(Verdict::Reject));
                };
                let (canonical, bytes) = index.node.channel_checkpoints.canonical_state_budgeted(
                    index.source.channel,
                    self.header.id,
                    false,
                    limit.bytes,
                )?;
                budget.charge(PreparationStep {
                    rows: 1,
                    bytes,
                    ..Default::default()
                })?;
                let Some(canonical) = canonical else {
                    return Ok(Some(Verdict::Reject));
                };
                return Ok(Some(self.validate(&before, &canonical)));
            }
            Phase::Done => return Err(LogError::StaleCursor),
        }
        Ok(None)
    }
    fn validate(&self, before: &ChannelState, canonical: &ChannelState) -> Verdict {
        let author_id = self.header.author.user_id();
        let Some(author) = before
            .members()
            .iter()
            .find(|m| m.user_id() == author_id)
            .cloned()
        else {
            return Verdict::Reject;
        };
        match &self.control {
            Control::Publish { keys, .. } => {
                let mut recipients = HashSet::new();
                if keys.entries.iter().any(|entry| {
                    !before.is_accepted_member(&entry.user_id)
                        || !canonical.is_accepted_member(&entry.user_id)
                        || entry.sealed.is_empty()
                        || entry.sealed.len() > 256
                        || !recipients.insert(&entry.user_id)
                }) {
                    return Verdict::Reject;
                }
                let account = before
                    .member_account_id(&author_id)
                    .map(str::to_owned)
                    .unwrap_or(author_id);
                Verdict::Publication(author, account)
            }
            Control::Release { recipient, .. } => {
                if [before, canonical].iter().any(|s| {
                    s.is_dissolved()
                        || !s.is_accepted_member(&author_id)
                        || !s.is_accepted_member(&recipient.user_id())
                        || !s.members().contains(recipient)
                }) {
                    return Verdict::Reject;
                }
                Verdict::Release(author)
            }
        }
    }
    fn commit(&self, index: &Index<'_>, verdict: Verdict) -> Result<(), LogError> {
        index.write(|c| {
            match (&self.control, verdict) {
                (Control::Publish { field, .. }, Verdict::Publication(author, account)) => {
                    c.execute("INSERT OR IGNORE INTO group_metadata_publications VALUES(?1,?2,?3,?4,?5,?6,?7)",
                        params![self.job.as_slice(), self.header.id.as_bytes().as_slice(), self.rank,
                        self.header.lamport.to_be_bytes().as_slice(), *field as i64, identity_key(&author).as_slice(), account])?;
                }
                (Control::Release { publication, recipient, .. }, Verdict::Release(author)) => {
                    c.execute("INSERT OR IGNORE INTO group_metadata_releases VALUES(?1,?2,?3,?4,?5,?6)",
                        params![self.job.as_slice(), self.header.id.as_bytes().as_slice(), publication.as_bytes().as_slice(),
                        identity_key(recipient).as_slice(), identity_key(&author).as_slice(), self.rank])?;
                }
                (_, Verdict::Reject) => {}
                _ => return Err(invalid("metadata preparation/control mismatch")),
            }
            c.execute("UPDATE group_metadata_index SET after_rank=?2 WHERE channel=?1",
                params![self.source.channel.as_bytes().as_slice(), self.rank])?;
            Ok(())
        })
    }
}
