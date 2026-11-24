#![cfg(feature = "legacy-wire")]

use mesh_identity::device::DeviceIdentity;
use mesh_replication::legacy_round::{ResponderBackend, serve};
use mesh_replication::{
    legacy_event::{ConversationId, Event, EventId, EventKind},
    legacy_round::{
        ApplyReport, HaveSender, Probe, QuietComparison, RoundBackend, RoundFault, SyncMetadata,
        SyncProgress,
    },
    legacy_wire::{self, FpResponse, HaveChunk, SyncResponse, SyncWire},
    session::{HaveError, HaveLimits, HavePage, HaveSource, ReconciliationSession},
};
use std::{
    cell::RefCell,
    collections::{HashSet, VecDeque},
    rc::Rc,
};

#[derive(Default)]
struct Connection {
    incoming: VecDeque<Vec<u8>>,
    sent: Vec<Vec<u8>>,
}
impl ReconciliationSession for Connection {
    type Peer = ();
    type Error = RoundFault;

    fn peer_identity(&self) -> &Self::Peer {
        &()
    }
    async fn send(&mut self, bytes: &[u8]) -> Result<(), RoundFault> {
        self.sent.push(bytes.to_vec());
        Ok(())
    }
    async fn recv(&mut self) -> Result<Vec<u8>, RoundFault> {
        self.incoming
            .pop_front()
            .ok_or(RoundFault::UnexpectedMessage)
    }
}

#[derive(Default)]
struct Replica {
    fingerprint: Option<[u8; 32]>,
    accepted: RefCell<HashSet<EventId>>,
    exports: Vec<Event>,
    deny_inventory: bool,
    reservation: Rc<()>,
}
struct FixtureHave(Vec<EventId>);

impl HaveSource for FixtureHave {
    type Id = EventId;
    type Cursor = usize;
    type Error = RoundFault;

    fn page(
        &mut self,
        cursor: Option<&usize>,
        limit: usize,
        _: usize,
    ) -> Result<HavePage<EventId, usize>, RoundFault> {
        let start = cursor.copied().unwrap_or(0);
        let end = (start + limit).min(self.0.len());
        Ok(HavePage {
            ids: self.0[start..end].to_vec(),
            next: (end < self.0.len()).then_some(end),
        })
    }

    fn validate(&self) -> Result<(), RoundFault> {
        Ok(())
    }
}

impl RoundBackend<RoundFault> for Replica {
    type Peer = ();
    type View = ();
    type RemoteHave = Box<HashSet<EventId>>;

    fn bind_session_peer(&self, _: &()) {}
    async fn prepared_probe(&self, _: &ConversationId) -> Result<Probe<()>, RoundFault> {
        Ok((
            SyncMetadata {
                count: self.accepted.borrow().len() as u64,
                fingerprint: self.fingerprint,
                ..SyncMetadata::default()
            },
            None,
        ))
    }
    async fn open_prepared_view(&self, _: &ConversationId) -> Result<(), RoundFault> {
        Ok(())
    }
    fn advertised_count(&self, _: &()) -> Result<usize, RoundFault> {
        Ok(self.accepted.borrow().len())
    }
    fn have_count(&self, count: u64) -> Result<usize, RoundFault> {
        usize::try_from(count).map_err(|_| RoundFault::CounterExhausted)
    }
    fn validate_view(&self, _: &()) -> Result<(), RoundFault> {
        Ok(())
    }
    fn reusable_view(&self, _: &()) -> Result<bool, RoundFault> {
        Ok(true)
    }
    fn retain_custody_view(&self, _: ()) -> Result<Option<()>, RoundFault> {
        Ok(None)
    }
    fn quiet_comparison(&self, _: &ConversationId) -> Result<Option<QuietComparison>, RoundFault> {
        Ok(None)
    }
    fn validate_quiet_comparison(
        &self,
        _: &ConversationId,
        _: QuietComparison,
    ) -> Result<bool, RoundFault> {
        Ok(false)
    }
    fn quiet_comparison_token(&self, _: &()) -> Result<Option<[u8; 32]>, RoundFault> {
        Ok(None)
    }
    fn retain_quiet_comparison(&self, _: &(), _: QuietComparison) -> Result<(), RoundFault> {
        Ok(())
    }
    type HaveReservation = Rc<()>;
    type HaveCursor = usize;

