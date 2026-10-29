//! Durable event access. Participant profiles use the shared encrypted indexed
//! repository; the explicit standalone constructor preserves legacy SDK files.
//! Owned reads distinguish absence from storage errors and retain no body cache.
use super::repository::EventRepository;
use super::repository::maintenance::MaintenanceState;
use super::repository::pages::PageBudget;
use super::sync::SyncStore;
use super::{AppendOutcome, Author, ConversationId, Event, EventId, LogError};
use crate::storage::profile::context::ProfileStorage;
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    sync::Arc,
};

pub(crate) mod legacy;

enum Backend {
    Legacy(Box<legacy::LegacyEventLog>),
    Indexed(EventRepository),
}

pub struct PersistentEventLog {
    backend: Backend,
    #[cfg(test)]
    read_failure: bool,
    #[cfg(test)]
    read_failure_id: Option<EventId>,
}

impl PersistentEventLog {
    /// Standalone compatibility owner. A retired participant source contains a
    /// distinct magic and therefore cannot be opened by this legacy writer.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        Ok(Self::from_backend(Backend::Legacy(Box::new(
            legacy::LegacyEventLog::open(path, password)?,
        ))))
    }
    pub(crate) fn open_profile(
        profile: Arc<ProfileStorage>,
        password: &str,
    ) -> Result<Self, LogError> {
        let repository = profile.participant_events(password)?;
        repository.initialize_selections()?;
        Ok(Self::from_backend(Backend::Indexed(repository)))
    }
    fn from_backend(backend: Backend) -> Self {
        Self {
            backend,
            #[cfg(test)]
            read_failure: false,
            #[cfg(test)]
            read_failure_id: None,
        }
    }
    pub(crate) fn repository(&self) -> Result<&EventRepository, LogError> {
        match &self.backend {
            Backend::Indexed(repository) => Ok(repository),
            Backend::Legacy(_) => Err(LogError::AdmissionRejected(
                "indexed profile required".into(),
            )),
        }
    }
    fn check_read(&self, _id: Option<&EventId>) -> Result<(), LogError> {
        #[cfg(test)]
        if self.read_failure
            || self
                .read_failure_id
                .as_ref()
                .is_some_and(|id| Some(id) == _id)
        {
            return Err(LogError::Io(std::io::Error::other(
                "injected event read failure",
            )));
        }
        Ok(())
    }
    #[cfg(test)]
    pub(crate) fn fail_reads(&mut self, fail: bool) {
        self.read_failure = fail;
    }
    #[cfg(test)]
    pub(crate) fn fail_read_for(&mut self, id: Option<EventId>) {
        self.read_failure_id = id;
    }
    #[cfg(test)]
    pub(crate) fn before_rewrite_rename(&mut self, hook: Box<dyn FnOnce() + Send + Sync>) {
        match &mut self.backend {
            Backend::Legacy(log) => log.before_rewrite_rename(hook),
            Backend::Indexed(repository) => repository.before_mutation(hook),
        }
    }
    pub fn append(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append_with_precommit(event, || Ok(()))
    }
    pub(crate) fn append_with_precommit(
        &mut self,
        event: Event,
        precommit: impl FnOnce() -> Result<(), LogError>,
    ) -> Result<AppendOutcome, LogError> {
        match &mut self.backend {
            Backend::Legacy(log) => log.append_with_precommit(event, precommit),
            Backend::Indexed(repository) => repository.append_with_precommit(&event, precommit),
        }
    }
    pub fn sync(&self) -> Result<(), LogError> {
        match &self.backend {
            Backend::Legacy(log) => log.sync(),
            Backend::Indexed(repository) => repository.sync(),
        }
    }
    pub fn append_durable(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        match &mut self.backend {
            Backend::Legacy(log) => log.append_durable(event),
            Backend::Indexed(repository) => repository.append(&event),
        }
    }
    pub(crate) fn append_channel_batch_atomic(&mut self, events: &[Event]) -> Result<(), LogError> {
        match &mut self.backend {
            Backend::Legacy(log) => log.append_channel_batch_atomic(events),
            Backend::Indexed(repository) => repository.append_channel_batch_atomic(events),
        }
    }
    pub fn has(&self, id: &EventId) -> Result<bool, LogError> {
        self.check_read(Some(id))?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.has(id)),
            Backend::Indexed(repository) => repository.has(id),
        }
    }
    pub(crate) fn event_conversation(
        &self,
        id: &EventId,
    ) -> Result<Option<ConversationId>, LogError> {
        self.check_read(Some(id))?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.get(id)?.map(|event| event.conversation_id)),
            Backend::Indexed(repository) => repository.event_conversation(id),
        }
    }
    pub fn get(&self, id: &EventId) -> Result<Option<Event>, LogError> {
        self.check_read(Some(id))?;
        match &self.backend {
            Backend::Legacy(log) => log.get(id),
            Backend::Indexed(repository) => repository.get(id),
        }
    }
    pub fn authored_event(
        &self,
        conversation: ConversationId,
        author: Author,
        seq: u64,
    ) -> Result<Option<Event>, LogError> {
        self.check_read(None)?;
        let event = match &self.backend {
            Backend::Legacy(log) => log.authored_event(conversation, author, seq),
            Backend::Indexed(repository) => repository.authored_event(&conversation, &author, seq),
        }?;
        self.check_read(event.as_ref().map(|event| &event.id))?;
        Ok(event)
    }
    /// Explicit collecting compatibility read. Channel replay still uses this
    /// temporary owned collection; ordinary exact reads and network export page.
    pub fn events(&self, conversation: &ConversationId) -> Result<Vec<Event>, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.events(conversation).into_iter().cloned().collect()),
            Backend::Indexed(repository) => repository.collect_events(conversation),
        }
    }
    /// The projection discards each fetched body before the next page is read.
    /// Retained projection metadata remains an explicit collecting consumer.
    pub(crate) fn project_events<T>(
        &self,
        conversation: &ConversationId,
        mut project: impl FnMut(Event) -> T,
    ) -> Result<Vec<T>, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log
                .events(conversation)
                .into_iter()
                .map(|event| project(event.clone()))
                .collect()),
            Backend::Indexed(repository) => repository.project_events(conversation, project),
        }
    }
    pub fn conversations(&self) -> Result<Vec<ConversationId>, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.conversations()),
            Backend::Indexed(repository) => repository.collect_conversations(),
        }
    }
    pub fn all_event_ids(&self) -> Result<Vec<EventId>, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.all_event_ids()),
            Backend::Indexed(repository) => repository.collect_ids(),
        }
    }
    pub fn message_keys_after(
        &self,
        conversation: ConversationId,
        author: Author,
        cursor: Option<(u64, EventId)>,
        limit: usize,
    ) -> Result<Vec<(u64, EventId)>, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.message_keys_after(conversation, author, cursor, limit)),
            Backend::Indexed(repository) => {
                repository.message_keys_after(conversation, author, cursor, limit)
            }
        }
    }
    pub fn heads(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.heads(conversation)),
            Backend::Indexed(repository) => repository.heads(conversation),
        }
    }
    pub fn version_vector(
        &self,
        conversation: &ConversationId,
    ) -> Result<HashMap<Author, u64>, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.version_vector(conversation)),
            Backend::Indexed(repository) => repository.collect_author_maxima(conversation),
        }
    }
    pub fn prepare(&self, conversation: &ConversationId) -> Result<(Vec<EventId>, u64), LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => log.prepare(conversation),
            Backend::Indexed(repository) => repository.prepare(conversation),
        }
    }
    pub fn next_sequence(
        &self,
        conversation: &ConversationId,
        author: &Author,
    ) -> Result<u64, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => log.next_sequence(conversation, author),
            Backend::Indexed(repository) => repository.next_sequence(conversation, author),
        }
    }
    pub fn conversation_len(&self, conversation: &ConversationId) -> Result<usize, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.conversation_len(conversation)),
            Backend::Indexed(repository) => usize::try_from(repository.count(conversation)?)
                .map_err(|_| LogError::RepositoryCounterExhausted),
        }
    }
    pub fn requires_channel_v2(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.requires_channel_v2(conversation)),
            Backend::Indexed(repository) => Ok(repository
                .metadata(conversation)?
                .is_some_and(|m| m.channel_v2)),
        }
    }
    pub fn requires_channel_v3(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.requires_channel_v3(conversation)),
            Backend::Indexed(repository) => Ok(repository
                .metadata(conversation)?
                .is_some_and(|m| m.channel_v3)),
        }
    }
    pub fn requires_channel_v4(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.requires_channel_v4(conversation)),
            Backend::Indexed(repository) => Ok(repository
                .metadata(conversation)?
                .is_some_and(|m| m.channel_v4)),
        }
    }
    pub fn fingerprint_and_len(
        &self,
        conversation: &ConversationId,
    ) -> Result<([u8; 32], usize), LogError> {
        self.check_read(None)?;
        match &self.backend {
            Backend::Legacy(log) => Ok(log.fingerprint_and_len(conversation)),
            Backend::Indexed(repository) => {
                let (hash, count) = repository.fingerprint_and_len(conversation)?;
                Ok((
                    hash,
                    usize::try_from(count).map_err(|_| LogError::RepositoryCounterExhausted)?,
                ))
            }
        }
    }
    pub fn request_profile_compaction(
        &mut self,
        conversation: ConversationId,
    ) -> Result<(), LogError> {
        match &mut self.backend {
            Backend::Legacy(log) => {
                log.request_profile_compaction(conversation);
                Ok(())
            }
            Backend::Indexed(repository) => repository.request_profile_maintenance(&conversation),
        }
    }
    pub fn drain_one_profile_compaction(&mut self) -> Result<Option<usize>, LogError> {
        match &mut self.backend {
            Backend::Legacy(log) => log.drain_one_profile_compaction(),
            Backend::Indexed(repository) => {
                let token = repository.begin_profile_maintenance()?;
                if repository
                    .step_profile_maintenance(token, PageBudget::default())?
                    .state
                    != MaintenanceState::Ready
                {
                    return Ok(None);
                }
                let applied = repository.apply_profile_maintenance(token)?;
                Ok((applied.state == MaintenanceState::Ready).then_some(applied.dropped))
            }
        }
    }
    pub fn compact_superseded_profiles(
        &mut self,
        conversation: &ConversationId,
    ) -> Result<usize, LogError> {
        match &mut self.backend {
            Backend::Legacy(log) => log.compact_superseded_profiles(conversation),
            Backend::Indexed(repository) => {
                repository.request_profile_maintenance(conversation)?;
                let token = repository.begin_profile_maintenance()?;
                while repository
                    .step_profile_maintenance(token, PageBudget::default())?
                    .state
                    == MaintenanceState::Pending
                {}
                Ok(repository.apply_profile_maintenance(token)?.dropped)
            }
        }
    }
    pub fn drop_conversation(&mut self, conversation: &ConversationId) -> Result<usize, LogError> {
        match &mut self.backend {
            Backend::Legacy(log) => log.drop_conversation(conversation),
            Backend::Indexed(repository) => repository.drop_conversation(conversation),
        }
    }
    pub fn retain_conversations(
        &mut self,
        keep: &HashSet<ConversationId>,
    ) -> Result<usize, LogError> {
        match &mut self.backend {
            Backend::Legacy(log) => log.retain_conversations(keep),
            Backend::Indexed(repository) => repository.retain_conversations(keep),
        }
    }
}

