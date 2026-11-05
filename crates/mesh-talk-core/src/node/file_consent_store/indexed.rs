//! Selected signed file consent and direct observations on the participant DB.
use super::{MAGIC, Record};
use crate::{
    eventlog::{Event, EventId, LogError},
    identity::{account::AccountPublic, device::PublicIdentity},
    node::{
        ReceiveDecision,
        delivery_store::DeliveryLimits,
        file_consent::Decision,
        transcript_repository::{FILE_CONSENT, publication::PublishedHistorySource},
    },
    storage::{
        profile::{
            context::ProfileStorage,
            manifest,
            participant::event_ownership::{self, ParticipantEventOwner},
        },
        record_log::EncryptedRecordLog,
    },
};
use bincode::Options;
use rusqlite::{Connection, OptionalExtension, params};
use std::{
    collections::HashMap,
    path::Path,
    sync::Arc,
    time::{Duration, Instant},
};

const OFFER_SCAN_RUNTIME_LIMIT: usize = 512;
const OFFER_SCAN_CHECKPOINT_INTERVAL: Duration = Duration::from_secs(30);

struct OfferScanProgress {
    position: i64,
    durable: i64,
    checkpoint_at: Instant,
    touched: Instant,
}

// Decision::open admits <=1024 ciphertext bytes, an empty parent list and a
// canonical 64-byte signature. This includes every valid encoded Event header.
const CONTROL_BYTES: usize = 2048;
const SCHEMA: &str = "
CREATE TABLE file_consent_counts(account TEXT PRIMARY KEY REFERENCES participant_account_namespaces(account_id),published INTEGER NOT NULL CHECK(published>=0),received INTEGER NOT NULL CHECK(received>=0),decisions INTEGER NOT NULL CHECK(decisions>=0)) WITHOUT ROWID;
CREATE TABLE file_consent_published(account TEXT NOT NULL REFERENCES file_consent_counts(account),manifest BLOB NOT NULL CHECK(length(manifest)=32),sequence BLOB NOT NULL CHECK(length(sequence)=8),event BLOB NOT NULL CHECK(length(event)=32),body BLOB NOT NULL CHECK(length(body)<=2048),session BLOB CHECK(session IS NULL OR length(session)=16),generation BLOB CHECK(generation IS NULL OR length(generation)=8),PRIMARY KEY(account,manifest)) WITHOUT ROWID;
CREATE TABLE file_consent_received(account TEXT NOT NULL REFERENCES file_consent_counts(account),manifest BLOB NOT NULL CHECK(length(manifest)=32),recipient BLOB NOT NULL CHECK(length(recipient)=32),sequence BLOB NOT NULL CHECK(length(sequence)=8),event BLOB NOT NULL CHECK(length(event)=32),body BLOB NOT NULL CHECK(length(body)<=2048),PRIMARY KEY(account,manifest,recipient)) WITHOUT ROWID;
CREATE TABLE file_consent_decisions(account TEXT NOT NULL REFERENCES file_consent_counts(account),manifest BLOB NOT NULL CHECK(length(manifest)=32),decision INTEGER NOT NULL CHECK(decision BETWEEN 0 AND 2),direct_override INTEGER CHECK(direct_override BETWEEN 0 AND 2),PRIMARY KEY(account,manifest)) WITHOUT ROWID;
";

