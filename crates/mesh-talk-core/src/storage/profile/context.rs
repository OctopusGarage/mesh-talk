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
use std::{path::Path, sync::Arc};

pub(crate) const RELAY_EVENTS: EventDomainId = EventDomainId(*b"MeshRelayEvents1");
pub(crate) const RELAY_ARRIVALS: [u8; 16] = *b"MeshRelayArrive1";

// The database drops before the OS lease. No password or derived key is copied
// outside SQLCipher, and production repositories cannot own a bare database Arc.
pub(crate) struct ProfileStorage {
    database: EncryptedDatabase,
    lease: ProfileStorageLease,
}
impl ProfileStorage {
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
        let profile = Arc::new(Self { database, lease });
        manifest::validate(&profile.database, &paths)?;
        EventRepository::validate_existing_domain(&profile.database, RELAY_EVENTS)?;
        publication::complete(&profile, password, now)?;
        Ok(profile)
    }
    pub(crate) fn database(&self) -> &EncryptedDatabase {
        &self.database
    }
    pub(crate) fn root(&self) -> &Path {
        self.lease.canonical_root()
    }
    pub(crate) fn events(self: &Arc<Self>) -> Result<EventRepository, LogError> {
        manifest::require_published(&self.database)?;
        EventRepository::from_profile(self.clone(), RELAY_EVENTS)
    }
    pub(crate) fn cleanup_pending(&self) -> Result<bool, LogError> {
        self.database.read(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM relay_sources WHERE cleanup_pending=1)",
                [],
                |r| r.get(0),
            )?)
        })
    }
}