    fn have_limits(&self) -> HaveLimits {
        HaveLimits {
            ids_per_frame: 32,
            frames: 8,
            total_ids: 256,
        }
    }

    fn have_source<'a>(
        &'a self,
        _: &'a (),
    ) -> impl HaveSource<Id = EventId, Cursor = Self::HaveCursor, Error = RoundFault> + 'a {
        FixtureHave(self.accepted.borrow().iter().copied().collect())
    }

    fn reserve_remote_have(&self) -> Result<Self::HaveReservation, RoundFault> {
        if self.deny_inventory {
            return Err(RoundFault::NoProgress);
        }
        Ok(Rc::clone(&self.reservation))
    }

    fn collect_remote_have(
        &self,
        ids: Vec<EventId>,
        _: Self::HaveReservation,
        _: bool,
    ) -> Result<Self::RemoteHave, RoundFault> {
        Ok(Box::new(ids.into_iter().collect()))
    }

    fn have_error(&self, error: HaveError<RoundFault>) -> RoundFault {
        match error {
            HaveError::Exchange(error) => error,
            _ => RoundFault::UnexpectedMessage,
        }
    }

    async fn yield_inventory_work(&self) {}

    async fn intersection_count(
        &self,
        _: &(),
        remote: &Self::RemoteHave,
    ) -> Result<usize, RoundFault> {
        Ok(self.accepted.borrow().intersection(remote).count())
    }
    fn comparison_fingerprint(&self, _: &Self::RemoteHave) -> Option<[u8; 32]> {
        None
    }
    fn apply(&self, events: &[Event]) -> Result<ApplyReport, RoundFault> {
        let mut report = ApplyReport::default();
        for event in events {
            if !event.verify_integrity() || !event.verify_signature() {
                return Err(RoundFault::UnexpectedMessage);
            }
            if self.accepted.borrow_mut().insert(event.id) {
                report.applied += 1;
            } else {
                report.duplicates += 1;
            }
        }
        Ok(report)
    }
    fn admission_denied(&self) -> bool {
        false
    }
    async fn select_export_batches(
        &self,
        _: &(),
        have: &Self::RemoteHave,
        _: usize,
        _: usize,
    ) -> Result<Vec<Vec<Event>>, RoundFault> {
        let events = self
            .exports
            .iter()
            .filter(|event| !have.contains(&event.id))
            .cloned()
            .collect::<Vec<_>>();
        Ok(if events.is_empty() {
            vec![]
        } else {
            vec![events]
        })
    }
}

#[tokio::test]
async fn file_identity_exchange_checks_both_budgets_and_preserves_scope() {
    use mesh_replication::legacy_round::MAX_FILE_IDENTITIES;

    let scope = ConversationId::new([7; 32]);
    let event = Event::new(
        &DeviceIdentity::generate(),
        scope,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![],
    );
    let replica = Replica::default();
    let mut channel = Connection::default();

    for (requests, offers) in [
        (vec![(scope, event.id); MAX_FILE_IDENTITIES + 1], vec![]),
        (vec![], vec![event.clone(); MAX_FILE_IDENTITIES + 1]),
    ] {
        let request =
            legacy_wire::encode(&SyncWire::FileMessageIdentitiesRequest { requests, offers })
                .unwrap();
        assert_eq!(
            serve(&mut channel, &replica, &request).await,
            Err(RoundFault::UnexpectedMessage)
        );
        assert!(channel.sent.is_empty());
    }

    let request = legacy_wire::encode(&SyncWire::FileMessageIdentitiesRequest {
        requests: vec![],
        offers: vec![event],
    })
    .unwrap();
    assert_eq!(serve(&mut channel, &replica, &request).await, Ok(scope));
    let [response] = channel.sent.as_slice() else {
        panic!("expected one response");
    };
    assert!(matches!(
        legacy_wire::decode(response).unwrap(),
        SyncWire::FileMessageIdentitiesResponse { bundles } if bundles.is_empty()
    ));
}

