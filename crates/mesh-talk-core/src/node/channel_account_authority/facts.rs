//! Historical authority decisions are exact indexed facts, separate from the
//! bounded current role snapshot. Candidate changes publish with its replay
//! cursor; a failed transaction cannot partially install authority.
use super::*;
use crate::{eventlog::LogError, storage::profile::context::ProfileStorage};
use bincode::Options;
use rusqlite::{Connection, OptionalExtension, params};
use std::sync::Arc;

const CONTROL: i64 = 1;
const REVOKED: i64 = 2;
const ADDED: i64 = 3;
const PIN: i64 = 4;
const ORIGINAL: i64 = 5;
const ORIGIN: i64 = 6;
const KNOWN_PUBLIC: i64 = 7;
const LEGACY_SEEN: i64 = 8;
const ACCEPTANCE: i64 = 9;
const SCHEMA: &str = "
CREATE TABLE channel_authority_facts(
 job BLOB NOT NULL CHECK(length(job)=16),channel BLOB NOT NULL CHECK(length(channel)=32),
 kind INTEGER NOT NULL,key BLOB NOT NULL,account BLOB,value BLOB NOT NULL,
 PRIMARY KEY(job,channel,kind,key)
) WITHOUT ROWID;
CREATE INDEX channel_authority_account ON channel_authority_facts(job,channel,kind,account,key);
CREATE TABLE channel_authority_current(
 job BLOB NOT NULL CHECK(length(job)=16), channel BLOB NOT NULL CHECK(length(channel)=32),
 state BLOB NOT NULL, PRIMARY KEY(job,channel)
) WITHOUT ROWID;
CREATE TABLE channel_legacy_candidates(
 job BLOB NOT NULL,channel BLOB NOT NULL,event BLOB NOT NULL,lifetime BLOB NOT NULL,member BLOB NOT NULL,
 PRIMARY KEY(job,channel,event)
) WITHOUT ROWID;
CREATE INDEX channel_legacy_lifetime ON channel_legacy_candidates(job,channel,lifetime,event);
CREATE TABLE channel_legacy_scan(job BLOB NOT NULL,channel BLOB NOT NULL,next_rank INTEGER NOT NULL,rebuilding INTEGER NOT NULL CHECK(rebuilding IN(0,1)),PRIMARY KEY(job,channel)) WITHOUT ROWID;";
fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, LogError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .serialize(value)
        .map_err(|e| invalid(&e.to_string()))
}
fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, LogError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(bytes.len() as u64)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| invalid(&e.to_string()))
}
pub(in crate::node) struct AuthorityHistory {
    profile: Arc<ProfileStorage>,
    job: [u8; 16],
    channel: ConversationId,
    source_revision: i64,
    source_job: [u8; 16],
    _query_lease: Option<Arc<crate::node::channel::checkpoint::causal::Query>>,
}
impl AuthorityHistory {
    pub(in crate::node) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
        if fresh {
            c.execute_batch(SCHEMA)?;
        }
        crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
    }
    #[cfg(test)]
    pub(in crate::node) fn load(
        profile: Arc<ProfileStorage>,
        job: [u8; 16],
        channel: ConversationId,
    ) -> Result<AccountAuthority, LogError> {
        Self::load_bound(profile, job, job, channel)
    }
    pub(in crate::node) fn load_bound(
        profile: Arc<ProfileStorage>,
        job: [u8; 16],
        source_job: [u8; 16],
        channel: ConversationId,
    ) -> Result<AccountAuthority, LogError> {
        let source_revision=profile.database().read(|c| {
            let revision:i64=c.query_row("SELECT source_revision FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=0 AND job=?3",params![b"MeshPartEventsV1".as_slice(),channel.as_bytes().as_slice(),source_job.as_slice()],|r|r.get(0))?;
            crate::node::channel::graph::require_source_on(c,channel,source_job,revision)?;
            Ok(revision)
        })?;
        let bytes: Option<Vec<u8>> = profile.database().read(|c| {
            c.query_row(
                "SELECT state FROM channel_authority_current WHERE job=?1 AND channel=?2",
                params![job.as_slice(), channel.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()
            .map_err(Into::into)
        })?;
        let mut authority: AccountAuthority = bytes
            .as_deref()
            .map(decode)
            .transpose()?
            .unwrap_or_default();
        authority.history = Some(Arc::new(Self {
            profile,
            job,
            channel,
            source_revision,
            source_job,
            _query_lease: None,
        }));
        Ok(authority)
    }
    fn guard_on(&self, c: &Connection) -> Result<(), LogError> {
        crate::node::channel::graph::require_source_on(
            c,
            self.channel,
            self.source_job,
            self.source_revision,
        )
    }
    fn get<T: serde::de::DeserializeOwned>(
        &self,
        kind: i64,
        key: Vec<u8>,
    ) -> Result<Option<T>, LogError> {
        self.profile.database().read(|c| {
            self.guard_on(c)?;
            let bytes:Option<Vec<u8>>=c.query_row("SELECT value FROM channel_authority_facts WHERE job=?1 AND channel=?2 AND kind=?3 AND key=?4",
                params![self.job.as_slice(),self.channel.as_bytes().as_slice(),kind,key],|r|r.get(0)).optional()?;
            bytes.as_deref().map(decode).transpose()
        })
    }
    fn put_on<T: Serialize>(
        &self,
        c: &Connection,
        kind: i64,
        key: Vec<u8>,
        account: Option<[u8; 32]>,
        value: &T,
    ) -> Result<bool, LogError> {
        Ok(c.execute(
            "INSERT OR IGNORE INTO channel_authority_facts VALUES(?1,?2,?3,?4,?5,?6)",
            params![
                self.job.as_slice(),
                self.channel.as_bytes().as_slice(),
                kind,
                key,
                account.as_ref().map(|v| v.as_slice()),
                encode(value)?
            ],
        )? != 0)
    }
    fn all_revocations_observed(
        &self,
        account: [u8; 32],
        event: EventId,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, LogError> {
        let mut after: Option<Vec<u8>> = None;
        loop {
            let page:Vec<(Vec<u8>,EventId)>=self.profile.database().read(|c| {
                self.guard_on(c)?;
                let mut statement=c.prepare("SELECT key,value FROM channel_authority_facts WHERE job=?1 AND channel=?2 AND kind=2 AND account=?3 AND (?4 IS NULL OR key>?4) ORDER BY key LIMIT 64")?;
                let mut rows=statement.query(params![self.job.as_slice(),self.channel.as_bytes().as_slice(),account.as_slice(),after])?;
                let mut result=Vec::new();
                while let Some(row)=rows.next()? {let bytes:Vec<u8>=row.get(1)?;result.push((row.get(0)?,decode(&bytes)?));}
                Ok(result)
            })?;
            if page.is_empty() {
                return Ok(true);
            }
            for (key, removed) in page {
                if !graph.descends(event, removed)? {
                    return Ok(false);
                }
                after = Some(key);
            }
        }
    }
}
impl AccountAuthority {
    fn check_fact_source(&self) -> Result<(), LogError> {
        if let Some(history) = &self.history {
            history.profile.database().read(|c| history.guard_on(c))?;
        }
        Ok(())
    }
    pub(in crate::node) fn try_knows_authority(&self, event: EventId) -> Result<bool, LogError> {
        self.check_fact_source()?;
        Ok(self.valid_controls.contains(&event)
            || self
                .history
                .as_ref()
                .map(|h| h.get::<()>(CONTROL, event.as_bytes().to_vec()))
                .transpose()?
                .flatten()
                .is_some())
    }
    pub(super) fn known_control(&self, event: EventId) -> Result<bool, AuthorityError> {
        Ok(self.valid_controls.contains(&event)
            || self
                .history
                .as_ref()
                .map(|h| h.get::<()>(CONTROL, event.as_bytes().to_vec()))
                .transpose()?
                .flatten()
                .is_some())
    }
    pub(super) fn revoked_event(
        &self,
        id: AccountGrantId,
    ) -> Result<Option<EventId>, AuthorityError> {
        if let Some(event) = self.revoked.get(&id) {
            return Ok(Some(*event));
        }
        self.history
            .as_ref()
            .map(|h| h.get(REVOKED, encode(&id)?))
            .transpose()
            .map(|v| v.flatten())
            .map_err(Into::into)
    }
    pub(super) fn previous_pin(
        &self,
        key: LegacyBindingKey,
    ) -> Result<Option<[u8; 32]>, AuthorityError> {
        if let Some(account) = self.pinned.get(&key) {
            return Ok(Some(*account));
        }
        self.history
            .as_ref()
            .map(|h| h.get(PIN, encode(&key)?))
            .transpose()
            .map(|v| v.flatten())
            .map_err(Into::into)
    }
    pub(super) fn had_added(&self, id: AccountGrantId) -> Result<bool, AuthorityError> {
        Ok(self.added_grants.contains(&id)
            || self
                .history
                .as_ref()
                .map(|h| h.get::<()>(ADDED, encode(&id)?))
                .transpose()?
                .flatten()
                .is_some())
    }
    pub(super) fn conflicting_original(
        &self,
        public: &PublicIdentity,
    ) -> Result<bool, AuthorityError> {
        let Some(history) = &self.history else {
            return Ok(false);
        };
        let lower = encode(&(public.ed25519_pub, [0u8; 32]))?;
        let upper = encode(&(public.ed25519_pub, [255u8; 32]))?;
        let exact = encode(&(public.ed25519_pub, public.x25519_pub))?;
        Ok(history.profile.database().read(|c| {
            history.guard_on(c)?;
            c.query_row("SELECT EXISTS(SELECT 1 FROM channel_authority_facts WHERE job=?1 AND channel=?2 AND kind=7 AND key>=?3 AND key<=?4 AND key<>?5)",
                params![history.job.as_slice(),history.channel.as_bytes().as_slice(),lower,upper,exact],|r|r.get(0)).map_err(Into::into)
        })?)
    }
    pub(super) fn observed_revocations(
        &self,
        account: [u8; 32],
        event: EventId,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, AuthorityError> {
        for (id, removed) in &self.revoked {
            if id.account_pub == account && !graph.descends(event, *removed)? {
                return Ok(false);
            }
        }
        Ok(self
            .history
            .as_ref()
            .map(|h| h.all_revocations_observed(account, event, graph))
            .transpose()?
            .unwrap_or(true))
    }
    /// Call inside the channel projection transaction. This only mutates the
    /// private candidate; callers install it in RAM after COMMIT succeeds.
    pub(in crate::node) fn checkpoint_on(&mut self, c: &Connection) -> Result<(), LogError> {
        let Some(history) = self.history.clone() else {
            return Err(invalid("authority checkpoint lacks indexed owner"));
        };
        history.guard_on(c)?;
        for (lifetime, event) in &self.legacy_seen {
            let key = encode(lifetime)?;
            // Canonical evidence is the smallest exact event identity even
            // when concurrent branches are projected in a different order.
            let existing:Option<Vec<u8>>=c.query_row("SELECT value FROM channel_authority_facts WHERE job=?1 AND channel=?2 AND kind=?3 AND key=?4",params![history.job.as_slice(),history.channel.as_bytes().as_slice(),LEGACY_SEEN,&key],|r|r.get(0)).optional()?;
            let selected = existing
                .as_deref()
                .map(decode::<EventId>)
                .transpose()?
                .map_or(*event, |old| old.min(*event));
            c.execute("INSERT INTO channel_authority_facts VALUES(?1,?2,?3,?4,NULL,?5) ON CONFLICT(job,channel,kind,key) DO UPDATE SET value=excluded.value",params![history.job.as_slice(),history.channel.as_bytes().as_slice(),LEGACY_SEEN,key,encode(&selected)?])?;
        }
        for event in &self.valid_controls {
            history.put_on(c, CONTROL, event.as_bytes().to_vec(), None, &())?;
        }
        for (id, event) in &self.revoked {
            if history.put_on(c, REVOKED, encode(id)?, Some(id.account_pub), event)? {
                self.historical_revoked = self
                    .historical_revoked
                    .checked_add(1)
                    .ok_or(LogError::RepositoryCounterExhausted)?;
            }
        }
        for (id, events) in &self.acceptance_events {
            for event in events {
                history.put_on(
                    c,
                    ACCEPTANCE,
                    encode(&(id, event))?,
                    Some(id.account_pub),
                    event,
                )?;
            }
        }
        for id in self.added_grants.union(&self.declined_epoch_reservations) {
            if history.put_on(c, ADDED, encode(id)?, Some(id.account_pub), &())? {
                self.historical_added = self
                    .historical_added
                    .checked_add(1)
                    .ok_or(LogError::RepositoryCounterExhausted)?;
            }
        }
        for (key, account) in &self.pinned {
            history.put_on(c, PIN, encode(key)?, Some(*account), account)?;
        }
        for proof in &self.originals {
            let account = proof
                .account_cert
                .as_ref()
                .ok_or_else(|| invalid("original device proof has no account certificate"))?
                .account_ed25519_pub;
            history.put_on(
                c,
                ORIGINAL,
                encode(&(proof.ed25519_pub, proof.x25519_pub, account))?,
                Some(account),
                proof,
            )?;
            history.put_on(
                c,
                KNOWN_PUBLIC,
                encode(&(proof.ed25519_pub, proof.x25519_pub))?,
                None,
                &(),
            )?;
        }
        for (id, event) in &self.origins {
            history.put_on(c, ORIGIN, encode(id)?, Some(id.account_pub), event)?;
        }
        self.legacy_revoked.extend(
            self.revoked
                .keys()
                .filter(|id| self.initial_legacy.contains_key(id))
                .copied(),
        );
        let initial_keys: HashSet<_> = self
            .initial_legacy
            .values()
            .flat_map(|cohort| cohort.originals.iter().map(lifetime_key))
            .collect();
        self.legacy_base.retain(|key| initial_keys.contains(key));
        self.valid_controls.clear();
        self.revoked.clear();
        self.added_grants.clear();
        self.declined_epoch_reservations.clear();
        self.acceptance_events.clear();
        self.pinned.clear();
        let current: HashSet<_> = self.grants.values().filter_map(AccountGrant::id).collect();
        self.device_grants.retain(|_, id| current.contains(id));
        self.originals.retain(|proof| {
            self.device_grants
                .contains_key(&(proof.ed25519_pub, proof.x25519_pub))
        });
        self.origins.retain(|id, _| current.contains(id));
        self.activated.retain(|id| current.contains(id));
        self.inherited.retain(|_, (id, _)| current.contains(id));
        c.execute("INSERT INTO channel_authority_current VALUES(?1,?2,?3) ON CONFLICT(job,channel) DO UPDATE SET state=excluded.state",
            params![history.job.as_slice(),history.channel.as_bytes().as_slice(),encode(self)?])?;
        Ok(())
    }
}

/// Bounded historical revocation page. The encoded grant key is a local cursor,
/// never a wire identifier; its owner binds every read to the current source.
pub(in crate::node) struct RevocationPage {
    pub(in crate::node) events: Vec<EventId>,
    pub(in crate::node) grants: Vec<(AccountGrantId, EventId)>,
    pub(in crate::node) next: Option<Vec<u8>>,
}
impl AccountAuthority {
    pub(in crate::node) fn try_registered_account(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Result<Option<[u8; 32]>, LogError> {
        self.check_fact_source()?;
        if proof.public() != *public || !proof.valid_presence() {
            return Ok(None);
        }
        let Some(account) = proof.account_cert.as_ref().map(|c| c.account_ed25519_pub) else {
            return Ok(None);
        };
        if self.registered_account(public, proof).is_some() {
            return Ok(Some(account));
        }
        let original: Option<Announce> = self
            .history
            .as_ref()
            .map(|h| {
                h.get(
                    ORIGINAL,
                    encode(&(public.ed25519_pub, public.x25519_pub, account))?,
                )
            })
            .transpose()?
            .flatten();
        Ok(original
            .filter(|p| {
                p.public() == *public
                    && p.account_cert
                        .as_ref()
                        .is_some_and(|c| c.account_ed25519_pub == account)
            })
            .map(|_| account))
    }
    pub(in crate::node) fn has_revocation(&self, account: &[u8; 32]) -> Result<bool, LogError> {
        self.check_fact_source()?;
        if self.revoked.keys().any(|id| &id.account_pub == account) {
            return Ok(true);
        }
        let Some(history) = &self.history else {
            return Ok(false);
        };
        history.profile.database().read(|c| {
            history.guard_on(c)?;
            c.query_row("SELECT EXISTS(SELECT 1 FROM channel_authority_facts WHERE job=?1 AND channel=?2 AND kind=2 AND account=?3)",params![history.job.as_slice(),history.channel.as_bytes().as_slice(),account.as_slice()],|r|r.get(0)).map_err(Into::into)
        })
    }
    pub(in crate::node) fn revocation_page(
        &self,
        account: &[u8; 32],
        after: Option<&[u8]>,
    ) -> Result<RevocationPage, LogError> {
        self.revocation_page_with_limit(account, after, 64)
    }
    pub(in crate::node) fn revocation_page_with_limit(
        &self,
        account: &[u8; 32],
        after: Option<&[u8]>,
        limit: usize,
    ) -> Result<RevocationPage, LogError> {
        self.check_fact_source()?;
        let limit = crate::eventlog::sync::network::scan_limit(limit.min(64))? - 1;
        let Some(history) = &self.history else {
            let mut selected = std::collections::BTreeMap::new();
            for (id, event) in &self.revoked {
                if &id.account_pub != account {
                    continue;
                }
                let key = encode(id)?;
                if after.is_some_and(|after| key.as_slice() <= after) {
                    continue;
                }
                selected.insert(key, (*id, *event));
                if selected.len() > limit as usize {
                    selected.pop_last();
                }
            }
            let next = selected.last_key_value().map(|(key, _)| key.clone());
            let grants: Vec<_> = selected.into_values().collect();
            let events = grants.iter().map(|(_, event)| *event).collect();
            return Ok(RevocationPage {
                events,
                grants,
                next,
            });
        };
        history.profile.database().read(|c| {
            history.guard_on(c)?;
            let mut statement=c.prepare("SELECT key,value FROM channel_authority_facts WHERE job=?1 AND channel=?2 AND kind=2 AND account=?3 AND (?4 IS NULL OR key>?4) ORDER BY key LIMIT ?5")?;
            let mut rows=statement.query(params![history.job.as_slice(),history.channel.as_bytes().as_slice(),account.as_slice(),after,limit])?;
            let mut events=Vec::new();let mut grants=Vec::new();let mut next=None;
            while let Some(row)=rows.next()? {let key:Vec<u8>=row.get(0)?;let bytes:Vec<u8>=row.get(1)?;let event=decode(&bytes)?;grants.push((decode(&key)?,event));events.push(event);next=Some(key);}
            Ok(RevocationPage{events,grants,next})
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        eventlog::{
            persist::PersistentEventLog,
            repository::order::{CausalOrder, OrderReadiness, OrderWorkBudget},
        },
        identity::{account::Account, device::DeviceIdentity},
        storage::profile::ProfileStorageLease,
    };
    #[test]
    fn channel_historical_device_binding_keeps_each_account_certificate() {
        let dir = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let first = Account::generate();
        let second = Account::generate();
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(dir.path()).unwrap(),
            &device.public(),
            &first.public(),
            &dir.path().join("messages.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        let log = PersistentEventLog::open_profile(profile.clone(), "pw").unwrap();
        let channel = ConversationId::new([37; 32]);
        let root = Event::new(
            &device,
            channel,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            crate::channel::ChannelMeta {
                name: "binding".into(),
                members: vec![device.public()],
                owner: device.user_id(),
                epoch: 0,
            }
            .encode(),
        );
        let repository = log.repository().unwrap();
        repository.append(&root).unwrap();
        while repository
            .order_step(&channel, CausalOrder::Logical, OrderWorkBudget::default())
            .unwrap()
            .readiness
            != OrderReadiness::Ready
        {}
        let job = repository
            .order_generation(&channel, CausalOrder::Logical)
            .unwrap()
            .unwrap()
            .job;
        profile
            .database()
            .transaction(|c| AuthorityHistory::initialize(c, true))
            .unwrap();
        let first_proof = Announce::new_with_account(&device, &first, "first", 1);
        let second_proof = Announce::new_with_account(&device, &second, "second", 1);
        assert!(first_proof.valid_presence() && second_proof.valid_presence());
        // The reducer supplies authenticated bindings at distinct grant lifetimes;
        // each checkpoint then drops retired roles from its current-only blob.
        for proof in [&first_proof, &second_proof] {
            let mut candidate = AuthorityHistory::load(profile.clone(), job, channel).unwrap();
            candidate.originals.push(proof.clone());
            profile
                .database()
                .transaction(|c| candidate.checkpoint_on(c))
                .unwrap();
        }
        let restored = AuthorityHistory::load(profile, job, channel).unwrap();
        assert_eq!(
            restored
                .try_registered_account(&device.public(), &first_proof)
                .unwrap(),
            Some(first.public().ed25519_pub)
        );
        assert_eq!(
            restored
                .try_registered_account(&device.public(), &second_proof)
                .unwrap(),
            Some(second.public().ed25519_pub)
        );
        assert!(!restored.conflicting_original(&device.public()).unwrap());
        let mut conflicting = DeviceIdentity::generate().public();
        conflicting.ed25519_pub = device.public().ed25519_pub;
        assert!(restored.conflicting_original(&conflicting).unwrap());
    }
}

impl AccountAuthority {
    pub(in crate::node) fn legacy_candidate(
        &self,
        event: EventId,
    ) -> Result<Option<LegacyDeparture>, LogError> {
        if let Some(candidate) = self
            .legacy_candidates
            .iter()
            .find(|candidate| candidate.event == event)
        {
            return Ok(Some(candidate.clone()));
        }
        let Some(history) = &self.history else {
            return Ok(None);
        };
        history.profile.database().read(|c| {
            history.guard_on(c)?;
            let bytes: Option<Vec<u8>> = c.query_row("SELECT member FROM channel_legacy_candidates WHERE job=?1 AND channel=?2 AND event=?3",params![history.source_job.as_slice(),history.channel.as_bytes().as_slice(),event.as_bytes().as_slice()],|r|r.get(0)).optional()?;
            bytes.map(|bytes| Ok(LegacyDeparture {event,member:decode(&bytes)?})).transpose()
        })
    }
}

impl AccountAuthority {
    /// Load only the member lifetimes affecting the current bounded context.
    /// Historical candidates and observed departures stay in the fact tables.
    pub(in crate::node) fn prepare_legacy_members<'a>(
        &mut self,
        members: impl Iterator<Item = &'a LegacyAccountMember>,
        enabled: bool,
    ) -> Result<(), LogError> {
        let Some(history) = &self.history else {
            return Ok(());
        };
        let mut keys: HashSet<_> = members.map(lifetime_key).collect();
        keys.extend(
            self.initial_legacy
                .values()
                .flat_map(|cohort| cohort.originals.iter().map(lifetime_key)),
        );
        keys.extend(self.legacy_omitted.iter().map(lifetime_key));
        self.legacy_seen.retain(|key, _| keys.contains(key));
        self.legacy_candidate_lifetimes.clear();
        self.legacy_revoked.clear();
        for id in self.initial_legacy.keys() {
            if history.get::<EventId>(REVOKED, encode(id)?)?.is_some() {
                self.legacy_revoked.insert(*id);
            }
        }
        for key in keys {
            if let Some(event) = history.get(LEGACY_SEEN, encode(&key)?)? {
                self.legacy_seen.insert(key, event);
            }
            if enabled {
                let exists:bool=history.profile.database().read(|c| {
                    history.guard_on(c)?;
                    c.query_row("SELECT EXISTS(SELECT 1 FROM channel_legacy_candidates WHERE job=?1 AND channel=?2 AND lifetime=?3)",params![history.source_job.as_slice(),history.channel.as_bytes().as_slice(),encode(&key)?],|r|r.get(0)).map_err(Into::into)
                })?;
                if exists {
                    self.legacy_candidate_lifetimes.insert(key);
                }
            }
        }
        Ok(())
    }
    pub(super) fn legacy_candidate_observed(
        &self,
        member: &LegacyAccountMember,
        event: EventId,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, AuthorityError> {
        let key = lifetime_key(member);
        for candidate in &self.legacy_candidates {
            if lifetime_key(&candidate.member) == key && graph.descends(event, candidate.event)? {
                return Ok(true);
            }
        }
        let Some(history) = &self.history else {
            return Ok(false);
        };
        let mut after: Option<Vec<u8>> = None;
        loop {
            let page:Vec<Vec<u8>>=history.profile.database().read(|c| {
                history.guard_on(c)?;
                let mut statement=c.prepare("SELECT event FROM channel_legacy_candidates WHERE job=?1 AND channel=?2 AND lifetime=?3 AND (?4 IS NULL OR event>?4) ORDER BY event LIMIT 64")?;
                statement.query_map(params![history.source_job.as_slice(),history.channel.as_bytes().as_slice(),encode(&key)?,after],|r|r.get(0))?.collect::<Result<_,_>>().map_err(Into::into)
            })?;
            if page.is_empty() {
                return Ok(false);
            }
            for bytes in page {
                let id = EventId::new(
                    bytes
                        .as_slice()
                        .try_into()
                        .map_err(|_| invalid("invalid legacy candidate identity"))?,
                );
                if graph.descends(event, id)? {
                    return Ok(true);
                }
                after = Some(bytes);
            }
        }
    }
}

impl AccountAuthority {
    pub(super) fn observed_acceptance(
        &self,
        id: AccountGrantId,
        event: EventId,
        graph: &impl AuthorityGraph,
    ) -> Result<bool, AuthorityError> {
        if let Some(accepted) = self.acceptance_events.get(&id) {
            for accepted in accepted {
                if graph.descends(event, *accepted)? {
                    return Ok(true);
                }
            }
        }
        let Some(history) = &self.history else {
            return Ok(false);
        };
        let mut after: Option<Vec<u8>> = None;
        loop {
            let page:Vec<(Vec<u8>,Option<EventId>)>=history.profile.database().read(|c| {
                history.guard_on(c)?;
                let mut statement=c.prepare("SELECT key,value FROM channel_authority_facts WHERE job=?1 AND channel=?2 AND kind=?3 AND account=?4 AND (?5 IS NULL OR key>?5) ORDER BY key LIMIT 64")?;
                let mut rows=statement.query(params![history.job.as_slice(),history.channel.as_bytes().as_slice(),ACCEPTANCE,id.account_pub.as_slice(),after])?;
                let mut result=Vec::new();
                while let Some(row)=rows.next()? {let key:Vec<u8>=row.get(0)?;let (stored,_):(AccountGrantId,EventId)=decode(&key)?;let bytes:Vec<u8>=row.get(1)?;result.push((key,if stored==id {Some(decode::<EventId>(&bytes)?)}else{None}));}
                Ok(result)
            })?;
            if page.is_empty() {
                return Ok(false);
            }
            for (key, accepted) in page {
                if let Some(accepted) = accepted
                    && graph.descends(event, accepted)?
                {
                    return Ok(true);
                }
                after = Some(key);
            }
        }
    }
}

impl AccountAuthority {
    pub(in crate::node) fn try_grant_origin(
        &self,
        id: AccountGrantId,
    ) -> Result<Option<EventId>, LogError> {
        self.check_fact_source()?;
        if let Some(origin) = self.origins.get(&id) {
            return Ok(Some(*origin));
        }
        self.history
            .as_ref()
            .map(|h| h.get(ORIGIN, encode(&id)?))
            .transpose()
            .map(Option::flatten)
    }
    fn historical_origin_matches(
        &self,
        account: [u8; 32],
        mut matches: impl FnMut(AccountGrantId, EventId) -> bool,
    ) -> Result<bool, LogError> {
        if self
            .origins
            .iter()
            .any(|(id, event)| id.account_pub == account && matches(*id, *event))
        {
            return Ok(true);
        }
        let Some(history) = &self.history else {
            return Ok(false);
        };
        let mut after: Option<Vec<u8>> = None;
        loop {
            let page:Vec<(Vec<u8>,AccountGrantId,EventId)>=history.profile.database().read(|c| {
                history.guard_on(c)?;
                let mut statement=c.prepare("SELECT key,value FROM channel_authority_facts WHERE job=?1 AND channel=?2 AND kind=?3 AND account=?4 AND (?5 IS NULL OR key>?5) ORDER BY key LIMIT 64")?;
                let mut rows=statement.query(params![history.job.as_slice(),history.channel.as_bytes().as_slice(),ORIGIN,account.as_slice(),after])?;
                let mut result=Vec::new();
                while let Some(row)=rows.next()? {let key:Vec<u8>=row.get(0)?;let bytes:Vec<u8>=row.get(1)?;result.push((key.clone(),decode(&key)?,decode(&bytes)?));}
                Ok(result)
            })?;
            if page.is_empty() {
                return Ok(false);
            }
            for (key, id, event) in page {
                if matches(id, event) {
                    return Ok(true);
                }
                after = Some(key);
            }
        }
    }
    pub(in crate::node) fn try_legacy_retired_source(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Result<Option<(AccountGrant, LegacyAccountMember, EventId)>, LogError> {
        self.check_fact_source()?;
        let Some(value) = self.legacy_retired_source(public, proof) else {
            return Ok(None);
        };
        let Some(id) = value.0.id() else {
            return Ok(None);
        };
        if self.historical_origin_matches(id.account_pub, |other, _| other != id)? {
            return Ok(None);
        }
        Ok(Some(value))
    }
    pub(in crate::node) fn try_omitted_legacy_departure(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Result<Option<EventId>, LogError> {
        self.check_fact_source()?;
        let Some(event) = self.omitted_legacy_departure(public, proof) else {
            return Ok(None);
        };
        let Some(account) = proof
            .account_cert
            .as_ref()
            .map(|cert| cert.account_ed25519_pub)
        else {
            return Ok(None);
        };
        if self.historical_origin_matches(account, |_, origin| Some(origin) != self.genesis)? {
            return Ok(None);
        }
        Ok(Some(event))
    }
    /// The immutable initial signed cohort bounds this selected terminal basis;
    /// repeated later grant history is tested through indexed origin facts.
    pub(in crate::node) fn try_legacy_closing_basis(
        &self,
        public: &PublicIdentity,
        proof: &Announce,
    ) -> Result<Vec<EventId>, LogError> {
        self.check_fact_source()?;
        let retired = self
            .try_legacy_retired_source(public, proof)?
            .and_then(|(grant, _, _)| grant.id());
        let omitted = self.try_omitted_legacy_departure(public, proof)?;
        let account = proof
            .account_cert
            .as_ref()
            .map(|cert| cert.account_ed25519_pub);
        let mut events = Vec::new();
        for (id, cohort) in &self.initial_legacy {
            if retired == Some(*id)
                || omitted.is_some()
                    && account == Some(id.account_pub)
                    && self.cohort_closed(*id, false)
            {
                events.extend(
                    cohort
                        .originals
                        .iter()
                        .filter_map(|member| self.legacy_seen.get(&lifetime_key(member)).copied()),
                );
            }
        }
        let has_origin = if let Some(account) = account {
            self.historical_origin_matches(account, |_, _| true)?
        } else {
            false
        };
        if !events.is_empty() || !has_origin {
            events.extend(omitted);
        }
        if !events.is_empty() {
            events.extend(self.genesis);
        }
        events.sort_unstable();
        events.dedup();
        Ok(events)
    }
}

impl AccountAuthority {
    pub(in crate::node) fn retain_query_lease(
        &mut self,
        lease: Arc<crate::node::channel::checkpoint::causal::Query>,
    ) -> Result<(), LogError> {
        let history = self
            .history
            .as_mut()
            .and_then(Arc::get_mut)
            .ok_or_else(|| invalid("causal authority lease already shared"))?;
        history._query_lease = Some(lease);
        Ok(())
    }
}
