//! One encrypted connection and cooperating lease, retained by every repository.
use super::{ProfileStorageLease, manifest, publication};
use crate::{
    eventlog::{
        LogError,
        repository::{EventDomainId, EventRepository},
    },
    identity::device::PublicIdentity,
    storage::database::EncryptedDatabase,
};
use std::{
    path::Path,
    sync::{Arc, OnceLock},
};

pub(crate) const RELAY_EVENTS: EventDomainId = EventDomainId(*b"MeshRelayEvents1");
pub(crate) const RELAY_ARRIVALS: [u8; 16] = *b"MeshRelayArrive1";

/// Archive retry is distinct from bounded publication-staging progress.
#[derive(Debug)]
pub(crate) enum ArchiveCleanupOutcome {
    Complete,
    Waiting,
    Deferred(LogError),
}
#[derive(Debug)]
pub(crate) struct ProfileCleanupProgress {
    pub staging_pending: bool,
    pub archive: ArchiveCleanupOutcome,
}

// The database drops before the OS lease. No password or derived key is copied
// outside SQLCipher, and production repositories cannot own a bare database Arc.
pub(crate) struct ProfileStorage {
    database: OnceLock<EncryptedDatabase>,
    participant: Option<super::participant::ParticipantOwner>,
    lease: ProfileStorageLease,
    federation_owner_epoch: OnceLock<i64>,
    federation_cancellations: crate::postoffice::storage::federation::CancellationQueue,
}
impl ProfileStorage {
    pub(crate) fn open_participant(
        mut lease: ProfileStorageLease,
        identity: &PublicIdentity,
        account: &crate::identity::account::AccountPublic,
        events: &Path,
        sent: &Path,
        password: &str,
    ) -> Result<Arc<Self>, LogError> {
        let paths = super::participant::paths(&mut lease, account, events, sent)?;
        let owner = super::participant::ParticipantOwner {
            identity: identity.clone(),
            account: account.clone(),
            paths,
        };
        let database = OnceLock::new();
        if let Some(existing) = super::participant::open_existing(&lease, &owner, password)? {
            database
                .set(existing)
                .map_err(|_| manifest::invalid("participant database already initialized"))?;
        }
        Ok(Arc::new(Self {
            database,
            participant: Some(owner),
            lease,
            federation_owner_epoch: OnceLock::new(),
            federation_cancellations: Default::default(),
        }))
    }
    pub(crate) fn close(self) -> Result<(), LogError> {
        match self.database.into_inner() {
            Some(database) => database.checkpoint_and_close(),
            None => Err(manifest::invalid("participant bootstrap has not completed")),
        }
    }
    pub(crate) fn participant_database(&self) -> Option<&EncryptedDatabase> {
        self.database.get()
    }
    /// A producer keeps the authenticated account captured when this context
    /// opened. Check it inside the effect transaction so a retired context can
    /// never write reception work into a newly adopted account's namespace.
    pub(crate) fn check_participant_account_on(
        &self,
        connection: &rusqlite::Connection,
    ) -> Result<(), LogError> {
        let owner = self
            .participant
            .as_ref()
            .ok_or_else(|| manifest::invalid("not a participant context"))?;
        let account: Vec<u8> = connection.query_row(
            "SELECT account FROM participant_profile WHERE singleton=1",
            [],
            |row| row.get(0),
        )?;
        if account.as_slice() != owner.account.ed25519_pub.as_slice() {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    pub(crate) fn validate_participant_request(
        &self,
        identity: &PublicIdentity,
        account: &crate::identity::account::AccountPublic,
        events: &Path,
        sent: &Path,
    ) -> Result<(), LogError> {
        let owner = self
            .participant
            .as_ref()
            .ok_or_else(|| manifest::invalid("not a participant context"))?;
        let canonical = |path: &Path| -> Result<std::path::PathBuf, LogError> {
            let parent = path
                .parent()
                .filter(|p| !p.as_os_str().is_empty())
                .unwrap_or_else(|| Path::new("."));
            Ok(std::fs::canonicalize(parent)?.join(
                path.file_name()
                    .ok_or_else(|| manifest::invalid("source must name a file"))?,
            ))
        };
        if &owner.identity != identity
            || &owner.account != account
            || canonical(events)? != owner.paths.events
            || canonical(sent)? != owner.paths.sent
        {
            return Err(manifest::invalid(
                "participant inputs differ from owned context",
            ));
        }
        Ok(())
    }
    pub(crate) fn complete_participant(
        &self,
        password: &str,
        legacy_previous: Option<&str>,
    ) -> Result<(), LogError> {
        if self.database.get().is_none() {
            let owner = self
                .participant
                .as_ref()
                .ok_or_else(|| manifest::invalid("not a participant context"))?;
            let database =
                super::participant::bootstrap(&self.lease, owner, password, legacy_previous)?;
            self.database
                .set(database)
                .map_err(|_| manifest::invalid("participant database already initialized"))?;
        }
        Ok(())
    }
    pub(crate) fn validate_legacy_events(&self, password: &str) -> Result<(), LogError> {
        if self.database.get().is_none() {
            let owner = self
                .participant
                .as_ref()
                .ok_or_else(|| manifest::invalid("not a participant context"))?;
            super::participant_events::authenticate_legacy(
                self.root(),
                &owner.paths.events,
                password,
            )?;
        }
        Ok(())
    }
    pub(crate) fn maintain_participant_events(&self) -> Result<(), LogError> {
        super::participant_events::maintenance(self)
    }
    pub(crate) fn participant_events(
        self: &Arc<Self>,
        password: &str,
    ) -> Result<EventRepository, LogError> {
        let owner = self
            .participant
            .as_ref()
            .ok_or_else(|| manifest::invalid("not a participant context"))?;
        super::participant_events::open(self, &owner.paths.events, password)
    }
    pub(crate) fn open_relay(
        lease: ProfileStorageLease,
        identity: &PublicIdentity,
        path: &Path,
        password: &str,
        now: u64,
    ) -> Result<Arc<Self>, LogError> {
        let paths = publication::source_layout(lease.canonical_root(), path)?;
        let mut binding = b"DeviceProfileV1".to_vec();
        binding.extend_from_slice(&identity.ed25519_pub);
        binding.extend_from_slice(&identity.x25519_pub);
        let database = publication::open_database(&lease, &paths, password, &binding)?;
        let profile = Arc::new(Self {
            database: OnceLock::from(database),
            participant: None,
            lease,
            federation_owner_epoch: OnceLock::new(),
            federation_cancellations: Default::default(),
        });
        manifest::validate(profile.database(), &paths)?;
        EventRepository::validate_existing_domain(profile.database(), RELAY_EVENTS)?;
        publication::complete(&profile, password, now)?;
        let epoch = crate::postoffice::storage::federation::start_owner(profile.database())?;
        profile
            .federation_owner_epoch
            .set(epoch)
            .map_err(|_| manifest::invalid("federation owner already initialized"))?;
        Ok(profile)
    }
    pub(crate) fn federation_cancellations(
        &self,
    ) -> &crate::postoffice::storage::federation::CancellationQueue {
        &self.federation_cancellations
    }
    pub(crate) fn federation_owner_epoch(&self) -> Result<i64, LogError> {
        self.federation_owner_epoch
            .get()
            .copied()
            .ok_or_else(|| manifest::invalid("federation owner is not started"))
    }
    pub(crate) fn database(&self) -> &EncryptedDatabase {
        self.database
            .get()
            .expect("indexed repositories require initialized profile storage")
    }
    pub(crate) fn root(&self) -> &Path {
        self.lease.canonical_root()
    }
    pub(crate) fn events(self: &Arc<Self>) -> Result<EventRepository, LogError> {
        manifest::require_published(self.database())?;
        EventRepository::from_profile(self.clone(), RELAY_EVENTS)
    }
    pub(crate) fn cleanup_pending(&self) -> Result<bool, LogError> {
        self.database().read(|c| {
            Ok(c.query_row(
                "SELECT staging_pending OR EXISTS(SELECT 1 FROM relay_sources WHERE cleanup_pending=1) FROM relay_publication WHERE singleton=1",
                [], |r| r.get(0),
            )?)
        })
    }
    /// Staging always receives one bounded step. Only archive retry is gated;
    /// authentication and authoritative failures never become deferred success.
    pub(crate) fn cleanup_step(
        &self,
        archive_due: bool,
    ) -> Result<ProfileCleanupProgress, LogError> {
        publication::resume_cleanup_step(self, archive_due)
    }
    pub(super) fn archive_cleanup_pending(&self) -> Result<bool, LogError> {
        self.database().read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM relay_sources WHERE cleanup_pending=1)",
                [],
                |r| r.get(0),
            )?)
        })
    }
}