fn incoming(frames: impl IntoIterator<Item = SyncWire>) -> Connection {
    Connection {
        incoming: frames
            .into_iter()
            .map(|frame| legacy_wire::encode(&frame).unwrap())
            .collect(),
        sent: Vec::new(),
    }
}

#[tokio::test]
async fn matching_fingerprint_exits_without_inventory_or_export() {
    let replica = Replica {
        fingerprint: Some([19; 32]),
        ..Replica::default()
    };
    let mut connection = incoming([SyncWire::FpResponse(FpResponse { matched: true })]);
    let mut progress = SyncProgress::default();
    assert!(
        progress
            .step(&mut connection, &replica, ConversationId::new([7; 32]), 1)
            .await
            .unwrap()
    );
    assert_eq!(connection.sent.len(), 1);
    assert!(matches!(
        legacy_wire::decode(&connection.sent[0]).unwrap(),
        SyncWire::FpRequest(_)
    ));
}

#[tokio::test]
async fn a_reconnected_requester_advertises_the_event_already_committed() {
    let scope = ConversationId::new([7; 32]);
    let event = Event::new(
        &DeviceIdentity::generate(),
        scope,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![1],
    );
    let response_have = || {
        SyncWire::RespHave(HaveChunk {
            conversation: scope,
            ids: vec![event.id],
            more: false,
        })
    };
    let replica = Replica::default();
    let mut first = incoming([
        SyncWire::Response(SyncResponse {
            conversation: scope,
            events: vec![event.clone()],
            have: vec![],
        }),
        response_have(),
    ]);
    assert!(
        !SyncProgress::default()
            .step(&mut first, &replica, scope, 1)
            .await
            .unwrap()
    );
    assert!(replica.accepted.borrow().contains(&event.id));
    let mut second = incoming([
        SyncWire::Response(SyncResponse {
            conversation: scope,
            events: vec![],
            have: vec![],
        }),
        response_have(),
    ]);
    assert!(
        SyncProgress::default()
            .step(&mut second, &replica, scope, 1)
            .await
            .unwrap()
    );
    let sent = second
        .sent
        .into_iter()
        .map(|bytes| legacy_wire::decode(&bytes).unwrap())
        .collect::<Vec<_>>();
    assert!(
        matches!(&sent[..], [SyncWire::Request(_), SyncWire::ReqHave(have), SyncWire::Followup(_)]
        if have.ids == [event.id] && !have.more)
    );
}

impl ResponderBackend<RoundFault> for Replica {
    fn metadata(&self, _: &()) -> Result<SyncMetadata, RoundFault> {
        Ok(SyncMetadata {
            count: self.accepted.borrow().len() as u64,
            ..SyncMetadata::default()
        })
    }

    fn durable_have_in_view(
        &self,
        _: &(),
        _: &ConversationId,
        id: &EventId,
    ) -> Result<bool, RoundFault> {
        Ok(self.accepted.borrow().contains(id))
    }
}

