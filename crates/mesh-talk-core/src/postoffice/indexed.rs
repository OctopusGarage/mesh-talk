//! Dedicated relay facade. All history access is bounded and policy qualified.
pub use super::storage::role::network::{
    IndexedRelayExportCursor, IndexedRelayHaveCursor, IndexedRelayView,
};
use super::storage::role::{IndexedRelayState, RelayAdmission};
use crate::{
    eventlog::{
        AppendOutcome, ConversationId, Event, EventId, LogError,
        sync::{FileConsentStatus, network::*},
    },
    identity::device::DeviceIdentity,
    storage::profile::{ProfileStorageLease, context::ProfileStorage},
};
use std::{collections::HashSet, path::Path, sync::Arc};

#[derive(Clone)]
pub struct IndexedPostOffice {
    pub(crate) state: Arc<IndexedRelayState>,
}
impl IndexedPostOffice {
    /// Consumes the caller's existing profile lease; never reacquires node.lock.
    pub fn open_with_lease(
        lease: ProfileStorageLease,
        path: &Path,
        password: &str,
        identity: DeviceIdentity,
    ) -> Result<Self, LogError> {
        let profile = ProfileStorage::open_relay(
            lease,
            &identity.public(),
            path,
            password,
            super::network::unix_now(),
        )?;
        Ok(Self {
            state: Arc::new(IndexedRelayState::from_profile(
                profile,
                RelayAdmission::OpaqueOnly,
            )?),
        })
    }
    pub fn subscribe_changes(&self) -> tokio::sync::watch::Receiver<u64> {
        self.state.subscribe_changes()
    }
}
impl NetworkSyncStore for IndexedPostOffice {
    type View = IndexedRelayView;
    type HaveCursor = IndexedRelayHaveCursor;
    type ExportCursor = IndexedRelayExportCursor;
    fn open_view(&self, conv: &ConversationId) -> Result<Self::View, LogError> {
        self.state.network_view_at(conv, super::network::unix_now())
    }
    fn metadata(&self, view: &Self::View) -> Result<SyncMetadata, LogError> {
        self.state
            .network_metadata_at(view, super::network::unix_now())
    }
    fn validate_view(&self, view: &Self::View) -> Result<(), LogError> {
        self.state
            .network_validate_at(view, super::network::unix_now())
    }
    fn have_page(
        &self,
        view: &Self::View,
        after: Option<&Self::HaveCursor>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, Self::HaveCursor>, LogError> {
        self.state
            .network_have_at(view, after, budget, super::network::unix_now())
    }
    fn export_page(
        &self,
        view: &Self::View,
        have: &HashSet<EventId>,
        after: Option<&Self::ExportCursor>,
        budget: ExportPageBudget,
    ) -> Result<ExportPage<Self::ExportCursor>, LogError> {
        self.state
            .network_export_at(view, have, after, budget, super::network::unix_now())
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.state.record(&event, super::network::unix_now(), None)
    }
    fn durable_have(&self, conv: &ConversationId, id: &EventId) -> Result<bool, LogError> {
        self.state
            .durable_have_at(conv, id, super::network::unix_now())
    }
    fn file_consent(&self, _: &ConversationId, _: &EventId) -> Result<FileConsentStatus, LogError> {
        Ok(FileConsentStatus::Pending)
    }
    fn signed_file_consent(
        &self,
        _: &ConversationId,
        _: &EventId,
    ) -> Result<Option<Event>, LogError> {
        Ok(None)
    }
    fn admission_denied(&self) -> bool {
        false
    }
}

/// Session-owned network authority. It contains no inventory collection and its
/// cloned lease cannot extend the separately owned exchange lifetime.
pub(crate) struct IndexedFederationStore {
    office: IndexedPostOffice,
    session: super::storage::federation::SessionLease,
}
impl IndexedFederationStore {
    pub(crate) fn new(
        office: &IndexedPostOffice,
        session: super::storage::federation::SessionLease,
    ) -> Self {
        Self {
            office: office.clone(),
            session,
        }
    }
    fn check_view(&self, view: &IndexedRelayView) -> Result<(), LogError> {
        IndexedRelayState::network_session_matches(view, &self.session)
    }
}
impl NetworkSyncStore for IndexedFederationStore {
    type View = IndexedRelayView;
    type HaveCursor = IndexedRelayHaveCursor;
    type ExportCursor = IndexedRelayExportCursor;
    fn open_view(&self, conv: &ConversationId) -> Result<Self::View, LogError> {
        self.office.state.network_view_with_session_at(
            conv,
            super::network::unix_now(),
            Some(&self.session),
        )
    }
    fn metadata(&self, view: &Self::View) -> Result<SyncMetadata, LogError> {
        self.check_view(view)?;
        self.office.metadata(view)
    }
    fn validate_view(&self, view: &Self::View) -> Result<(), LogError> {
        self.check_view(view)?;
        self.office.validate_view(view)
    }
    fn have_page(
        &self,
        view: &Self::View,
        after: Option<&Self::HaveCursor>,
        budget: IdPageBudget,
    ) -> Result<SyncPage<EventId, Self::HaveCursor>, LogError> {
        self.check_view(view)?;
        self.office.have_page(view, after, budget)
    }
    fn export_page(
        &self,
        view: &Self::View,
        have: &HashSet<EventId>,
        after: Option<&Self::ExportCursor>,
        budget: ExportPageBudget,
    ) -> Result<ExportPage<Self::ExportCursor>, LogError> {
        self.check_view(view)?;
        self.office.export_page(view, have, after, budget)
    }
    fn ingest(&mut self, event: Event) -> Result<AppendOutcome, LogError> {
        self.office
            .state
            .record_with_session_at(&event, super::network::unix_now(), &self.session)
    }
    fn durable_have(&self, conv: &ConversationId, id: &EventId) -> Result<bool, LogError> {
        self.office
            .state
            .session_custody_at(&self.session, conv, id, super::network::unix_now())
    }
    fn file_consent(
        &self,
        conv: &ConversationId,
        _: &EventId,
    ) -> Result<FileConsentStatus, LogError> {
        self.office
            .state
            .session_consent_at(&self.session, conv, super::network::unix_now())?;
        Ok(FileConsentStatus::Pending)
    }
    fn signed_file_consent(
        &self,
        conv: &ConversationId,
        _: &EventId,
    ) -> Result<Option<Event>, LogError> {
        self.office
            .state
            .session_consent_at(&self.session, conv, super::network::unix_now())?;
        Ok(None)
    }
    fn admission_denied(&self) -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn federation_store_cancellation_revokes_every_network_endpoint() {
        use crate::postoffice::storage::federation::{Direction, SessionOwner};
        use std::time::Instant;
        let root = tempfile::tempdir().unwrap();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let owner = SessionOwner::begin(
            office.state.profile().clone(),
            Direction::Outbound,
            super::super::network::unix_now(),
            Instant::now(),
        )
        .unwrap();
        owner
            .lease()
            .receive_page(&[], true, super::super::network::unix_now())
            .unwrap();
        let mut store = IndexedFederationStore::new(&office, owner.lease());
        let conv = ConversationId::new([1; 32]);
        let view = store.open_view(&conv).unwrap();
        drop(owner);
        assert!(
            store.metadata(&view).is_err(),
            "cancelled authority must revoke metadata"
        );
        assert!(store.validate_view(&view).is_err());
        assert!(
            store
                .have_page(
                    &view,
                    None,
                    IdPageBudget {
                        ids: 1,
                        examined: 1
                    }
                )
                .is_err()
        );
        assert!(
            store
                .export_page(
                    &view,
                    &HashSet::new(),
                    None,
                    ExportPageBudget {
                        examined: 1,
                        events: 1,
                        encoded_bytes: 65519,
                        allocated_bytes: 65519
                    }
                )
                .is_err()
        );
        assert!(store.open_view(&conv).is_err());
        assert!(store.durable_have(&conv, &EventId::new([2; 32])).is_err());
        assert!(store.file_consent(&conv, &EventId::new([2; 32])).is_err());
        assert!(
            store
                .signed_file_consent(&conv, &EventId::new([2; 32]))
                .is_err()
        );
        let event = Event::new(
            &DeviceIdentity::generate(),
            conv,
            1,
            vec![],
            1,
            1,
            crate::eventlog::EventKind::Message,
            vec![1],
        );
        assert!(store.ingest(event).is_err());
    }
    #[test]
    fn authoritative_write_error_survives_simultaneous_session_authority_loss() {
        use crate::postoffice::storage::federation::{Direction, SessionOwner};
        let root = tempfile::tempdir().unwrap();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let inner = Event::new(
            &author,
            ConversationId::new([4; 32]),
            1,
            vec![],
            1,
            1,
            crate::eventlog::EventKind::Message,
            vec![42],
        );
        let event =
            crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public())
                .unwrap()
                .wrap(&inner)
                .unwrap();
        let now = super::super::network::unix_now();
        let owner = SessionOwner::begin(
            office.state.profile().clone(),
            Direction::Outbound,
            now,
            std::time::Instant::now(),
        )
        .unwrap();
        let lease = owner.lease();
        lease.receive_page(&[], true, now).unwrap();
        office.state.profile().database().transaction(|c| {
            c.execute_batch("CREATE TRIGGER fail_session_insert AFTER INSERT ON retained_events BEGIN UPDATE relay_federation_sessions SET live=0; SELECT RAISE(FAIL,'authoritative-write-fixture'); END;")?;
            Ok(())
        }).unwrap();
        let mut store = IndexedFederationStore::new(&office, lease.clone());
        assert!(
            matches!(store.ingest(event.clone()), Err(LogError::Database(_))),
            "original database failure must survive authority loss"
        );
        assert_eq!(office.state.physical_charge().unwrap().event_count, 0);
        assert!(
            !office
                .durable_have(&event.conversation_id, &event.id)
                .unwrap()
        );
        office
            .state
            .profile()
            .database()
            .read(|c| lease.validate_on(c, true))
            .unwrap();
    }
    #[test]
    fn expected_local_unavailability_rolls_back_if_session_authority_is_lost() {
        use crate::postoffice::storage::federation::{Direction, SessionOwner};
        let root = tempfile::tempdir().unwrap();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let now = super::super::network::unix_now();
        let owner = SessionOwner::begin(
            office.state.profile().clone(),
            Direction::Outbound,
            now,
            std::time::Instant::now(),
        )
        .unwrap();
        let lease = owner.lease();
        lease.receive_page(&[], true, now).unwrap();
        let result: Result<(), LogError> =
            office.state.session_transaction(now, Some(&lease), |c, _| {
                c.execute("UPDATE relay_federation_sessions SET live=0", [])?;
                c.execute(
                    "UPDATE relay_runtime SET next_touch=x'0000000000000009'",
                    [],
                )?;
                Err(LogError::RelayCapacityUnavailable)
            });
        assert!(matches!(result, Err(LogError::AdmissionRejected(_))));
        office
            .state
            .profile()
            .database()
            .read(|c| {
                lease.validate_on(c, true)?;
                let touch: Vec<u8> =
                    c.query_row("SELECT next_touch FROM relay_runtime", [], |r| r.get(0))?;
                assert_eq!(touch, 1u64.to_be_bytes());
                Ok(())
            })
            .unwrap();
    }
    #[test]
    fn constructor_consumes_one_lease_and_clones_hold_it_until_last_drop() {
        let root = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let (ed, x) = identity.secret_bytes();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            identity,
        )
        .unwrap();
        let clone = office.clone();
        drop(office);
        assert!(ProfileStorageLease::acquire(root.path()).is_err());
        drop(clone);
        let lease = ProfileStorageLease::acquire(root.path()).unwrap();
        assert!(
            IndexedPostOffice::open_with_lease(
                lease,
                &root.path().join("relay.log"),
                "wrong password",
                DeviceIdentity::from_secret_bytes(ed, x)
            )
            .is_err()
        );
        assert!(
            ProfileStorageLease::acquire(root.path()).is_ok(),
            "constructor errors must release the transferred lease"
        );
    }
    fn authority_fixture() -> (tempfile::TempDir, IndexedPostOffice, Event) {
        let root = tempfile::tempdir().unwrap();
        let office = IndexedPostOffice::open_with_lease(
            ProfileStorageLease::acquire(root.path()).unwrap(),
            &root.path().join("relay.log"),
            "pw",
            DeviceIdentity::generate(),
        )
        .unwrap();
        let author = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        let event =
            crate::postoffice::opaque::RelayMailbox::federated(&author, &recipient.public())
                .unwrap()
                .wrap(&Event::new(
                    &author,
                    ConversationId::new([9; 32]),
                    1,
                    vec![],
                    1,
                    0,
                    crate::eventlog::EventKind::Message,
                    vec![1],
                ))
                .unwrap();
        office.state.record(&event, 100, None).unwrap();
        (root, office, event)
    }
    type AuthoritySnapshot = (Option<Vec<u8>>, i64, i64, Vec<u8>, i64, i64);
    fn authority_snapshot(office: &IndexedPostOffice) -> AuthoritySnapshot {
        office.state.profile().database().read(|c|Ok(c.query_row("SELECT expired_cutoff,inventory_visibility_epoch,(SELECT queued FROM relay_digest_preparation),next_touch,(SELECT sum(event_count) FROM event_conversations),(SELECT count(*) FROM relay_federation_local_entries) FROM relay_runtime",[],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?)))?)).unwrap()
    }
    #[test]
    fn initially_lost_session_authority_rolls_back_due_policy_across_every_role_boundary() {
        use crate::postoffice::storage::federation::{Direction, SessionOwner};
        use std::time::{Duration, Instant};
        for expired in [false, true] {
            for boundary in [
                "network",
                "record",
                "origins",
                "inventory",
                "frame",
                "mismatch_begin",
                "mismatch_step",
            ] {
                let (_root, office, event) = authority_fixture();
                let view = office.state.inventory_view_at(100).unwrap();
                let started = if expired {
                    Instant::now() - Duration::from_secs(31)
                } else {
                    Instant::now()
                };
                let owner = SessionOwner::begin(
                    office.state.profile().clone(),
                    Direction::Outbound,
                    100,
                    started,
                )
                .unwrap();
                let lease = owner.lease();
                if !expired {
                    lease.receive_page(&[], true, 100).unwrap();
                }
                let retained_owner = if expired {
                    Some(owner)
                } else {
                    drop(owner);
                    None
                };
                let before = authority_snapshot(&office);
                let now = 100 + super::super::MAX_RELAY_AGE.as_secs();
                let result = match boundary {
                    "network" => office
                        .state
                        .network_view_with_session_at(&event.conversation_id, now, Some(&lease))
                        .map(|_| ()),
                    "record" => office
                        .state
                        .record_with_session_at(&event, now, &lease)
                        .map(|_| ()),
                    "origins" => office
                        .state
                        .correct_known_origins_at(&lease, None, 512, now)
                        .map(|_| ()),
                    "inventory" => office
                        .state
                        .selected_inventory_page_at(&view, &lease, None, 512, now)
                        .map(|_| ()),
                    "frame" => office
                        .state
                        .finish_inventory_frame_at(&view, &lease, 0, true, now),
                    "mismatch_begin" => office
                        .state
                        .begin_inventory_mismatch_at(&view, &lease, None, now),
                    _ => office
                        .state
                        .inventory_mismatch_step_at(&view, &lease, 512, 16, now)
                        .map(|_| ()),
                };
                assert_eq!(
                    authority_snapshot(&office),
                    before,
                    "{boundary}: initially canceled/expired authority must roll back cutoff, visibility and derived mutations (expired={expired})"
                );
                assert!(
                    matches!(result, Err(LogError::AdmissionRejected(_))),
                    "{boundary}: lost session authority must cause rollback: {result:?}"
                );
                drop(retained_owner);
            }
        }
    }
    #[test]
    fn cancelled_missing_cache_inventory_cannot_commit_digest_demand() {
        use crate::postoffice::storage::federation::{Direction, SessionOwner};
        let (_root, office, _event) = authority_fixture();
        let view = office.state.inventory_view_at(100).unwrap();
        let owner = SessionOwner::begin(
            office.state.profile().clone(),
            Direction::Outbound,
            100,
            std::time::Instant::now(),
        )
        .unwrap();
        let lease = owner.lease();
        drop(owner);
        let before = authority_snapshot(&office);
        let result = office
            .state
            .selected_inventory_page_at(&view, &lease, None, 512, 100);
        assert_eq!(
            authority_snapshot(&office),
            before,
            "canceled cache miss must not commit derived demand"
        );
        assert!(matches!(result, Err(LogError::AdmissionRejected(_))));
    }
}