fn invalid(message: &str) -> LogError {
    LogError::CorruptFile(message.into())
}
fn decision_code(value: ReceiveDecision) -> i64 {
    match value {
        ReceiveDecision::Pending => 0,
        ReceiveDecision::Accepted => 1,
        ReceiveDecision::Declined => 2,
    }
}
fn decision_value(value: i64) -> Result<ReceiveDecision, LogError> {
    match value {
        0 => Ok(ReceiveDecision::Pending),
        1 => Ok(ReceiveDecision::Accepted),
        2 => Ok(ReceiveDecision::Declined),
        _ => Err(invalid("invalid file consent decision")),
    }
}
fn active_on(c: &Connection) -> Result<AccountPublic, LogError> {
    let bytes: Vec<u8> = c.query_row(
        "SELECT account FROM participant_profile WHERE singleton=1",
        [],
        |r| r.get(0),
    )?;
    Ok(AccountPublic {
        ed25519_pub: bytes
            .try_into()
            .map_err(|_| invalid("invalid file consent owner"))?,
    })
}
fn initialize_on(c: &Connection, account: &str) -> Result<(), LogError> {
    c.execute(
        "INSERT OR IGNORE INTO file_consent_counts VALUES(?1,0,0,0)",
        [account],
    )?;
    Ok(())
}
fn reserve_on(c: &Connection, account: &str, kind: &str, limit: usize) -> Result<(), LogError> {
    let sql = match kind {
        "published" => {
            "UPDATE file_consent_counts SET published=published+1 WHERE account=?1 AND published<?2"
        }
        "received" => {
            "UPDATE file_consent_counts SET received=received+1 WHERE account=?1 AND received<?2"
        }
        "decisions" => {
            "UPDATE file_consent_counts SET decisions=decisions+1 WHERE account=?1 AND decisions<?2"
        }
        _ => return Err(invalid("invalid consent counter")),
    };
    if c.execute(
        sql,
        params![
            account,
            i64::try_from(limit).map_err(|_| invalid("invalid consent limit"))?
        ],
    )? != 1
    {
        return Err(LogError::Serialization("file consent control limit".into()));
    }
    Ok(())
}
fn decision_on(
    c: &Connection,
    account: &str,
    manifest: EventId,
) -> Result<Option<(i64, Option<i64>)>, LogError> {
    Ok(c.query_row("SELECT decision,direct_override FROM file_consent_decisions WHERE account=?1 AND manifest=?2",params![account,manifest.as_bytes().as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?)
}
fn put_decision_on(
    c: &Connection,
    account: &str,
    id: EventId,
    decision: ReceiveDecision,
    direct: bool,
    limit: usize,
) -> Result<(), LogError> {
    if decision_on(c, account, id)?.is_none() {
        reserve_on(c, account, "decisions", limit)?;
    }
    let code = decision_code(decision);
    c.execute("INSERT INTO file_consent_decisions VALUES(?1,?2,?3,?4) ON CONFLICT(account,manifest) DO UPDATE SET decision=excluded.decision,direct_override=excluded.direct_override",params![account,id.as_bytes().as_slice(),code,direct.then_some(code)])?;
    Ok(())
}
fn body(event: &Event) -> Result<Vec<u8>, LogError> {
    let size =
        bincode::serialized_size(event).map_err(|e| LogError::Serialization(e.to_string()))?;
    if size > CONTROL_BYTES as u64 {
        return Err(invalid("file consent event exceeds its admitted format"));
    }
    bincode::serialize(event).map_err(|e| LogError::Serialization(e.to_string()))
}
fn decode(body: &[u8], manifest: EventId, recipient: Option<&[u8; 32]>) -> Result<Event, LogError> {
    if body.len() > CONTROL_BYTES {
        return Err(invalid("oversized indexed file consent"));
    }
    let event: Event = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(CONTROL_BYTES as u64)
        .reject_trailing_bytes()
        .deserialize(body)
        .map_err(|_| invalid("invalid indexed file consent"))?;
    let decision =
        Decision::open(&event).ok_or_else(|| invalid("invalid signed indexed file consent"))?;
    if decision.manifest != manifest
        || recipient.is_some_and(|key| key != &decision.recipient.ed25519_pub)
    {
        return Err(invalid("indexed file consent identity changed"));
    }
    Ok(event)
}
fn apply_on(c: &Connection, account: &str, record: Record, limit: usize) -> Result<(), LogError> {
    initialize_on(c, account)?;
    let (event, published, direct) = match record {
        Record::Observed { manifest, decision } => {
            return put_decision_on(c, account, manifest, decision, true, limit);
        }
        Record::Published(event) => (event, true, false),
        Record::Received(event) => (event, false, false),
        Record::ReceivedDirect(event) => (event, false, true),
    };
    let payload = Decision::open(&event).ok_or_else(|| invalid("invalid file consent control"))?;
    let sequence = event.seq.to_be_bytes();
    let old: Option<Vec<u8>> = if published {
        c.query_row(
            "SELECT sequence FROM file_consent_published WHERE account=?1 AND manifest=?2",
            params![account, payload.manifest.as_bytes().as_slice()],
            |r| r.get(0),
        )
        .optional()?
    } else {
        c.query_row("SELECT sequence FROM file_consent_received WHERE account=?1 AND manifest=?2 AND recipient=?3",params![account,payload.manifest.as_bytes().as_slice(),payload.recipient.ed25519_pub.as_slice()],|r|r.get(0)).optional()?
    };
    let changed = old.as_deref().is_none_or(|old| old < sequence.as_slice());
    if changed {
        if old.is_none() {
            reserve_on(
                c,
                account,
                if published { "published" } else { "received" },
                limit,
            )?;
        }
        let bytes = body(&event)?;
        if published {
            c.execute("INSERT INTO file_consent_published VALUES(?1,?2,?3,?4,?5,NULL,NULL) ON CONFLICT(account,manifest) DO UPDATE SET sequence=excluded.sequence,event=excluded.event,body=excluded.body",params![account,payload.manifest.as_bytes().as_slice(),sequence.as_slice(),event.id.as_bytes().as_slice(),bytes])?;
        } else {
            c.execute("INSERT INTO file_consent_received VALUES(?1,?2,?3,?4,?5,?6) ON CONFLICT(account,manifest,recipient) DO UPDATE SET sequence=excluded.sequence,event=excluded.event,body=excluded.body",params![account,payload.manifest.as_bytes().as_slice(),payload.recipient.ed25519_pub.as_slice(),sequence.as_slice(),event.id.as_bytes().as_slice(),bytes])?;
        }
    }
    if !published
        && (direct
            || (changed
                && decision_on(c, account, payload.manifest)?
                    .and_then(|(_, value)| value)
                    .is_none_or(|value| value == 1)))
    {
        put_decision_on(c, account, payload.manifest, payload.decision, false, limit)?;
    }
    Ok(())
}

pub(crate) struct IndexedFileConsentStore {
    source: PublishedHistorySource,
    owner: Option<ParticipantEventOwner>,
    session: [u8; 16],
    limit: usize,
    offer_scans: HashMap<[u8; 32], OfferScanProgress>,
    #[cfg(test)]
    fail_writes: bool,
}
impl IndexedFileConsentStore {
    fn durable_offer_scan_position(&self, peer: &[u8; 32]) -> Result<i64, LogError> {
        self.source.check_barrier()?;
        self.source.profile().database().read(|c| {
            let owner = self.check_on(c)?;
            Ok(c.query_row(
                "SELECT after_ordinal FROM file_consent_offer_scan WHERE account=?1 AND peer=?2",
                params![owner.namespace(), peer.as_slice()],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(0))
        })
    }

    pub(crate) fn offer_scan_position(&self, peer: &[u8; 32]) -> Result<i64, LogError> {
        let durable = self.durable_offer_scan_position(peer)?;
        // Another live handle may have checkpointed this peer. Its durable
        // position supersedes our hint; a fresh retry must still make progress.
        Ok(self
            .offer_scans
            .get(peer)
            .filter(|scan| scan.durable == durable)
            .map_or(durable, |scan| scan.position))
    }

    pub(crate) fn advance_offer_scan(
        &mut self,
        peer: &[u8; 32],
        before: i64,
        after: i64,
    ) -> Result<(), LogError> {
        let durable = self.durable_offer_scan_position(peer)?;
        let previous = self
            .offer_scans
            .get(peer)
            .filter(|scan| scan.durable == durable);
        if previous.map_or(durable, |scan| scan.position) != before || after < 0 {
            return Err(LogError::StaleCursor);
        }
        if before == after {
            return Ok(());
        }
        let now = Instant::now();
        let checkpoint = after != durable
            && previous.is_none_or(|scan| {
                now.duration_since(scan.checkpoint_at) >= OFFER_SCAN_CHECKPOINT_INTERVAL
            });
        let checkpoint_at = previous.map_or(now, |scan| scan.checkpoint_at);
        if checkpoint {
            // Discovery is a replayable hint, never durable consent acceptance.
            // Checkpoint the first changed page so repeated short restarts still
            // progress; warm rotation only needs a bounded periodic checkpoint.
            #[cfg(test)]
            self.check_write_failure()?;
            self.source.profile().database().transaction(|c| {
                let owner = self.check_on(c)?;
                c.execute("INSERT OR IGNORE INTO file_consent_offer_scan VALUES(?1,?2,0)", params![owner.namespace(), peer.as_slice()])?;
                if c.execute("UPDATE file_consent_offer_scan SET after_ordinal=?3 WHERE account=?1 AND peer=?2 AND after_ordinal=?4", params![owner.namespace(), peer.as_slice(), after, durable])? != 1 {
                    return Err(LogError::StaleCursor);
                }
                Ok(())
            })?;
        }
        // Failed required writes never publish the runtime cursor or reset the
        // checkpoint clock. Eviction may replay old discovery after re-admission,
        // while preserving its already durable restart position.
        if !self.offer_scans.contains_key(peer)
            && self.offer_scans.len() >= OFFER_SCAN_RUNTIME_LIMIT
        {
            let oldest = *self
                .offer_scans
                .iter()
                .min_by_key(|(_, scan)| scan.touched)
                .expect("full offer scan cache")
                .0;
            self.offer_scans.remove(&oldest);
        }
        self.offer_scans.insert(
            *peer,
            OfferScanProgress {
                position: after,
                durable: if checkpoint { after } else { durable },
                checkpoint_at: if checkpoint { now } else { checkpoint_at },
                touched: now,
            },
        );
        Ok(())
    }

    pub(crate) fn authenticate_legacy(
        profile: &ProfileStorage,
        path: &Path,
        password: &str,
    ) -> Result<(), LogError> {
        PublishedHistorySource::authenticate_legacy::<Record>(
            profile,
            path,
            password,
            FILE_CONSENT,
            MAGIC,
        )
    }
    pub(crate) fn from_profile(
        profile: Arc<ProfileStorage>,
        path: &Path,
        password: &str,
    ) -> Result<Self, LogError> {
        let limits = DeliveryLimits::default();
        let limit = limits
            .completed_receipts
            .saturating_mul(limits.destinations);
        profile.database().transaction(|c| {
            let present: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_consent_counts')",
                [],
                |r| r.get(0),
            )?;
            let registry: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='auxiliary_history_sources')",
                [],
                |r| r.get(0),
            )?;
            let published = registry
                && c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM auxiliary_history_sources WHERE kind=?1)",
                    [FILE_CONSENT],
                    |r| r.get::<_, bool>(0),
                )?;
            if !present && !published {
                c.execute_batch(SCHEMA)?;
            }
            manifest::validate_ddl(c, SCHEMA)?;
            const SCAN_SCHEMA: &str="CREATE TABLE file_consent_offer_scan(account TEXT NOT NULL REFERENCES file_consent_counts(account),peer BLOB NOT NULL CHECK(length(peer)=32),after_ordinal INTEGER NOT NULL CHECK(typeof(after_ordinal)='integer' AND after_ordinal>=0),PRIMARY KEY(account,peer)) WITHOUT ROWID;";
            let scan:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='file_consent_offer_scan')",[],|r|r.get(0))?;
            if !scan { c.execute_batch(SCAN_SCHEMA)?; }
            manifest::validate_ddl(c,SCAN_SCHEMA)
        })?;
        let source = PublishedHistorySource::open(
            profile,
            path,
            password,
            FILE_CONSENT,
            |c, path, password| {
                let mut namespace = None;
                let log = EncryptedRecordLog::<Record>::open_with_visitor(
                    path,
                    password,
                    MAGIC,
                    |record| {
                        if namespace.is_none() {
                            namespace = Some(event_ownership::legacy_source_namespace_on(
                                c,
                                "file consent",
                            )?);
                        }
                        apply_on(
                            c,
                            namespace.as_deref().expect("resolved source namespace"),
                            record,
                            limit,
                        )
                    },
                )?;
                log.sync()
            },
        )?;
        let mut store = Self {
            source,
            owner: None,
            session: *uuid::Uuid::new_v4().as_bytes(),
            limit,
            offer_scans: HashMap::new(),
            #[cfg(test)]
            fail_writes: false,
        };
        store.capture(true)?;
        Ok(store)
    }
    fn capture(&mut self, migration: bool) -> Result<(), LogError> {
        self.owner = self.source.profile().database().transaction(|c| {
            let account = active_on(c)?;
            initialize_on(c, &account.account_id())?;
            match event_ownership::capture_event_owner_on(c, &account) {
                Ok(owner) => Ok(Some(owner)),
                Err(LogError::StaleCursor) if migration => Ok(None),
                Err(error) => Err(error),
            }
        })?;
        Ok(())
    }
    pub(crate) fn recapture(&mut self) -> Result<(), LogError> {
        self.capture(false)?;
        self.offer_scans.clear();
        Ok(())
    }
    fn check_on<'a>(&'a self, c: &Connection) -> Result<&'a ParticipantEventOwner, LogError> {
        let owner = self.owner.as_ref().ok_or(LogError::StaleCursor)?;
        event_ownership::check_event_owner_on(c, owner)?;
        Ok(owner)
    }
    #[cfg(test)]
    fn check_write_failure(&self) -> Result<(), LogError> {
        if self.fail_writes {
            return Err(LogError::Io(std::io::Error::other(
                "injected consent write failure",
            )));
        }
        Ok(())
    }
    fn check_write(&self) -> Result<(), LogError> {
        #[cfg(test)]
        self.check_write_failure()?;
        self.source.check_write()
    }
    pub(crate) fn published_control(&self, id: EventId) -> Result<Option<Event>, LogError> {
        self.source.profile().database().read(|c|{
            let owner=self.check_on(c)?;
            let bytes:Option<Vec<u8>>=c.query_row("SELECT CASE WHEN length(body)<=2048 THEN body ELSE NULL END FROM file_consent_published WHERE account=?1 AND manifest=?2",params![owner.namespace(),id.as_bytes().as_slice()],|r|r.get(0)).optional()?;
            bytes.map(|bytes|decode(&bytes,id,None)).transpose()
        })
    }
    pub(crate) fn published_generation(&self, id: EventId) -> Result<Option<u64>, LogError> {
        self.source.profile().database().read(|c|{
            let owner=self.check_on(c)?;
            let bytes:Option<Vec<u8>>=c.query_row("SELECT generation FROM file_consent_published WHERE account=?1 AND manifest=?2 AND session=?3",params![owner.namespace(),id.as_bytes().as_slice(),self.session.as_slice()],|r|r.get(0)).optional()?;
            bytes.map(|bytes|bytes.try_into().map(u64::from_be_bytes).map_err(|_|invalid("invalid consent generation"))).transpose()
        })
    }
    pub(crate) fn publish_control(
        &mut self,
        event: Event,
        generation: u64,
    ) -> Result<(), LogError> {
        let payload =
            Decision::open(&event).ok_or_else(|| invalid("invalid file consent control"))?;
        self.check_write()?;
        self.source.profile().database().transaction(|c|{
            let owner=self.check_on(c)?;let account=owner.namespace();
            apply_on(c,&account,Record::Published(event),self.limit)?;
            c.execute("UPDATE file_consent_published SET session=?3,generation=?4 WHERE account=?1 AND manifest=?2",params![account,payload.manifest.as_bytes().as_slice(),self.session.as_slice(),generation.to_be_bytes().as_slice()])?;Ok(())
        })
    }
    fn unique_recipient_on(
        c: &Connection,
        account: &str,
        id: EventId,
    ) -> Result<Option<[u8; 32]>, LogError> {
        let mut statement=c.prepare("SELECT recipient FROM file_consent_received WHERE account=?1 AND manifest=?2 ORDER BY recipient LIMIT 2")?;
        let mut rows = statement.query(params![account, id.as_bytes().as_slice()])?;
        let first = rows
            .next()?
            .map(|row| row.get::<_, Vec<u8>>(0))
            .transpose()?;
        if rows.next()?.is_some() {
            return Ok(None);
        }
        first
            .map(|bytes| {
                bytes
                    .try_into()
                    .map_err(|_| invalid("invalid consent recipient"))
            })
            .transpose()
    }
    pub(crate) fn remote_decision(&self, id: EventId) -> Result<Option<ReceiveDecision>, LogError> {
        self.source.profile().database().read(|c|{
            let account=self.check_on(c)?.namespace();
            let multiple:bool=c.query_row("SELECT EXISTS(SELECT 1 FROM file_consent_received WHERE account=?1 AND manifest=?2 LIMIT 1 OFFSET 1)",params![account,id.as_bytes().as_slice()],|r|r.get(0))?;
            if multiple{return Ok(None);}
            decision_on(c,&account,id)?.map(|(value,_)|decision_value(value)).transpose()
        })
    }
    fn received_on(
        c: &Connection,
        account: &str,
        id: EventId,
        recipient: &[u8; 32],
    ) -> Result<Option<Event>, LogError> {
        let bytes:Option<Vec<u8>>=c.query_row("SELECT CASE WHEN length(body)<=2048 THEN body ELSE NULL END FROM file_consent_received WHERE account=?1 AND manifest=?2 AND recipient=?3",params![account,id.as_bytes().as_slice(),recipient.as_slice()],|r|r.get(0)).optional()?;
        bytes
            .map(|bytes| decode(&bytes, id, Some(recipient)))
            .transpose()
    }
    pub(crate) fn received_control(&self, id: EventId) -> Result<Option<Event>, LogError> {
        self.source.profile().database().read(|c| {
            let account = self.check_on(c)?.namespace();
            match Self::unique_recipient_on(c, &account, id)? {
                Some(recipient) => Self::received_on(c, &account, id, &recipient),
                None => Ok(None),
            }
        })
    }
    pub(crate) fn received_control_for(
        &self,
        id: EventId,
        recipient: &PublicIdentity,
    ) -> Result<Option<Event>, LogError> {
        self.source.profile().database().read(|c| {
            Self::received_on(
                c,
                &self.check_on(c)?.namespace(),
                id,
                &recipient.ed25519_pub,
            )
        })
    }
    pub(crate) fn receive_control(&mut self, event: Event) -> Result<bool, LogError> {
        self.receive(event, false)
    }
    pub(crate) fn receive_direct_control(&mut self, event: Event) -> Result<bool, LogError> {
        self.receive(event, true)
    }
    fn receive_changes_on(
        &self,
        c: &Connection,
        event: &Event,
        payload: &Decision,
        direct: bool,
    ) -> Result<bool, LogError> {
        let account = self.check_on(c)?.namespace();
        let old:Option<(Vec<u8>,Vec<u8>)>=c.query_row("SELECT sequence,event FROM file_consent_received WHERE account=?1 AND manifest=?2 AND recipient=?3",params![account,payload.manifest.as_bytes().as_slice(),payload.recipient.ed25519_pub.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        if let Some((sequence, id)) = old {
            if sequence.as_slice() > event.seq.to_be_bytes().as_slice()
                || (!direct && sequence.as_slice() == event.seq.to_be_bytes().as_slice())
            {
                return Ok(false);
            }
            if direct
                && id.as_slice() == event.id.as_bytes().as_slice()
                && decision_on(c, &account, payload.manifest)?
                    .is_none_or(|(_, value)| value.is_none())
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
    fn receive(&mut self, event: Event, direct: bool) -> Result<bool, LogError> {
        let payload =
            Decision::open(&event).ok_or_else(|| invalid("invalid file consent control"))?;
        if !self
            .source
            .profile()
            .database()
            .read(|c| self.receive_changes_on(c, &event, &payload, direct))?
        {
            return Ok(false);
        }
        self.check_write()?;
        self.source.profile().database().transaction(|c| {
            if !self.receive_changes_on(c, &event, &payload, direct)? {
                return Ok(false);
            }
            let account = self.check_on(c)?.namespace();
            apply_on(
                c,
                &account,
                if direct {
                    Record::ReceivedDirect(event)
                } else {
                    Record::Received(event)
                },
                self.limit,
            )?;
            Ok(true)
        })
    }
    pub(crate) fn observe_remote_decision(
        &mut self,
        id: EventId,
        decision: ReceiveDecision,
    ) -> Result<(), LogError> {
        let unchanged = self.source.profile().database().read(|c| {
            let account = self.check_on(c)?.namespace();
            Ok(decision_on(c, &account, id)?
                .is_some_and(|(value, _)| value == decision_code(decision)))
        })?;
        if unchanged {
            return Ok(());
        }
        self.check_write()?;
        self.source.profile().database().transaction(|c| {
            let account = self.check_on(c)?.namespace();
            if decision_on(c, &account, id)?
                .is_some_and(|(value, _)| value == decision_code(decision))
            {
                return Ok(());
            }
            apply_on(
                c,
                &account,
                Record::Observed {
                    manifest: id,
                    decision,
                },
                self.limit,
            )
        })
    }
    #[cfg(test)]
    pub(crate) fn fail_appends_for_test(&mut self, enabled: bool) -> std::io::Result<()> {
        self.fail_writes = enabled;
        Ok(())
    }
}

#[cfg(test)]
impl IndexedFileConsentStore {
    pub(in crate::node) fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        use crate::{
            identity::{account::Account, device::DeviceIdentity},
            storage::profile::ProfileStorageLease,
        };
        let root = path
            .parent()
            .ok_or_else(|| invalid("missing test consent directory"))?;
        let device = DeviceIdentity::from_secret_bytes([87; 32], [88; 32]);
        let account = Account::from_secret_bytes([89; 32]);
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(root)?,
            &device.public(),
            &account.public(),
            &root.join("consent-test-events.log"),
            &root.join("consent-test-sent.log"),
            password,
        )?;
        profile.complete_participant(password, None)?;
        drop(crate::eventlog::persist::PersistentEventLog::open_profile(
            profile.clone(),
            password,
        )?);
        Self::from_profile(profile, path, password)
    }
    pub(super) fn received_controls(&self) -> std::vec::IntoIter<Event> {
        self.source.profile().database().read(|c|{
            let account=self.check_on(c)?.namespace();
            let mut statement=c.prepare("SELECT manifest,recipient,body FROM file_consent_received WHERE account=?1 ORDER BY manifest,recipient")?;
            let mut rows=statement.query([account])?;
            let mut events=Vec::new();
            while let Some(row)=rows.next()? {
                let id:Vec<u8>=row.get(0)?;
                let key:Vec<u8>=row.get(1)?;
                let body:Vec<u8>=row.get(2)?;
                events.push(decode(&body,EventId::new(id.try_into().map_err(|_|invalid("invalid consent manifest"))?),Some(&key.try_into().map_err(|_|invalid("invalid consent recipient"))?))?);
            }
            Ok(events)
        }).unwrap().into_iter()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        eventlog::ConversationId,
        identity::{account::Account, device::DeviceIdentity},
        storage::profile::participant,
    };
    fn payload(recipient: &DeviceIdentity, number: u8) -> Decision {
        Decision {
            file: ConversationId::new([number; 32]),
            manifest: EventId::new([number; 32]),
            sender: DeviceIdentity::generate().public(),
            recipient: recipient.public(),
            sender_account: Some("a".repeat(32)),
            recipient_account: "b".repeat(32),
            decision: ReceiveDecision::Accepted,
            completion: None,
        }
    }
    fn consent_offer_fixture(
        count: u8,
    ) -> (tempfile::TempDir, Arc<crate::node::Node>, DeviceIdentity) {
        use crate::{discovery::Announce, eventlog::EventKind, file::FileManifest};
        let directory = tempfile::tempdir().unwrap();
        let sender = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&sender, &Account::generate(), "Sender", 4000);
        let (node, _, _) = crate::node::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.set_attachment_global_auto(true).unwrap();
        let host = crate::node::conversation::dm_conversation_id(
            &sender.public(),
            &node.identity.public(),
        );
        for index in 1..=count {
            let manifest = FileManifest {
                name: "empty.txt".into(),
                size: 0,
                mime: "text/plain".into(),
                checksum: crate::file::file_checksum(&[]),
                file_key: [3; 32],
                file_conv: ConversationId::new([index; 32]),
                chunk_count: 1,
            };
            let original = Event::new(
                &sender,
                host,
                u64::from(index),
                vec![],
                u64::from(index),
                1,
                EventKind::FileManifest,
                crate::dm::seal(
                    &sender,
                    &node.identity.public().x25519_pub,
                    &manifest.encode(),
                )
                .unwrap(),
            );
            node.accept_manifest_event(
                &original,
                &proof,
                crate::node::received_log::ReceivedEntry {
                    event_id: original.id,
                    conversation: host,
                    from: sender.user_id(),
                    wall_clock: 1,
                    plaintext: manifest.encode(),
                },
            )
            .unwrap();
        }
        (directory, node, sender)
    }

    #[test]
    fn quiet_file_consent_event_iteration_preserves_controls_without_writes() {
        for count in [1, 8] {
            let (directory, node, sender) = consent_offer_fixture(count);
            let before = node.storage.database().successful_write_commits();
            crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.set(0));
            let expected = node.file_consent_events(&sender.public()).unwrap();
            assert_eq!(expected.len(), usize::from(count));
            assert!(
                node.storage.database().successful_write_commits() > before,
                "new consent controls and discovery progress must remain durable writes"
            );
            assert!(
                crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.get())
                    > 0
            );
            assert!(
                node.file_consent_events(&sender.public()).unwrap() == expected,
                "warming the same offer cycle must preserve its signed controls"
            );
            let database = node.storage.database();
            let changes = database
                .read(|connection| Ok(connection.total_changes()))
                .unwrap();
            let commits = database.successful_write_commits();
            crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.set(0));
            for _ in 0..3 {
                assert!(
                    node.file_consent_events(&sender.public()).unwrap() == expected,
                    "quiet polling must refresh the same signed consent controls"
                );
            }
            assert_eq!(
                database
                    .read(|connection| Ok(connection.total_changes()))
                    .unwrap(),
                changes,
                "a completed single-page offer cycle must not update its unchanged cursor"
            );
            assert_eq!(database.successful_write_commits(), commits);
            assert_eq!(
                crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.get()),
                0,
                "quiet consent discovery must not synchronize the parent directory"
            );
            let path = directory.path().join("file-consent-controls.log");
            let barrier = std::fs::read(&path).unwrap();
            std::fs::write(&path, vec![0; barrier.len()]).unwrap();
            assert!(
                node.file_consent_events(&sender.public()).is_err(),
                "a no-op cursor must still reject a replaced source barrier"
            );
            std::fs::write(path, barrier).unwrap();
        }
    }

    #[test]
    fn quiet_file_consent_rotating_iteration_has_bounded_checkpoint_writes() {
        let (_directory, node, sender) = consent_offer_fixture(33);
        let cycle = || {
            let mut seen = std::collections::HashSet::new();
            for _ in 0..6 {
                let controls = node.file_consent_events(&sender.public()).unwrap();
                // Preserve the actual API's existing 33-offer fairness oracle.
                assert!(controls.len() <= 8);
                assert!(
                    controls
                        .iter()
                        .map(|event| bincode::serialized_size(event).unwrap())
                        .sum::<u64>()
                        <= 16 * 1024
                );
                seen.extend(controls.into_iter().map(|event| event.id));
            }
            assert_eq!(
                seen.len(),
                33,
                "one bounded cycle must revisit every retained offer"
            );
            seen
        };
        let expected = cycle();
        let database = node.storage.database();
        let changes = database
            .read(|connection| Ok(connection.total_changes()))
            .unwrap();
        let commits = database.successful_write_commits();
        crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.set(0));
        assert!(
            cycle() == expected,
            "warm rotation must retain the same signed controls"
        );
        assert_eq!(
            database
                .read(|connection| Ok(connection.total_changes()))
                .unwrap(),
            changes,
            "warm multi-page rotation must advance in memory between durable checkpoints"
        );
        assert_eq!(database.successful_write_commits(), commits);
        assert_eq!(
            crate::storage::profile::publication::tests::PARENT_SYNCS.with(|value| value.get()),
            0,
            "cursor-only rotation must not republish an unchanged legacy-writer barrier"
        );
    }

    #[test]
    fn rotating_offer_checkpoint_restarts_advance_and_failed_writes_retry() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controls");
        let peer = [42; 32];
        for expected in 0..3 {
            let mut store = IndexedFileConsentStore::open(&path, "pw").unwrap();
            assert_eq!(store.offer_scan_position(&peer).unwrap(), expected);
            store.fail_writes = true;
            assert!(
                store
                    .advance_offer_scan(&peer, expected, expected + 1)
                    .is_err()
            );
            assert_eq!(store.offer_scan_position(&peer).unwrap(), expected);
            store.fail_writes = false;
            store
                .advance_offer_scan(&peer, expected, expected + 1)
                .unwrap();
            let commits = store.source.profile().database().successful_write_commits();
            store.advance_offer_scan(&peer, expected + 1, 90).unwrap();
            assert_eq!(store.offer_scan_position(&peer).unwrap(), 90);
            assert_eq!(
                store.source.profile().database().successful_write_commits(),
                commits,
                "subsequent runtime rotation must not checkpoint again immediately"
            );
        }
    }

    #[test]
    fn rotating_offer_checkpoint_due_failure_preserves_runtime_and_retries() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controls");
        let peer = [42; 32];
        let mut store = IndexedFileConsentStore::open(&path, "pw").unwrap();
        store.advance_offer_scan(&peer, 0, 1).unwrap();
        store.fail_writes = true;
        store.advance_offer_scan(&peer, 1, 2).unwrap();
        assert_eq!(store.offer_scan_position(&peer).unwrap(), 2);
        store.offer_scans.get_mut(&peer).unwrap().checkpoint_at -= OFFER_SCAN_CHECKPOINT_INTERVAL;
        assert!(store.advance_offer_scan(&peer, 2, 3).is_err());
        assert_eq!(store.offer_scan_position(&peer).unwrap(), 2);
        store.fail_writes = false;
        store.advance_offer_scan(&peer, 2, 3).unwrap();
        drop(store);
        let reopened = IndexedFileConsentStore::open(&path, "pw").unwrap();
        assert_eq!(reopened.offer_scan_position(&peer).unwrap(), 3);
    }

    #[test]
    fn rotating_offer_checkpoint_recovers_from_another_writer_and_fences_barrier() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controls");
        let peer = [42; 32];
        let mut first = IndexedFileConsentStore::open(&path, "pw").unwrap();
        first.advance_offer_scan(&peer, 0, 1).unwrap();
        first.advance_offer_scan(&peer, 1, 90).unwrap();
        let mut second =
            IndexedFileConsentStore::from_profile(first.source.profile().clone(), &path, "pw")
                .unwrap();
        second.advance_offer_scan(&peer, 1, 2).unwrap();
        assert!(matches!(
            first.advance_offer_scan(&peer, 90, 91),
            Err(LogError::StaleCursor)
        ));
        assert_eq!(first.offer_scan_position(&peer).unwrap(), 2);
        first.advance_offer_scan(&peer, 2, 3).unwrap();
        assert_eq!(first.offer_scan_position(&peer).unwrap(), 3);
        let barrier = std::fs::read(&path).unwrap();
        std::fs::write(&path, vec![0; barrier.len()]).unwrap();
        assert!(first.offer_scan_position(&peer).is_err());
        assert!(first.advance_offer_scan(&peer, 3, 4).is_err());
        std::fs::write(&path, barrier).unwrap();
        assert_eq!(first.offer_scan_position(&peer).unwrap(), 3);
    }

    #[test]
    fn rotating_offer_checkpoint_cache_bounds_peers_without_losing_restart_progress() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controls");
        let mut store = IndexedFileConsentStore::open(&path, "pw").unwrap();
        for number in 0..=OFFER_SCAN_RUNTIME_LIMIT {
            let mut peer = [0; 32];
            peer[..8].copy_from_slice(&(number as u64).to_be_bytes());
            store.advance_offer_scan(&peer, 0, 1).unwrap();
            store.advance_offer_scan(&peer, 1, 90).unwrap();
            assert!(store.offer_scans.len() <= OFFER_SCAN_RUNTIME_LIMIT);
        }
        // The evicted first peer resumes from its durable hint, and its next
        // admitted page immediately checkpoints, preserving restart fairness.
        let peer = [0; 32];
        assert_eq!(store.offer_scan_position(&peer).unwrap(), 1);
        store.advance_offer_scan(&peer, 1, 2).unwrap();
        assert_eq!(store.offer_scans.len(), OFFER_SCAN_RUNTIME_LIMIT);
        drop(store);
        let reopened = IndexedFileConsentStore::open(&path, "pw").unwrap();
        assert_eq!(reopened.offer_scan_position(&peer).unwrap(), 2);
    }

    #[test]
    fn indexed_file_consent_migrates_exact_controls_and_reopens_without_frames() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controls");
        let recipient = DeviceIdentity::generate();
        let payload = payload(&recipient, 1);
        let first = payload.event(&recipient, 1).unwrap();
        let mut later = payload.clone();
        later.decision = ReceiveDecision::Declined;
        let declined = later.event(&recipient, 2).unwrap();
        let (mut legacy, _) = EncryptedRecordLog::<Record>::open(&path, "pw", MAGIC).unwrap();
        for record in [
            Record::Published(first.clone()),
            Record::Received(first.clone()),
            Record::Received(declined.clone()),
            Record::Observed {
                manifest: payload.manifest,
                decision: ReceiveDecision::Pending,
            },
        ] {
            legacy.append_durable(&record).unwrap();
        }
        drop(legacy);
        let old = IndexedFileConsentStore::open(&path, "pw").unwrap();
        assert_eq!(
            old.published_control(payload.manifest).unwrap().unwrap().id,
            first.id
        );
        assert_eq!(
            old.received_control(payload.manifest).unwrap().unwrap().id,
            declined.id
        );
        assert_eq!(
            old.remote_decision(payload.manifest).unwrap(),
            Some(ReceiveDecision::Pending)
        );
        assert_eq!(old.published_generation(payload.manifest).unwrap(), None);
        drop(old);
        crate::storage::record_log::frame_observations::reset();
        let mut reopened = IndexedFileConsentStore::open(&path, "pw").unwrap();
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0
        );
        assert!(!reopened.receive_control(first).unwrap());
        assert_eq!(
            reopened.remote_decision(payload.manifest).unwrap(),
            Some(ReceiveDecision::Pending)
        );
        assert!(reopened.receive_direct_control(declined).unwrap());
        assert_eq!(
            reopened.remote_decision(payload.manifest).unwrap(),
            Some(ReceiveDecision::Declined)
        );
    }
    #[test]
    fn indexed_file_consent_adoption_preserves_prior_work_and_fences_old_handles() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controls");
        let mut a = IndexedFileConsentStore::open(&path, "pw").unwrap();
        let profile = a.source.profile().clone();
        let recipient = DeviceIdentity::generate();
        let payload = payload(&recipient, 2);
        let event = payload.event(&recipient, 1).unwrap();
        a.receive_control(event.clone()).unwrap();
        a.publish_control(event.clone(), 8).unwrap();
        let peer = recipient.public().ed25519_pub;
        a.advance_offer_scan(&peer, 0, 91).unwrap();
        assert!(matches!(
            a.advance_offer_scan(&peer, 0, 92),
            Err(LogError::StaleCursor)
        ));
        a.offer_scans.get_mut(&peer).unwrap().checkpoint_at -= OFFER_SCAN_CHECKPOINT_INTERVAL;
        a.fail_writes = true;
        assert!(a.advance_offer_scan(&peer, 91, 92).is_err());
        a.fail_writes = false;
        assert_eq!(a.offer_scan_position(&peer).unwrap(), 91);
        let first = Account::from_secret_bytes([89; 32]);
        let second = Account::generate();
        participant::prepare_adoption(profile.database(), &first.public(), &second.public())
            .unwrap();
        participant::finish_adoption(profile.database(), &second.public()).unwrap();
        let mut b = IndexedFileConsentStore::from_profile(profile.clone(), &path, "pw").unwrap();
        assert!(matches!(
            a.remote_decision(payload.manifest),
            Err(LogError::StaleCursor)
        ));
        assert_eq!(b.remote_decision(payload.manifest).unwrap(), None);
        assert_eq!(b.offer_scan_position(&peer).unwrap(), 0);
        b.advance_offer_scan(&peer, 0, 17).unwrap();
        assert!(matches!(
            a.offer_scan_position(&peer),
            Err(LogError::StaleCursor)
        ));
        assert_eq!(b.received_control(payload.manifest).unwrap(), None);
        b.observe_remote_decision(payload.manifest, ReceiveDecision::Declined)
            .unwrap();
        participant::prepare_adoption(profile.database(), &second.public(), &first.public())
            .unwrap();
        participant::finish_adoption(profile.database(), &first.public()).unwrap();
        let resumed = IndexedFileConsentStore::from_profile(profile, &path, "pw").unwrap();
        assert_eq!(resumed.offer_scan_position(&peer).unwrap(), 91);
        assert!(matches!(
            a.advance_offer_scan(&peer, 91, 92),
            Err(LogError::StaleCursor)
        ));
        assert_eq!(
            resumed.remote_decision(payload.manifest).unwrap(),
            Some(ReceiveDecision::Accepted)
        );
        assert_eq!(
            resumed.received_control(payload.manifest).unwrap().unwrap(),
            event
        );
        assert_eq!(
            resumed.published_generation(payload.manifest).unwrap(),
            None,
            "launch-local generation must not survive reopening"
        );
        assert!(
            matches!(
                a.receive_control(payload.event(&recipient, 2).unwrap()),
                Err(LogError::StaleCursor)
            ),
            "A→B→A does not revive the first A writer"
        );
    }
    #[test]
    fn indexed_file_consent_rolls_back_control_and_quota_on_decision_failure() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controls");
        let mut store = IndexedFileConsentStore::open(&path, "pw").unwrap();
        store.limit = 1;
        let recipient = DeviceIdentity::generate();
        let first = payload(&recipient, 3);
        let event = first.event(&recipient, 1).unwrap();
        let profile = store.source.profile().clone();
        profile.database().transaction(|c|{c.execute_batch("CREATE TRIGGER reject_consent BEFORE INSERT ON file_consent_decisions BEGIN SELECT RAISE(ABORT,'injected consent failure'); END")?;Ok(())}).unwrap();
        assert!(store.receive_control(event.clone()).is_err());
        assert!(store.received_control(first.manifest).unwrap().is_none());
        profile
            .database()
            .transaction(|c| {
                c.execute_batch("DROP TRIGGER reject_consent")?;
                Ok(())
            })
            .unwrap();
        assert!(store.receive_control(event).unwrap());
        let second = payload(&recipient, 4);
        assert!(
            store
                .receive_control(second.event(&recipient, 1).unwrap())
                .is_err()
        );
        assert!(store.received_control(second.manifest).unwrap().is_none());
        assert_eq!(
            store.remote_decision(first.manifest).unwrap(),
            Some(ReceiveDecision::Accepted)
        );
        drop(store);
        drop(profile);
        let reopened = IndexedFileConsentStore::open(&path, "pw").unwrap();
        assert_eq!(
            reopened.remote_decision(first.manifest).unwrap(),
            Some(ReceiveDecision::Accepted)
        );
    }
    #[test]
    fn indexed_file_consent_equal_direct_revision_preserves_first_signed_evidence() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controls");
        let mut store = IndexedFileConsentStore::open(&path, "pw").unwrap();
        let recipient = DeviceIdentity::generate();
        let first = payload(&recipient, 5);
        let event = first.event(&recipient, 1).unwrap();
        assert!(store.receive_control(event.clone()).unwrap());
        let mut conflicting = first.clone();
        conflicting.decision = ReceiveDecision::Declined;
        assert!(
            !store
                .receive_control(conflicting.event(&recipient, 1).unwrap())
                .unwrap()
        );
        assert!(
            store
                .receive_direct_control(conflicting.event(&recipient, 1).unwrap())
                .unwrap()
        );
        assert_eq!(
            store.received_control(first.manifest).unwrap().unwrap(),
            event
        );
        assert_eq!(
            store.remote_decision(first.manifest).unwrap(),
            Some(ReceiveDecision::Declined)
        );
        drop(store);
        let reopened = IndexedFileConsentStore::open(&path, "pw").unwrap();
        assert_eq!(
            reopened.received_control(first.manifest).unwrap().unwrap(),
            event
        );
        assert_eq!(
            reopened.remote_decision(first.manifest).unwrap(),
            Some(ReceiveDecision::Declined)
        );
    }
}