#[tokio::test]
async fn responder_exports_only_missing_ids_and_finishes_its_inventory() {
    use mesh_replication::legacy_wire::SyncRequest;

    let scope = ConversationId::new([7; 32]);
    let author = DeviceIdentity::generate();
    let first = Event::new(&author, scope, 1, vec![], 1, 0, EventKind::Message, vec![1]);
    let missed = Event::new(
        &author,
        scope,
        2,
        vec![first.id],
        2,
        0,
        EventKind::Message,
        vec![2],
    );
    let replica = Replica {
        exports: vec![first.clone(), missed.clone()],
        accepted: RefCell::new([first.id, missed.id].into_iter().collect()),
        ..Replica::default()
    };
    let mut channel = incoming([SyncWire::ReqHave(HaveChunk {
        conversation: scope,
        ids: vec![first.id],
        more: false,
    })]);
    let request = legacy_wire::encode(&SyncWire::Request(SyncRequest {
        conversation: scope,
        have: vec![],
    }))
    .unwrap();
    assert_eq!(serve(&mut channel, &replica, &request).await, Ok(scope));
    let frames = channel
        .sent
        .into_iter()
        .map(|bytes| legacy_wire::decode(&bytes).unwrap())
        .collect::<Vec<_>>();
    let [SyncWire::Response(response), SyncWire::RespHave(have)] = frames.as_slice() else {
        panic!("expected response and Have")
    };
    assert_eq!(response.events.as_slice(), std::slice::from_ref(&missed));
    assert!(response.have.is_empty());
    assert!(!have.more);
    assert_eq!(
        have.ids.iter().copied().collect::<HashSet<_>>(),
        HashSet::from([first.id, missed.id])
    );
}

#[test]
fn have_codec_checks_role_and_scope() {
    let scope = ConversationId::new([1; 32]);
    let foreign = ConversationId::new([2; 32]);
    let sender = HaveSender::Responder;
    let bytes = sender
        .encode(
            scope,
            mesh_replication::session::HaveFrame {
                ids: Vec::new(),
                more: true,
            },
        )
        .unwrap();
    assert!(sender.decode(scope, &bytes).unwrap().more);
    assert!(matches!(
        sender.decode(foreign, &bytes),
        Err(RoundFault::UnexpectedMessage)
    ));
    assert!(matches!(
        HaveSender::Requester.decode(scope, &bytes),
        Err(RoundFault::UnexpectedMessage)
    ));
}

#[tokio::test]
async fn responder_reserves_before_reading_and_releases_on_invalid_inventory() {
    use mesh_replication::legacy_wire::SyncRequest;

    let scope = ConversationId::new([7; 32]);
    let request = legacy_wire::encode(&SyncWire::Request(SyncRequest {
        conversation: scope,
        have: vec![],
    }))
    .unwrap();
    let mut channel = incoming([SyncWire::RespHave(HaveChunk {
        conversation: scope,
        ids: vec![],
        more: false,
    })]);
    let mut replica = Replica {
        deny_inventory: true,
        ..Replica::default()
    };

    assert_eq!(
        serve(&mut channel, &replica, &request).await,
        Err(RoundFault::NoProgress)
    );
    assert_eq!(channel.incoming.len(), 1);
    assert!(channel.sent.is_empty());

    replica.deny_inventory = false;
    assert_eq!(
        serve(&mut channel, &replica, &request).await,
        Err(RoundFault::UnexpectedMessage)
    );
    assert!(channel.incoming.is_empty());
    assert!(channel.sent.is_empty());
    assert_eq!(Rc::strong_count(&replica.reservation), 1);
}

#[tokio::test]
async fn responder_rejects_an_export_from_another_scope_before_sending() {
    use mesh_replication::legacy_wire::SyncRequest;

    let scope = ConversationId::new([7; 32]);
    let foreign = ConversationId::new([8; 32]);
    let author = DeviceIdentity::generate();
    let event = Event::new(
        &author,
        foreign,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        vec![1],
    );
    let replica = Replica {
        exports: vec![event],
        ..Replica::default()
    };
    let mut channel = incoming([SyncWire::ReqHave(HaveChunk {
        conversation: scope,
        ids: vec![],
        more: false,
    })]);
    let request = legacy_wire::encode(&SyncWire::Request(SyncRequest {
        conversation: scope,
        have: vec![],
    }))
    .unwrap();
    assert_eq!(
        serve(&mut channel, &replica, &request).await,
        Err(RoundFault::UnexpectedMessage)
    );
    assert!(channel.sent.is_empty());
}