impl SyncStore for PersistentEventLog {
    fn requires_channel_v2(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        self.requires_channel_v2(conversation)
    }
    fn requires_channel_v3(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        self.requires_channel_v3(conversation)
    }
    fn requires_channel_v4(&self, conversation: &ConversationId) -> Result<bool, LogError> {
        self.requires_channel_v4(conversation)
    }
    fn fingerprint_and_len(
        &self,
        conversation: &ConversationId,
    ) -> Result<([u8; 32], usize), LogError> {
        self.fingerprint_and_len(conversation)
    }
    fn event_ids(&self, conversation: &ConversationId) -> Result<Vec<EventId>, LogError> {
        Ok(self
            .events(conversation)?
            .into_iter()
            .map(|e| e.id)
            .collect())
    }
    fn events_excluding(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
    ) -> Result<Vec<Event>, LogError> {
        let events = self.events(conversation)?;
        Ok(super::sync::causal_events(events.iter().collect())
            .into_iter()
            .filter(|event| !have.contains(&event.id))
            .cloned()
            .collect())
    }
    fn events_excluding_bounded(
        &self,
        conversation: &ConversationId,
        have: &HashSet<EventId>,
        event_budget: usize,
        max_frames: usize,
    ) -> Result<Vec<Event>, LogError> {
        let events = self.events(conversation)?;
        Ok(super::sync::bounded_events(
            super::sync::causal_events(events.iter().collect())
                .into_iter()
                .filter(|event| !have.contains(&event.id)),
            event_budget,
            max_frames,
        ))
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.append(event)
    }
}
