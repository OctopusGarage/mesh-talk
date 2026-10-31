use super::*;
use crate::{
    channel::ChannelMeta,
    eventlog::{event::EventKind, persist::PersistentEventLog},
    identity::{account::Account, device::DeviceIdentity},
    node::{
        received_log::ReceivedEntry,
        received_store::ReceivedStore,
        reception_effects::{ArrivalKind, ReceptionEffects, ReceptionMetadata},
    },
    storage::profile::ProfileStorageLease,
};

struct Fixture {
    _directory: tempfile::TempDir,
    me: DeviceIdentity,
    sender: DeviceIdentity,
    account: Account,
    store: ChannelCheckpointStore,
    log: PersistentEventLog,
    received: ReceivedStore,
    channel: ConversationId,
    genesis: EventId,
    messages: Vec<Event>,
}
impl Fixture {
    fn new(count: usize) -> Self {
        Self::with_ciphertext(count, |_, bytes| bytes)
    }
    fn with_ciphertext(count: usize, transform: impl FnMut(usize, Vec<u8>) -> Vec<u8>) -> Self {
        Self::with_payload_size(count, 0, transform)
    }
    fn with_payload_size(
        count: usize,
        body_len: usize,
        mut transform: impl FnMut(usize, Vec<u8>) -> Vec<u8>,
    ) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let me = DeviceIdentity::generate();
        let sender = DeviceIdentity::generate();
        let account = Account::generate();
        let profile = ProfileStorage::open_participant(
            ProfileStorageLease::acquire(directory.path()).unwrap(),
            &me.public(),
            &account.public(),
            &directory.path().join("messages.log"),
            &directory.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        profile.complete_participant("pw", None).unwrap();
        let log = PersistentEventLog::open_profile(profile.clone(), "pw").unwrap();
        let received = ReceivedStore::from_profile(
            profile.clone(),
            &directory.path().join("received.log"),
            "pw",
        )
        .unwrap();
        ReceptionEffects::open(profile.clone()).unwrap();
        let store = ChannelCheckpointStore::open(profile).unwrap();
        let repository = log.repository().unwrap();
        repository.initialize_ancestry().unwrap();
        let channel = ConversationId::new([17; 32]);
        let meta = ChannelMeta {
            name: "checkpoint".into(),
            members: vec![sender.public(), me.public()],
            epoch: 0,
            owner: sender.public().user_id(),
        };
        let genesis = Event::new(
            &sender,
            channel,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            meta.encode(),
        );
        repository.append(&genesis).unwrap();
        let mut state = ChannelState::from_meta(channel, meta.clone());
        state.set_identity(sender.public().user_id());
        let keys = crate::node::channel::seal_keys_for(
            &sender,
            &meta.members,
            &state.my_sender_distribution(),
            0,
        )
        .unwrap();
        let rotation = Event::new(
            &sender,
            channel,
            2,
            vec![genesis.id],
            2,
            2,
            EventKind::KeyRotation,
            keys.encode(),
        );
        repository.append(&rotation).unwrap();
        let mut parent = rotation.id;
        let mut messages = Vec::new();
        for n in 0..count {
            let body = if body_len == 0 {
                format!("message {n}").into_bytes()
            } else {
                vec![42; body_len]
            };
            let event = Event::new(
                &sender,
                channel,
                n as u64 + 3,
                vec![parent],
                n as u64 + 3,
                n as u64 + 3,
                EventKind::Message,
                transform(n, state.seal_sender_message(&body).unwrap()),
            );
            repository.append(&event).unwrap();
            parent = event.id;
            messages.push(event);
        }
        Self {
            _directory: directory,
            me,
            sender,
            account,
            store,
            log,
            received,
            channel,
            genesis: genesis.id,
            messages,
        }
    }
    fn project(&self) {
        // Projection now admits one canonical header per turn. The receive-gap
        // fixture intentionally exceeds MAX_SKIP, so setup's progress guard
        // must scale with its signed rows rather than assuming batched replay.
        let turn_limit = 1024 + 4 * (self.messages.len() + 2);
        let mut turns = 0;
        while !self
            .store
            .project_step(self.log.repository().unwrap(), &self.me, self.channel)
            .unwrap()
        {
            turns += 1;
            assert!(turns < turn_limit, "channel fixture projection stalled");
        }
    }
    fn row(&self, index: usize, prepared: &PreparedChannelReceive) -> ReceivedEntry {
        ReceivedEntry {
            event_id: self.messages[index].id,
            conversation: self.channel,
            from: self.sender.public().user_id(),
            wall_clock: self.messages[index].wall_clock,
            plaintext: prepared.plaintext().to_vec(),
        }
    }
}

#[test]
fn channel_indexed_authority_tail_and_warm_turns_read_no_historical_bodies() {
    for count in [8, 256] {
        let fixture = Fixture::new(count);
        fixture.project();
        let repository = fixture.log.repository().unwrap();
        // Genesis and initial rotation are the only decoded bodies, regardless
        // of history length. Header admission does not fetch message ciphertext.
        assert_eq!(repository.body_read_counts(), (2, 2));
        let state = fixture.store.current_page(None).unwrap().remove(0);
        assert_eq!(state.name(), "checkpoint");
        assert!(state.is_member(&fixture.me.public().user_id()));
        let warm = ChannelCheckpointStore::open(fixture.store.profile.clone()).unwrap();
        for _ in 0..4 {
            assert!(
                warm.project_step(repository, &fixture.me, fixture.channel)
                    .unwrap()
            );
        }
        assert_eq!(repository.body_read_counts(), (2, 2));
        let last = fixture.messages.last().unwrap();
        let event = Event::new(
            &fixture.sender,
            fixture.channel,
            count as u64 + 3,
            vec![last.id],
            count as u64 + 3,
            count as u64 + 3,
            EventKind::Message,
            vec![1],
        );
        repository.append(&event).unwrap();
        fixture.project();
        assert_eq!(repository.body_read_counts(), (2, 2));
    }
}

#[test]
fn channel_checkpoint_transaction_rollback_preserves_key_body_and_receipt_eligibility() {
    let mut fixture = Fixture::new(3);
    fixture.project();
    let prepared = fixture
        .store
        .prepare_receive(&fixture.messages[0])
        .unwrap()
        .unwrap();
    let row = fixture.row(0, &prepared);
    let metadata = ReceptionMetadata {
        from_device: fixture.sender.public().user_id(),
        received_at: 88,
        arrival_kind: ArrivalKind::Live,
        own_device: false,
        batch_id: None,
        batch_complete: true,
        batch_size: 1,
        reception_only: false,
    };
    let failed: Result<(), LogError> = fixture.store.profile.database().transaction(|c| {
        assert!(ChannelCheckpointStore::install_receive_on(c, &prepared)?);
        assert!(ReceivedStore::install_channel_on(c, &row, Some(&metadata))?);
        Err(invalid("injected rollback after all channel writes"))
    });
    assert!(failed.is_err());
    assert!(fixture.received.entry(row.event_id).unwrap().is_none());
    let retry = fixture
        .store
        .prepare_receive(&fixture.messages[0])
        .unwrap()
        .unwrap();
    assert_eq!(retry.plaintext(), prepared.plaintext());
    assert!(
        fixture
            .store
            .commit_message(&retry, &mut fixture.received, &row, Some(&metadata))
            .unwrap()
    );
    assert!(
        !fixture
            .store
            .commit_message(&retry, &mut fixture.received, &row, Some(&metadata))
            .unwrap()
    );
    assert!(
        fixture
            .store
            .prepare_receive(&fixture.messages[0])
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .received
            .entry(row.event_id)
            .unwrap()
            .unwrap()
            .plaintext,
        row.plaintext
    );
}

#[test]
fn channel_late_control_generation_preserves_consumed_holes_and_rejects_old_candidate() {
    let mut fixture = Fixture::new(3);
    fixture.project();
    let first = fixture
        .store
        .prepare_receive(&fixture.messages[0])
        .unwrap()
        .unwrap();
    let row = fixture.row(0, &first);
    fixture
        .store
        .commit_message(&first, &mut fixture.received, &row, None)
        .unwrap();
    let stale = fixture
        .store
        .prepare_receive(&fixture.messages[2])
        .unwrap()
        .unwrap();
    let rename = Event::new(
        &fixture.sender,
        fixture.channel,
        1000,
        vec![fixture.genesis],
        2,
        2,
        EventKind::ChannelRename,
        b"renamed".to_vec(),
    );
    fixture.log.repository().unwrap().append(&rename).unwrap();
    let row = fixture.row(2, &stale);
    assert!(matches!(
        fixture
            .store
            .commit_message(&stale, &mut fixture.received, &row, None),
        Err(LogError::StaleCursor)
    ));
    fixture.project();
    assert_eq!(
        fixture.store.current_page(None).unwrap()[0].name(),
        "renamed"
    );
    assert!(
        fixture
            .store
            .prepare_receive(&fixture.messages[0])
            .unwrap()
            .is_none()
    );
    let third = fixture
        .store
        .prepare_receive(&fixture.messages[2])
        .unwrap()
        .unwrap();
    let row = fixture.row(2, &third);
    fixture
        .store
        .commit_message(&third, &mut fixture.received, &row, None)
        .unwrap();
    let skipped = fixture
        .store
        .prepare_receive(&fixture.messages[1])
        .unwrap()
        .unwrap();
    assert_eq!(skipped.plaintext(), b"message 1");
    let row = fixture.row(1, &skipped);
    fixture
        .store
        .commit_message(&skipped, &mut fixture.received, &row, None)
        .unwrap();
    assert!(
        fixture
            .store
            .prepare_receive(&fixture.messages[2])
            .unwrap()
            .is_none()
    );
}

#[test]
fn channel_pending_failure_is_dormant_until_receiver_key_material_changes() {
    let fixture = Fixture::new(130);
    fixture.project();
    // Use a correct signed event with authenticated sender header but invalid
    // AEAD. Repeated unchanged work must not reopen the body.
    let mut wire = fixture.messages[0].ciphertext.clone();
    *wire.last_mut().unwrap() ^= 1;
    let last = fixture.messages.last().unwrap();
    let bad = Event::new(
        &fixture.sender,
        fixture.channel,
        2000,
        vec![last.id],
        2000,
        2000,
        EventKind::Message,
        wire,
    );
    fixture.log.repository().unwrap().append(&bad).unwrap();
    fixture.project();
    assert!(fixture.store.prepare_receive(&bad).unwrap().is_none());
    assert!(
        fixture
            .store
            .pending_page(fixture.channel, EventKind::Message)
            .unwrap()
            .ids
            .len()
            <= 64
    );
    let dormant: (i64, i64) = fixture
        .store
        .profile
        .database()
        .read(|c| {
            c.query_row(
                "SELECT reason,attempt_revision FROM channel_pending WHERE channel=?1 AND event=?2",
                params![
                    fixture.channel.as_bytes().as_slice(),
                    bad.id.as_bytes().as_slice()
                ],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(dormant, (1, 0));
    let before = fixture.log.repository().unwrap().body_read_counts();
    for _ in 0..5 {
        let page = fixture
            .store
            .pending_page(fixture.channel, EventKind::Message)
            .unwrap();
        assert!(!page.ids.contains(&bad.id));
        assert!(page.ids.len() <= 64);
    }
    assert_eq!(fixture.log.repository().unwrap().body_read_counts(), before);
}

#[test]
fn channel_batch_terminal_is_durable_after_source_deletion_and_owner_reopen() {
    let mut fixture = Fixture::new(2);
    fixture.project();
    for index in 0..2 {
        let prepared = fixture
            .store
            .prepare_receive(&fixture.messages[index])
            .unwrap()
            .unwrap();
        let entry = fixture.row(index, &prepared);
        let metadata = ReceptionMetadata {
            from_device: fixture.sender.public().user_id(),
            received_at: 80 + index as u64,
            arrival_kind: ArrivalKind::CatchUp,
            own_device: false,
            batch_id: None,
            batch_complete: false,
            batch_size: 0,
            reception_only: false,
        };
        let published = fixture
            .store
            .commit_reception(&prepared, &mut fixture.received, &entry, metadata, true)
            .unwrap()
            .unwrap();
        assert_eq!(published.batch_size, index + 1);
        assert!(!published.batch_complete);
    }
    fixture
        .received
        .remove_where(|entry| entry.event_id == fixture.messages[1].id)
        .unwrap();
    let reopened = ChannelCheckpointStore::open(fixture.store.profile.clone()).unwrap();
    let (template, terminal) = reopened.finish_batch(fixture.channel).unwrap().unwrap();
    assert_eq!(template, fixture.messages[1].id);
    assert!(terminal.reception_only && terminal.batch_complete);
    assert_eq!(terminal.batch_size, 2);
    assert!(reopened.finish_batch(fixture.channel).unwrap().is_none());
    let count: i64 = fixture
        .store
        .profile
        .database()
        .read(|c| {
            c.query_row(
                "SELECT COUNT(*) FROM reception_identities WHERE kind=4",
                [],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn channel_discovery_cursor_includes_first_commit_and_resumes_without_body_reads() {
    let fixture = Fixture::new(256);
    assert!(
        fixture
            .store
            .discover_step(fixture.log.repository().unwrap())
            .unwrap()
    );
    let cursor: i64 = fixture
        .store
        .profile
        .database()
        .read(|c| {
            c.query_row("SELECT ordinal FROM channel_source_scan", [], |r| r.get(0))
                .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(cursor, 127);
    let dirty: bool = fixture
        .store
        .profile
        .database()
        .read(|c| {
            c.query_row(
                "SELECT EXISTS(SELECT 1 FROM channel_dirty WHERE channel=?1)",
                [fixture.channel.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert!(dirty);
    while fixture
        .store
        .discover_step(fixture.log.repository().unwrap())
        .unwrap()
    {}
    let warm = ChannelCheckpointStore::open(fixture.store.profile.clone()).unwrap();
    assert!(
        !warm
            .discover_step(fixture.log.repository().unwrap())
            .unwrap()
    );
    assert_eq!(fixture.log.repository().unwrap().body_read_counts(), (0, 0));
}

#[test]
fn channel_indexed_authority_keeps_retired_grant_facts_after_checkpoint_reload() {
    use crate::{
        discovery::Announce,
        node::channel_account_authority::{AccountAuthorityControl, AccountGrant},
    };
    let fixture = Fixture::new(3);
    fixture.project();
    let owner_account = Account::generate();
    let owner = Announce::new_with_account(&fixture.sender, &owner_account, "owner", 1);
    let member = Announce::new_with_account(&fixture.me, &fixture.account, "member", 1);
    let grants = vec![
        AccountGrant {
            incarnation: [1; 16],
            pending: false,
            original: owner.clone(),
            membership_event: fixture.genesis,
            acceptance_event: None,
        },
        AccountGrant {
            incarnation: [2; 16],
            pending: false,
            original: member.clone(),
            membership_event: fixture.genesis,
            acceptance_event: None,
        },
    ];
    let initial = AccountAuthorityControl {
        version: 1,
        previous: None,
        issuer: owner.clone(),
        owner_account: owner_account.public().ed25519_pub,
        grants: grants.clone(),
        revoked: Vec::new(),
        legacy_bindings: vec![owner.clone(), member.clone()],
        account_signature: Vec::new(),
    };
    let initial = Event::new(
        &fixture.sender,
        fixture.channel,
        2000,
        vec![fixture.messages[2].id],
        2000,
        2000,
        EventKind::ChannelAccountAuthority,
        initial.signed_encode(fixture.channel, &owner_account),
    );
    fixture.log.repository().unwrap().append(&initial).unwrap();
    fixture.project();
    let remove = AccountAuthorityControl {
        version: 1,
        previous: Some(initial.id),
        issuer: owner,
        owner_account: owner_account.public().ed25519_pub,
        grants: vec![grants[0].clone()],
        revoked: vec![grants[1].id().unwrap()],
        legacy_bindings: Vec::new(),
        account_signature: Vec::new(),
    };
    let remove = Event::new(
        &fixture.sender,
        fixture.channel,
        2001,
        vec![initial.id],
        2001,
        2001,
        EventKind::ChannelAccountAuthority,
        remove.signed_encode(fixture.channel, &owner_account),
    );
    fixture.log.repository().unwrap().append(&remove).unwrap();
    fixture.project();
    let before = fixture.log.repository().unwrap().body_read_counts();
    let reopened = ChannelCheckpointStore::open(fixture.store.profile.clone()).unwrap();
    let current = reopened
        .published_projection(fixture.channel)
        .unwrap()
        .unwrap();
    assert!(
        current
            .authority
            .account_grant(&fixture.account.public().ed25519_pub)
            .is_none()
    );
    assert!(
        current
            .authority
            .has_revocation(&fixture.account.public().ed25519_pub)
            .unwrap()
    );
    assert_eq!(
        current
            .authority
            .try_registered_account(&fixture.me.public(), &member)
            .unwrap(),
        Some(fixture.account.public().ed25519_pub)
    );
    assert_eq!(
        current
            .authority
            .revocation_page(&fixture.account.public().ed25519_pub, None)
            .unwrap()
            .events,
        vec![remove.id]
    );
    assert!(current.is_authorized(remove.id).unwrap());
    assert_eq!(fixture.log.repository().unwrap().body_read_counts(), before);
    let tail = Event::new(
        &fixture.sender,
        fixture.channel,
        2002,
        vec![remove.id],
        2002,
        2002,
        EventKind::Message,
        vec![1],
    );
    fixture.log.repository().unwrap().append(&tail).unwrap();
    assert!(matches!(
        current.is_authorized(remove.id),
        Err(LogError::StaleCursor)
    ));
}

#[test]
fn channel_ancestry_wide_frontier_is_exact_without_decoding_bodies() {
    let fixture = Fixture::new(1);
    let repository = fixture.log.repository().unwrap();
    let mut parents = Vec::new();
    for seq in 100..1125 {
        let event = Event::new(
            &fixture.sender,
            fixture.channel,
            seq,
            vec![],
            1,
            seq,
            EventKind::Message,
            vec![1],
        );
        repository.append(&event).unwrap();
        parents.push(event.id);
    }
    let target = Event::new(
        &fixture.sender,
        fixture.channel,
        2000,
        parents.clone(),
        0,
        0,
        EventKind::Message,
        vec![2],
    );
    repository.append(&target).unwrap();
    assert!(
        repository
            .descends_from(fixture.channel, target.id, parents[0])
            .unwrap()
    );
    assert!(
        repository
            .descends_from(fixture.channel, target.id, parents[1024])
            .unwrap()
    );
    assert!(
        !repository
            .descends_from(fixture.channel, target.id, fixture.genesis)
            .unwrap()
    );
    let selection = repository
        .select_ancestors(fixture.channel, target.id)
        .unwrap()
        .unwrap();
    let mut count = 0;
    let mut after = -1;
    loop {
        let page = repository
            .ancestor_headers_page(&selection, after, 64)
            .unwrap();
        assert!(page.items.len() <= 64);
        for (_, header) in page.items {
            assert!(parents.contains(&header.id));
            count += 1;
        }
        let Some(next) = page.next else {
            break;
        };
        after = next;
    }
    assert_eq!(count, 1025);
    assert_eq!(repository.body_read_counts(), (0, 0));
}

#[test]
fn channel_receive_preview_preserves_atomic_sequential_chain_progress() {
    let mut fixture = Fixture::new(3);
    fixture.project();
    let first = fixture
        .store
        .prepare_receive(&fixture.messages[0])
        .unwrap()
        .unwrap();
    let second = fixture
        .store
        .prepare_receive_following(&fixture.messages[1], &first)
        .unwrap()
        .unwrap();
    assert_eq!(first.plaintext(), b"message 0");
    assert_eq!(second.plaintext(), b"message 1");
    let first_row = fixture.row(0, &first);
    let second_row = fixture.row(1, &second);
    // Preview is private: the second CAS cannot publish before its predecessor.
    assert!(matches!(
        fixture
            .store
            .commit_message(&second, &mut fixture.received, &second_row, None),
        Err(LogError::StaleCursor)
    ));
    assert!(
        fixture
            .received
            .entry(fixture.messages[1].id)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .store
            .commit_message(&first, &mut fixture.received, &first_row, None)
            .unwrap()
    );
    assert!(
        fixture
            .store
            .commit_message(&second, &mut fixture.received, &second_row, None)
            .unwrap()
    );
    assert_eq!(
        fixture
            .received
            .entry(fixture.messages[0].id)
            .unwrap()
            .unwrap()
            .plaintext,
        b"message 0"
    );
    assert_eq!(
        fixture
            .received
            .entry(fixture.messages[1].id)
            .unwrap()
            .unwrap()
            .plaintext,
        b"message 1"
    );
    assert!(
        fixture
            .store
            .prepare_receive(&fixture.messages[2])
            .unwrap()
            .is_some()
    );
}

#[test]
fn channel_pending_retries_receive_a_turn_while_new_ciphertexts_remain() {
    let fixture = Fixture::new(130);
    fixture.project();
    // An installed rotation can wake an earlier failed ciphertext even while
    // new arrivals keep the unattempted selector nonempty.
    let generation = fixture
        .log
        .repository()
        .unwrap()
        .order_generation(
            &fixture.channel,
            crate::eventlog::repository::order::CausalOrder::Logical,
        )
        .unwrap()
        .unwrap();
    fixture
        .store
        .note_attempt(
            &fixture.messages[0],
            Some(0),
            -1,
            generation.job,
            generation.source_revision,
            None,
        )
        .unwrap();
    let first = fixture
        .store
        .pending_page(fixture.channel, EventKind::Message)
        .unwrap();
    assert_eq!(first.ids.len(), 64);
    assert!(!first.ids.contains(&fixture.messages[0].id));
    let second = fixture
        .store
        .pending_page(fixture.channel, EventKind::Message)
        .unwrap();
    assert!(
        second.ids.contains(&fixture.messages[0].id),
        "new arrivals must not starve a woken receiver chain"
    );
    assert_eq!(fixture.log.repository().unwrap().body_read_counts(), (2, 2));
}

#[test]
fn channel_message_preview_is_bounded_and_does_not_publish_plaintext() {
    let fixture = Fixture::new(130);
    fixture.project();
    let repository = fixture.log.repository().unwrap();
    let before = repository.body_read_counts();
    let page = fixture
        .store
        .prepare_message_turn(repository, fixture.channel)
        .unwrap();
    assert_eq!(page.candidates.len(), 2);
    assert!(page.more_work);
    assert_eq!(page.candidates[0].event_id(), fixture.messages[0].id);
    assert_eq!(page.candidates[1].event_id(), fixture.messages[1].id);
    assert_eq!(
        page.candidates[0].author(),
        fixture.sender.public().user_id()
    );
    assert_eq!(
        page.candidates[0].wall_clock(),
        fixture.messages[0].wall_clock
    );
    assert_eq!(repository.body_read_counts(), (before.0 + 2, before.1 + 2));
    assert!(
        fixture
            .received
            .entry(fixture.messages[0].id)
            .unwrap()
            .is_none()
    );
    assert!(
        fixture
            .store
            .prepare_receive(&fixture.messages[0])
            .unwrap()
            .is_some()
    );
}

#[test]
fn channel_warm_open_rejects_weakened_owned_selector_schema() {
    let fixture = Fixture::new(1);
    fixture.project();
    fixture.store.profile.database().transaction(|c| {
        c.execute_batch("DROP INDEX channel_pending_retry; CREATE INDEX channel_pending_retry ON channel_pending(channel,event);")?;
        Ok(())
    }).unwrap();
    assert!(matches!(
        ChannelCheckpointStore::open(fixture.store.profile.clone()),
        Err(LogError::CorruptFile(_))
    ));
}

#[test]
fn channel_projection_checkpoint_retires_already_projected_discovery() {
    let fixture = Fixture::new(130);
    fixture.project();
    let repository = fixture.log.repository().unwrap();
    while fixture.store.discover_step(repository).unwrap() {}
    assert_eq!(fixture.store.next_dirty().unwrap(), Some(fixture.channel));
    let before = repository.body_read_counts();
    assert!(
        fixture
            .store
            .project_step(repository, &fixture.me, fixture.channel)
            .unwrap()
    );
    assert_eq!(fixture.store.next_dirty().unwrap(), None);
    assert_eq!(repository.body_read_counts(), before);
}

#[test]
fn channel_projection_checkpoint_canonical_boundaries_use_exact_metadata() {
    let fixture = Fixture::new(1);
    fixture.project();
    assert!(
        fixture
            .store
            .canonical_state(fixture.channel, fixture.genesis, false)
            .unwrap()
            .is_none()
    );
    assert_eq!(
        fixture
            .store
            .canonical_state(fixture.channel, fixture.genesis, true)
            .unwrap()
            .unwrap()
            .name(),
        "checkpoint"
    );
    let rename = Event::new(
        &fixture.sender,
        fixture.channel,
        100,
        vec![fixture.genesis],
        2,
        2,
        EventKind::ChannelRename,
        b"renamed".to_vec(),
    );
    fixture.log.repository().unwrap().append(&rename).unwrap();
    assert!(matches!(
        fixture
            .store
            .canonical_state(fixture.channel, rename.id, true),
        Err(LogError::StaleCursor)
    ));
    fixture.project();
    let before = fixture.log.repository().unwrap().body_read_counts();
    assert_eq!(
        fixture
            .store
            .canonical_state(fixture.channel, rename.id, false)
            .unwrap()
            .unwrap()
            .name(),
        "checkpoint"
    );
    assert_eq!(
        fixture
            .store
            .canonical_state(fixture.channel, rename.id, true)
            .unwrap()
            .unwrap()
            .name(),
        "renamed"
    );
    // Canonical prefix differs from causal ancestry: the concurrent rename is
    // earlier than this message in Kahn order, although it is not its ancestor.
    assert_eq!(
        fixture
            .store
            .canonical_state(fixture.channel, fixture.messages[0].id, false)
            .unwrap()
            .unwrap()
            .name(),
        "renamed"
    );
    assert_eq!(fixture.log.repository().unwrap().body_read_counts(), before);
}

#[test]
fn channel_ancestor_selection_excludes_concurrent_prefix_and_pages_headers_only() {
    let fixture = Fixture::new(130);
    fixture.project();
    let repository = fixture.log.repository().unwrap();
    let rename = Event::new(
        &fixture.sender,
        fixture.channel,
        1000,
        vec![fixture.genesis],
        2,
        2,
        EventKind::ChannelRename,
        b"concurrent".to_vec(),
    );
    repository.append(&rename).unwrap();
    fixture.project();
    let before = repository.body_read_counts();
    let target = fixture.messages.last().unwrap();
    let selection = repository
        .select_ancestors(fixture.channel, target.id)
        .unwrap()
        .unwrap();
    let mut ids = Vec::new();
    let mut after = -1;
    loop {
        let page = repository
            .ancestor_headers_page(&selection, after, 16)
            .unwrap();
        assert!(page.items.len() <= 16);
        ids.extend(page.items.into_iter().map(|(_, header)| header.id));
        let Some(next) = page.next else {
            break;
        };
        assert!(next > after && next - after <= 16);
        after = next;
    }
    assert_eq!(ids.len(), 131);
    assert!(ids.contains(&fixture.genesis));
    assert!(!ids.contains(&rename.id));
    assert!(!ids.contains(&target.id));
    let tail = Event::new(
        &fixture.sender,
        fixture.channel,
        1001,
        vec![target.id],
        1001,
        1001,
        EventKind::Message,
        vec![1],
    );
    repository.append(&tail).unwrap();
    assert!(matches!(
        repository.ancestor_headers_page(&selection, -1, 16),
        Err(LogError::StaleCursor)
    ));
    assert_eq!(repository.body_read_counts(), before);
}

#[test]
fn channel_discarded_plaintext_consumes_exactly_without_body_or_receipt() {
    let mut fixture = Fixture::new(1);
    fixture.project();
    let candidate = fixture
        .store
        .prepare_receive(&fixture.messages[0])
        .unwrap()
        .unwrap();
    let entry = fixture.row(0, &candidate);
    let metadata = ReceptionMetadata {
        from_device: entry.from.clone(),
        received_at: 9,
        arrival_kind: ArrivalKind::Live,
        own_device: false,
        batch_id: None,
        batch_complete: true,
        batch_size: 1,
        reception_only: false,
    };
    assert!(
        fixture
            .store
            .commit_reception(&candidate, &mut fixture.received, &entry, metadata, false)
            .unwrap()
            .is_none()
    );
    assert!(fixture.received.processed(entry.event_id).unwrap());
    assert!(!fixture.received.contains(entry.event_id).unwrap());
    let reopened = ChannelCheckpointStore::open(fixture.store.profile.clone()).unwrap();
    assert!(
        reopened
            .prepare_receive(&fixture.messages[0])
            .unwrap()
            .is_none()
    );
    let effects: i64 = fixture
        .store
        .profile
        .database()
        .read(|c| {
            c.query_row("SELECT COUNT(*) FROM reception_identities", [], |r| {
                r.get(0)
            })
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(effects, 0);
}

#[test]
fn channel_causal_checkpoint_excludes_unobserved_owner_transfer_and_reuses_warm_state() {
    let fixture = Fixture::new(130);
    let repository = fixture.log.repository().unwrap();
    let transfer = Event::new(
        &fixture.sender,
        fixture.channel,
        1000,
        vec![fixture.genesis],
        2,
        2,
        EventKind::ChannelOwnerTransfer,
        fixture.me.user_id().into_bytes(),
    );
    repository.append(&transfer).unwrap();
    fixture.project();
    let target = fixture.messages.last().unwrap().id;
    assert_eq!(
        fixture
            .store
            .canonical_state(fixture.channel, target, false)
            .unwrap()
            .unwrap()
            .owner(),
        fixture.me.user_id()
    );
    let before = repository.body_read_counts();
    let causal = fixture
        .store
        .causal_state(repository, &fixture.me, fixture.channel, target)
        .unwrap()
        .unwrap();
    assert_eq!(causal.owner(), fixture.sender.user_id());
    assert_eq!(
        repository.body_read_counts(),
        (before.0 + 1, before.1 + 1),
        "only the selected genesis control is decoded"
    );
    let warm = ChannelCheckpointStore::open(fixture.store.profile.clone()).unwrap();
    let before = repository.body_read_counts();
    assert_eq!(
        warm.causal_state(repository, &fixture.me, fixture.channel, target)
            .unwrap()
            .unwrap()
            .owner(),
        fixture.sender.user_id()
    );
    assert_eq!(repository.body_read_counts(), before);
    let tail = Event::new(
        &fixture.sender,
        fixture.channel,
        1001,
        vec![target],
        1001,
        1001,
        EventKind::Message,
        vec![1],
    );
    repository.append(&tail).unwrap();
    fixture.project();
    let before = repository.body_read_counts();
    assert_eq!(
        fixture
            .store
            .causal_state(repository, &fixture.me, fixture.channel, target)
            .unwrap()
            .unwrap()
            .owner(),
        fixture.sender.user_id()
    );
    assert_eq!(
        repository.body_read_counts(),
        before,
        "safe-tail appends cannot change an old event's ancestors"
    );
}

#[test]
fn channel_batch_roles_complete_independently_with_body_free_file_terminal() {
    let mut fixture = Fixture::new(1);
    fixture.project();
    let candidate = fixture
        .store
        .prepare_receive(&fixture.messages[0])
        .unwrap()
        .unwrap();
    let entry = fixture.row(0, &candidate);
    let metadata = ReceptionMetadata {
        from_device: entry.from.clone(),
        received_at: 9,
        arrival_kind: ArrivalKind::CatchUp,
        own_device: false,
        batch_id: None,
        batch_complete: false,
        batch_size: 1,
        reception_only: false,
    };
    assert!(
        fixture
            .store
            .profile
            .database()
            .transaction(|c| ChannelCheckpointStore::prepare_batch_on(
                c,
                &candidate,
                ChannelReceptionRole::File,
                metadata.clone()
            ))
            .is_err()
    );
    fixture
        .store
        .commit_reception(
            &candidate,
            &mut fixture.received,
            &entry,
            metadata.clone(),
            true,
        )
        .unwrap();
    // Restore a separately committed file batch without requiring its template
    // body. Terminal identity is scoped by its independent file: batch key.
    let mut file_metadata = metadata;
    file_metadata.batch_id = Some(format!("file:{}", entry.event_id.to_hex()));
    file_metadata.batch_size = 3;
    fixture
        .store
        .profile
        .database()
        .transaction(|c| {
            c.execute(
                "INSERT INTO channel_reception_batches VALUES(?1,?2,1,?3,?4)",
                params![
                    fixture.store.account.as_slice(),
                    fixture.channel.as_bytes().as_slice(),
                    entry.event_id.as_bytes().as_slice(),
                    bincode::serialize(&file_metadata).unwrap()
                ],
            )?;
            Ok(())
        })
        .unwrap();
    let (_, message) = fixture
        .store
        .finish_batch(fixture.channel)
        .unwrap()
        .unwrap();
    assert_eq!(message.batch_size, 1);
    assert!(message.batch_id.unwrap().starts_with("message:"));
    assert!(
        fixture
            .store
            .finish_batch(fixture.channel)
            .unwrap()
            .is_none()
    );
    let (_, file) = fixture
        .store
        .finish_file_batch(fixture.channel)
        .unwrap()
        .unwrap();
    assert_eq!(file.batch_size, 3);
    assert_eq!(file.batch_id, file_metadata.batch_id);
    assert!(file.reception_only && file.batch_complete);
    assert!(
        fixture
            .store
            .finish_file_batch(fixture.channel)
            .unwrap()
            .is_none()
    );
}

#[test]
fn channel_stalled_authentication_does_not_reopen_on_each_valid_receive() {
    for count in [8, 128] {
        let mut fixture = Fixture::new(3);
        let mut parent = fixture.messages[2].id;
        let mut failures = Vec::new();
        for n in 0..count {
            let mut ciphertext = fixture.messages[0].ciphertext.clone();
            *ciphertext.last_mut().unwrap() ^= 1;
            let event = Event::new(
                &fixture.sender,
                fixture.channel,
                n + 6,
                vec![parent],
                n + 6,
                n + 6,
                EventKind::Message,
                ciphertext,
            );
            fixture.log.repository().unwrap().append(&event).unwrap();
            parent = event.id;
            failures.push(event);
        }
        fixture.project();
        for event in &failures {
            assert!(fixture.store.prepare_receive(event).unwrap().is_none());
        }
        let before = fixture.log.repository().unwrap().body_read_counts();
        let mut consumed = 0;
        loop {
            let turn = fixture
                .store
                .prepare_message_turn(fixture.log.repository().unwrap(), fixture.channel)
                .unwrap();
            for candidate in turn.candidates {
                let index = fixture
                    .messages
                    .iter()
                    .position(|event| event.id == candidate.event_id())
                    .unwrap();
                let row = fixture.row(index, &candidate);
                fixture
                    .store
                    .commit_message(&candidate, &mut fixture.received, &row, None)
                    .unwrap();
                consumed += 1;
            }
            if !turn.more_work {
                break;
            }
        }
        assert_eq!(consumed, 3);
        assert_eq!(
            fixture.log.repository().unwrap().body_read_counts(),
            (before.0 + 3, before.1 + 3),
            "ordinary chain progress reopened {count} immutable authentication failures"
        );
    }
}

#[test]
fn channel_gap_retry_waits_until_the_skip_bound_can_admit_it() {
    let mut fixture = Fixture::new(1003);
    fixture.project();
    let target = fixture.messages[1002].id;
    assert!(
        fixture
            .store
            .prepare_receive(&fixture.messages[1002])
            .unwrap()
            .is_none()
    );
    let first = fixture
        .store
        .prepare_receive(&fixture.messages[0])
        .unwrap()
        .unwrap();
    let row = fixture.row(0, &first);
    fixture
        .store
        .commit_message(&first, &mut fixture.received, &row, None)
        .unwrap();
    fixture
        .store
        .pending_page(fixture.channel, EventKind::Message)
        .unwrap();
    let retry = fixture
        .store
        .pending_page(fixture.channel, EventKind::Message)
        .unwrap();
    assert!(
        !retry.ids.contains(&target),
        "one step still leaves a gap larger than MAX_SKIP"
    );
    let second = fixture
        .store
        .prepare_receive(&fixture.messages[1])
        .unwrap()
        .unwrap();
    let row = fixture.row(1, &second);
    fixture
        .store
        .commit_message(&second, &mut fixture.received, &row, None)
        .unwrap();
    fixture
        .store
        .pending_page(fixture.channel, EventKind::Message)
        .unwrap();
    let retry = fixture
        .store
        .pending_page(fixture.channel, EventKind::Message)
        .unwrap();
    assert!(
        retry.ids.contains(&target),
        "eligible gaps must receive a retry turn despite fresh work"
    );
    let last = fixture
        .store
        .prepare_receive(&fixture.messages[1002])
        .unwrap()
        .unwrap();
    let row = fixture.row(1002, &last);
    fixture
        .store
        .commit_message(&last, &mut fixture.received, &row, None)
        .unwrap();
    assert_eq!(
        fixture.received.entry(target).unwrap().unwrap().plaintext,
        b"message 1002"
    );
}

#[test]
fn channel_malformed_header_commits_exact_discovery_evidence() {
    let mut fixture = Fixture::new(0);
    let event = Event::new(
        &fixture.sender,
        fixture.channel,
        3,
        vec![fixture.log.heads(&fixture.channel).unwrap()[0]],
        3,
        3,
        EventKind::Message,
        vec![1],
    );
    fixture.log.append(event.clone()).unwrap();
    fixture.project();
    fixture.received.check_publication().unwrap();
    fixture.store.profile.database().transaction(|c| {
        c.execute_batch("CREATE TEMP TRIGGER reject_discard_evidence BEFORE INSERT ON transcript_received_evidence BEGIN SELECT RAISE(ABORT,'injected discard evidence failure'); END;")?;
        Ok(())
    }).unwrap();
    assert!(fixture.store.prepare_receive(&event).is_err());
    assert!(!fixture.received.processed(event.id).unwrap());
    fixture
        .store
        .profile
        .database()
        .transaction(|c| {
            assert!(!ChannelCheckpointStore::processed_on(
                c,
                fixture.channel,
                event.id
            )?);
            c.execute_batch("DROP TRIGGER reject_discard_evidence;")?;
            Ok(())
        })
        .unwrap();
    assert!(fixture.store.prepare_receive(&event).unwrap().is_none());
    assert!(
        fixture.received.processed(event.id).unwrap(),
        "definitive channel discard must also retire generic discovery"
    );
    assert!(fixture.received.entry(event.id).unwrap().is_none());
}

#[test]
fn channel_message_preview_bounds_failed_backlogs_and_retains_single_candidate() {
    for failures in [128, 512] {
        let mut fixture = Fixture::with_ciphertext(failures + 2, |n, mut bytes| {
            if n > 0 && n <= failures {
                *bytes.last_mut().unwrap() ^= 1;
            }
            bytes
        });
        fixture.project();
        let repository = fixture.log.repository().unwrap();
        let before = repository.body_read_counts();
        let turn = fixture
            .store
            .prepare_message_turn(repository, fixture.channel)
            .unwrap();
        let after = repository.body_read_counts();
        assert!(
            after.0 - before.0 <= 64,
            "one preview decoded {} bodies from {failures} failed rows",
            after.0 - before.0
        );
        assert!(
            turn.candidates.is_empty(),
            "a lone preview cannot be classified before the remaining work is examined"
        );
        assert!(turn.more_work);
        assert!(!fixture.received.contains(fixture.messages[0].id).unwrap());
        let mut turns = 1;
        let candidates = loop {
            let before = repository.body_read_counts();
            let turn = fixture
                .store
                .prepare_message_turn(repository, fixture.channel)
                .unwrap();
            assert!(repository.body_read_counts().0 - before.0 <= 64);
            turns += 1;
            assert!(turns < 32);
            if !turn.candidates.is_empty() {
                break turn.candidates;
            }
            assert!(turn.more_work);
        };
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].event_id(), fixture.messages[0].id);
        assert_eq!(candidates[1].event_id(), fixture.messages[failures + 1].id);
        for candidate in candidates {
            let index = if candidate.event_id() == fixture.messages[0].id {
                0
            } else {
                failures + 1
            };
            let row = fixture.row(index, &candidate);
            let metadata = ReceptionMetadata {
                from_device: row.from.clone(),
                received_at: 42,
                arrival_kind: ArrivalKind::CatchUp,
                own_device: false,
                batch_id: None,
                batch_complete: true,
                batch_size: 1,
                reception_only: false,
            };
            let committed = fixture
                .store
                .commit_reception(&candidate, &mut fixture.received, &row, metadata, true)
                .unwrap()
                .unwrap();
            assert_eq!(committed.arrival_kind, ArrivalKind::CatchUp);
        }
        let terminal = fixture
            .store
            .finish_batch(fixture.channel)
            .unwrap()
            .unwrap()
            .1;
        assert_eq!(terminal.batch_size, 2);
        assert!(
            fixture
                .store
                .finish_batch(fixture.channel)
                .unwrap()
                .is_none()
        );
    }
}

#[test]
fn channel_message_preview_admits_two_maximum_charge_rows() {
    let base = Fixture::new(1);
    let base_bytes = base
        .log
        .repository()
        .unwrap()
        .header(&base.messages[0].id)
        .unwrap()
        .unwrap()
        .stored_bytes;
    let limit = super::super::CHANNEL_PENDING_BYTES;
    let body_len = (limit - 1024) / 4 - base_bytes + b"message 0".len();
    let fixture = Fixture::with_payload_size(2, body_len, |_, bytes| bytes);
    fixture.project();
    for event in &fixture.messages {
        let header = fixture
            .log
            .repository()
            .unwrap()
            .header(&event.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            header.stored_bytes * 4 + 1024,
            limit,
            "fixture must exercise the exact existing per-row admission boundary"
        );
    }
    let turn = fixture
        .store
        .prepare_message_turn(fixture.log.repository().unwrap(), fixture.channel)
        .unwrap();
    assert_eq!(
        turn.candidates.len(),
        2,
        "two legal maximum-charge rows cannot indefinitely defer the first candidate"
    );
    assert!(
        turn.candidates
            .iter()
            .all(|candidate| candidate.plaintext().len() == body_len)
    );
}

#[test]
fn channel_legacy_causal_cut_resumes_without_hydrating_message_history() {
    let fixture = Fixture::new(300);
    fixture.project();
    let repository = fixture.log.repository().unwrap();
    let before = repository.body_read_counts();
    let selection = repository
        .start_ancestor_selection(fixture.channel, fixture.messages.last().unwrap().id)
        .unwrap()
        .unwrap();
    assert!(
        !repository.ancestor_selection_step(&selection).unwrap(),
        "one bounded turn cannot drain a 300-event ancestor graph"
    );
    let id = selection.id();
    drop(selection);
    let selection = repository.resume_ancestor_selection(id).unwrap().unwrap();
    let mut turns = 1;
    while !repository.ancestor_selection_step(&selection).unwrap() {
        turns += 1;
        assert!(turns < 10);
    }
    assert!(turns >= 2);
    let mut after = -1;
    let mut count = 0;
    loop {
        let page = repository
            .ancestor_headers_page(&selection, after, 64)
            .unwrap();
        count += page.items.len();
        let Some(next) = page.next else {
            break;
        };
        after = next;
    }
    assert_eq!(count, 301);
    assert_eq!(repository.body_read_counts(), before);
    repository.retire_ancestor_selection(id).unwrap();
    assert!(repository.resume_ancestor_selection(id).unwrap().is_none());
}

#[test]
fn channel_indexed_legacy_parent_cut_rebuild_preserves_account_lifetime_and_lease() {
    use crate::{
        discovery::Announce,
        node::channel_account_authority::{AccountAuthorityControl, AccountGrant},
    };
    let fixture = Fixture::new(0);
    let repository = fixture.log.repository().unwrap();
    let owner_account = Account::generate();
    let grants: Vec<_> = [
        (&fixture.sender, &owner_account, 1),
        (&fixture.me, &fixture.account, 2),
    ]
    .into_iter()
    .map(|(device, account, n)| AccountGrant {
        incarnation: [n; 16],
        pending: false,
        original: Announce::new_with_account(device, account, "legacy", 1),
        membership_event: fixture.genesis,
        acceptance_event: None,
    })
    .collect();
    let control = AccountAuthorityControl {
        version: 1,
        previous: None,
        issuer: Announce::new_with_account(&fixture.sender, &owner_account, "owner", 1),
        owner_account: owner_account.public().ed25519_pub,
        legacy_bindings: grants.iter().map(|grant| grant.original.clone()).collect(),
        grants,
        revoked: vec![],
        account_signature: vec![],
    };
    let authority = Event::new(
        &fixture.sender,
        fixture.channel,
        3,
        vec![fixture.genesis],
        3,
        3,
        EventKind::ChannelAccountAuthority,
        control.signed_encode(fixture.channel, &owner_account),
    );
    repository.append(&authority).unwrap();
    fixture.project();
    assert!(
        fixture
            .store
            .current_state(fixture.channel)
            .unwrap()
            .unwrap()
            .is_member(&fixture.me.user_id())
    );
    let departure = Event::new(
        &fixture.me,
        fixture.channel,
        1,
        vec![fixture.genesis],
        4,
        4,
        EventKind::ChannelLeave,
        vec![],
    );
    repository.append(&departure).unwrap();
    fixture.project();
    let projection = fixture
        .store
        .published_projection(fixture.channel)
        .unwrap()
        .unwrap();
    assert_eq!(projection.ownership, Some(authority.id));
    assert!(
        !projection
            .state
            .as_ref()
            .unwrap()
            .is_member(&fixture.me.user_id())
    );
    assert!(projection.pending_departure(&fixture.me.user_id()).unwrap());
    assert!(projection.is_authorized(departure.id).unwrap());
    let basis = fixture
        .store
        .causal_projection_basis(repository, fixture.channel, &[authority.id, departure.id])
        .unwrap()
        .unwrap();
    assert!(basis.contains_event(departure.id).unwrap());
    assert!(
        !basis
            .state
            .as_ref()
            .unwrap()
            .is_member(&fixture.me.user_id())
    );
    let held = basis.authority.clone();
    drop(basis);
    let owner_grant = control
        .grants
        .iter()
        .find(|grant| grant.id().unwrap().account_pub == owner_account.public().ed25519_pub)
        .unwrap()
        .id()
        .unwrap();
    assert_eq!(
        held.try_grant_origin(owner_grant).unwrap(),
        Some(authority.id)
    );
    assert!(
        fixture
            .store
            .profile
            .database()
            .read(|c| c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM channel_projection_queries WHERE retired=0)",
                    [],
                    |r| r.get::<_, bool>(0)
                )
                .map_err(Into::into))
            .unwrap(),
        "moved authority retains its causal fact lease"
    );
    drop(held);
    assert!(
        !fixture
            .store
            .profile
            .database()
            .read(|c| c
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM channel_projection_queries WHERE retired=0)",
                    [],
                    |r| r.get::<_, bool>(0)
                )
                .map_err(Into::into))
            .unwrap(),
        "the last authority owner retires the causal query"
    );
    let before = repository.body_read_counts();
    let warm = ChannelCheckpointStore::open(fixture.store.profile.clone()).unwrap();
    assert!(
        warm.project_step(repository, &fixture.me, fixture.channel)
            .unwrap()
    );
    assert!(
        !warm
            .current_state(fixture.channel)
            .unwrap()
            .unwrap()
            .is_member(&fixture.me.user_id())
    );
    assert_eq!(
        repository.body_read_counts(),
        before,
        "unchanged warm authority does not decode historical controls"
    );
}

#[test]
fn channel_receiver_and_source_handles_stay_stale_after_account_returns() {
    use crate::storage::profile::participant::{finish_adoption, prepare_adoption};
    let mut fixture = Fixture::new(1);
    fixture.project();
    let source = fixture
        .store
        .source_binding(fixture.channel)
        .unwrap()
        .unwrap();
    let prepared = fixture
        .store
        .prepare_receive(&fixture.messages[0])
        .unwrap()
        .unwrap();
    let next = Account::generate();
    let database = fixture.store.profile.database();
    prepare_adoption(database, &fixture.account.public(), &next.public()).unwrap();
    finish_adoption(database, &next.public()).unwrap();
    prepare_adoption(database, &next.public(), &fixture.account.public()).unwrap();
    finish_adoption(database, &fixture.account.public()).unwrap();

    assert!(
        matches!(
            fixture.store.check_source(&source),
            Err(LogError::StaleCursor)
        ),
        "returning to the same account must not revive a captured source handle"
    );
    assert!(matches!(
        fixture.store.prepare_receive(&fixture.messages[0]),
        Err(LogError::StaleCursor)
    ));
    assert!(
        matches!(
            database.transaction(|c| ChannelCheckpointStore::install_receive_on(c, &prepared)),
            Err(LogError::StaleCursor)
        ),
        "the static consuming transaction must reject an earlier account lifetime"
    );
    assert!(
        fixture
            .received
            .entry(fixture.messages[0].id)
            .unwrap()
            .is_none()
    );
    let current = ChannelCheckpointStore::open(fixture.store.profile.clone()).unwrap();
    assert!(
        matches!(current.check_source(&source), Err(LogError::StaleCursor)),
        "a new owner must also reject a source captured before adoption"
    );
    let prepared = current
        .prepare_receive(&fixture.messages[0])
        .unwrap()
        .unwrap();
    let entry = ReceivedEntry {
        conversation: fixture.channel,
        event_id: prepared.event_id(),
        wall_clock: prepared.wall_clock(),
        from: prepared.author().to_owned(),
        plaintext: prepared.plaintext().to_vec(),
    };
    assert!(
        current
            .commit_message(&prepared, &mut fixture.received, &entry, None)
            .unwrap()
    );
    assert_eq!(
        fixture
            .received
            .entry(entry.event_id)
            .unwrap()
            .unwrap()
            .plaintext,
        b"message 0"
    );
}

#[test]
fn channel_projection_control_does_not_drain_historical_ancestry_in_one_turn() {
    let fixture = Fixture::new(600);
    let repository = fixture.log.repository().unwrap();
    let leave = Event::new(
        &fixture.me,
        fixture.channel,
        1,
        vec![fixture.genesis],
        2,
        2,
        EventKind::ChannelLeave,
        vec![],
    );
    repository.append(&leave).unwrap();
    fixture.project();
    let change = Event::new(
        &fixture.sender,
        fixture.channel,
        603,
        vec![fixture.messages.last().unwrap().id],
        603,
        603,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "concurrent leave".into(),
            members: vec![fixture.sender.public(), fixture.me.public()],
            epoch: 1,
            owner: fixture.sender.user_id(),
        }
        .encode(),
    );
    repository.append(&change).unwrap();
    let mut complete = false;
    for _ in 0..256 {
        let before = repository.ancestry_vertex_reads();
        complete = fixture
            .store
            .project_step(repository, &fixture.me, fixture.channel)
            .unwrap();
        let examined = repository.ancestry_vertex_reads() - before;
        assert!(
            examined <= 64,
            "one actual channel projection turn traversed {examined} historical vertices"
        );
        if complete {
            break;
        }
    }
    assert!(
        complete,
        "bounded authority work must eventually publish its exact decision"
    );
    let state = fixture
        .store
        .current_state(fixture.channel)
        .unwrap()
        .unwrap();
    assert!(
        !state.is_member(&fixture.me.user_id()),
        "the unobserved signed leave must still block resurrection"
    );
}

#[test]
fn channel_causal_query_yields_exact_work_and_preserves_strict_ancestry() {
    use super::causal::{CausalProjectionQuery, CausalProjectionStep};
    use crate::eventlog::sync::network::PreparationBudget;
    fn send<T: Send>() {}
    send::<CausalProjectionQuery>();
    send::<crate::node::channel::ChannelMembershipProjection>();
    let fixture = Fixture::new(128);
    let repository = fixture.log.repository().unwrap();
    let target = fixture.messages.last().unwrap().id;
    assert!(
        matches!(
            fixture
                .store
                .start_causal_projection(fixture.channel, &[target], false),
            Err(LogError::StaleCursor)
        ),
        "unfinished canonical legacy discovery cannot authorize a causal query"
    );
    fixture.project();
    let mut query = fixture
        .store
        .start_causal_projection(fixture.channel, &[target], false)
        .unwrap();
    let budget = PreparationBudget {
        rows: 8,
        edges: 16,
        bytes: 32 * 1024,
    };
    let before = repository.body_read_counts();
    let mut pending = 0;
    let mut ready = None;
    for _ in 0..4096 {
        let step = fixture
            .store
            .causal_projection_step(repository, &mut query, budget)
            .unwrap();
        let work = match step {
            CausalProjectionStep::Pending(work) => {
                pending += 1;
                work
            }
            CausalProjectionStep::Ready { projection, work } => {
                ready = projection;
                work
            }
        };
        assert!(
            work.rows <= budget.rows && work.edges <= budget.edges && work.bytes <= budget.bytes,
            "{work:?}"
        );
        if ready.is_some() {
            break;
        }
    }
    assert!(pending > 16, "the signed ancestor prefix must yield");
    let ready = ready.expect("finite strict ancestor preparation completes");
    assert!(
        ready
            .state
            .as_ref()
            .unwrap()
            .members()
            .contains(&fixture.sender.public())
    );
    assert!(
        ready
            .state
            .as_ref()
            .unwrap()
            .members()
            .contains(&fixture.me.public())
    );
    fixture.store.check_causal_projection(&query).unwrap();
    assert!(
        !ready
            .historical
            .as_ref()
            .unwrap()
            .contains_event(target)
            .unwrap(),
        "the queried event itself is excluded"
    );
    let reads = repository.body_read_counts();
    assert_eq!(
        reads.0 - before.0,
        1,
        "only the genesis membership control is decoded, not retained message bodies"
    );
}

#[test]
fn channel_owner_query_preserves_later_unobserved_ownership_loss() {
    use crate::eventlog::sync::network::PreparationBudget;
    use crate::node::join_history::indexed::{
        IndexedView, OwnerAuthorizationQuery, OwnerAuthorizationStep,
    };
    fn send<T: Send>() {}
    send::<OwnerAuthorizationQuery>();
    for observed in [false, true] {
        let fixture = Fixture::new(0);
        let repository = fixture.log.repository().unwrap();
        let publication = Event::new(
            &fixture.sender,
            fixture.channel,
            3,
            vec![fixture.genesis],
            3,
            3,
            EventKind::ChannelRename,
            b"publication".to_vec(),
        );
        repository.append(&publication).unwrap();
        let transfer = Event::new(
            &fixture.sender,
            fixture.channel,
            4,
            vec![if observed {
                publication.id
            } else {
                fixture.genesis
            }],
            4,
            4,
            EventKind::ChannelOwnerTransfer,
            fixture.me.user_id().into_bytes(),
        );
        repository.append(&transfer).unwrap();
        fixture.project();
        let source = fixture
            .store
            .source_binding(fixture.channel)
            .unwrap()
            .unwrap();
        let view = IndexedView::new(&fixture.store, repository, &fixture.me, source);
        let mut query = view
            .start_owner_authorization(&repository.header(&publication.id).unwrap().unwrap())
            .unwrap();
        let budget = PreparationBudget {
            rows: 8,
            edges: 16,
            bytes: 32 * 1024,
        };
        let mut result = None;
        for _ in 0..512 {
            let work = match view.owner_authorized_step(&mut query, budget).unwrap() {
                OwnerAuthorizationStep::Pending(work) => work,
                OwnerAuthorizationStep::Ready {
                    authorized,
                    before,
                    work,
                } => {
                    assert_eq!(before.is_some(), authorized);
                    if let Some(before) = before {
                        assert_eq!(before.owner(), fixture.sender.user_id());
                    }
                    result = Some(authorized);
                    work
                }
            };
            assert!(
                work.rows <= budget.rows
                    && work.edges <= budget.edges
                    && work.bytes <= budget.bytes,
                "{work:?}"
            );
            if result.is_some() {
                break;
            }
        }
        assert_eq!(
            result,
            Some(observed),
            "only an observed publication survives later owner loss"
        );
    }
}

#[test]
fn channel_decision_replacement_rejects_old_predicate_publication() {
    use crate::eventlog::sync::network::PreparationBudget;
    use crate::node::channel::graph::{AuthorityGraph, preparing::PreparingGraph};
    let fixture = Fixture::new(2);
    fixture.project();
    let repository = fixture.log.repository().unwrap();
    let source = fixture
        .store
        .source_binding(fixture.channel)
        .unwrap()
        .unwrap();
    let first = PreparingGraph::open(
        fixture.store.profile.clone(),
        repository,
        crate::node::channel::graph::preparing::DecisionBinding {
            job: source.job,
            source_job: source.job,
            revision: source.revision,
            channel: fixture.channel,
            event: fixture.messages[0].id,
        },
        PreparationBudget::default(),
    )
    .unwrap();
    while !first.prepare().unwrap() {}
    first
        .save_predicate(b"predicate", None, Some(true))
        .unwrap();
    let next = PreparingGraph::open(
        fixture.store.profile.clone(),
        repository,
        crate::node::channel::graph::preparing::DecisionBinding {
            job: source.job,
            source_job: source.job,
            revision: source.revision,
            channel: fixture.channel,
            event: fixture.messages[1].id,
        },
        PreparationBudget::default(),
    )
    .unwrap();
    while !next.prepare().unwrap() {}
    assert!(
        matches!(
            first.save_predicate(b"predicate", None, Some(true)),
            Err(LogError::StaleCursor)
        ),
        "an old private reducer must not contaminate the next event's answers"
    );
    assert!(matches!(
        first.predicate(b"predicate"),
        Err(LogError::StaleCursor)
    ));
    assert!(matches!(
        next.predicate(b"predicate").unwrap(),
        crate::node::channel::graph::PredicateProgress::Scan(None)
    ));
    next.save_predicate(b"predicate", None, Some(false))
        .unwrap();
    fixture
        .store
        .profile
        .database()
        .transaction(|c| next.complete_on(c))
        .unwrap();
    assert!(
        matches!(
            next.save_predicate(b"late", None, Some(true)),
            Err(LogError::StaleCursor)
        ),
        "a completed decision is immutable"
    );
}

#[test]
fn channel_canonical_readiness_does_not_publish_history_before_recall_attempt() {
    let fixture = Fixture::new(1);
    assert!(!fixture.store.history_read_ready(fixture.channel).unwrap());
    assert!(matches!(
        fixture
            .store
            .content_visible(fixture.channel, fixture.messages[0].id),
        Err(LogError::StaleCursor)
    ));
    fixture.project();
    assert!(fixture.store.history_read_ready(fixture.channel).unwrap());
    let target = fixture.messages[0].id;
    let recall = Event::new(
        &fixture.sender,
        fixture.channel,
        4,
        vec![target],
        4,
        4,
        EventKind::Delete,
        vec![0],
    );
    fixture.log.repository().unwrap().append(&recall).unwrap();
    assert!(!fixture.store.history_read_ready(fixture.channel).unwrap());
    fixture.project();
    assert!(
        fixture
            .store
            .source_binding(fixture.channel)
            .unwrap()
            .is_some()
    );
    assert!(
        matches!(
            fixture.store.content_visible(fixture.channel, target),
            Err(LogError::StaleCursor)
        ),
        "canonical readiness exposed history while an admitted recall had not been attempted"
    );
    let before = fixture.log.repository().unwrap().body_read_counts();
    let source = fixture
        .store
        .source_binding(fixture.channel)
        .unwrap()
        .unwrap();
    // Unknown target identities keep their exact retry, but do not indefinitely
    // block unrelated readable history after the first authorization attempt.
    fixture
        .store
        .defer_recall(&source, recall.id, Some(target))
        .unwrap();
    assert!(fixture.store.history_read_ready(fixture.channel).unwrap());
    assert!(
        fixture
            .store
            .content_visible(fixture.channel, target)
            .unwrap()
    );
    fixture
        .store
        .profile
        .database()
        .transaction(|c| recall::wake_on(c, fixture.channel, target))
        .unwrap();
    assert!(!fixture.store.history_read_ready(fixture.channel).unwrap());
    assert_eq!(
        fixture.store.recall_page(&source).unwrap().ids,
        vec![recall.id]
    );
    assert!(!fixture.store.history_read_ready(fixture.channel).unwrap());
    fixture
        .store
        .defer_recall(&source, recall.id, None)
        .unwrap();
    assert!(fixture.store.history_read_ready(fixture.channel).unwrap());
    assert_eq!(fixture.log.repository().unwrap().body_read_counts(), before);
}

#[test]
fn channel_logical_retirement_reclaims_orphan_facts_in_bounded_turns() {
    let fixture = Fixture::new(257);
    fixture.project();
    let counts = || {
        fixture
            .store
            .profile
            .database()
            .read(|c| {
                let facts: i64 = c.query_row(
                    "SELECT count(*) FROM channel_event_authority WHERE channel=?1",
                    [fixture.channel.as_bytes().as_slice()],
                    |r| r.get(0),
                )?;
                let current: i64 = c.query_row(
                    "SELECT count(*) FROM channel_current WHERE channel=?1",
                    [fixture.channel.as_bytes().as_slice()],
                    |r| r.get(0),
                )?;
                let retained: i64 = c.query_row(
                    "SELECT count(*) FROM retained_events WHERE domain=?1 AND conversation=?2",
                    params![
                        b"MeshPartEventsV1".as_slice(),
                        fixture.channel.as_bytes().as_slice()
                    ],
                    |r| r.get(0),
                )?;
                let receivers: i64 = c.query_row(
                    "SELECT count(*) FROM channel_receivers WHERE channel=?1",
                    [fixture.channel.as_bytes().as_slice()],
                    |r| r.get(0),
                )?;
                Ok((facts, current, retained, receivers))
            })
            .unwrap()
    };
    let initial = counts();
    assert!(initial.0 >= 257);
    fixture
        .log
        .repository()
        .unwrap()
        .begin_retirement(&fixture.channel)
        .unwrap();
    let reads = fixture.log.repository().unwrap().body_read_counts();
    let mut turns = 0;
    loop {
        let work = fixture.store.retire_orphan_step(7).unwrap();
        assert!(work <= 7, "retirement exceeded caller row budget: {work}");
        if work == 0 {
            break;
        }
        turns += 1;
        assert!(
            turns < 2000,
            "retirement did not finish without another channel read"
        );
    }
    let final_counts = counts();
    assert_eq!((final_counts.0, final_counts.1), (0, 0));
    assert_eq!(
        final_counts.2, initial.2,
        "derived cleanup must not replace event retirement"
    );
    assert_eq!(
        final_counts.3, initial.3,
        "consumed receiver state is not disposable projection data"
    );
    assert_eq!(fixture.log.repository().unwrap().body_read_counts(), reads);
    assert!(turns > 257 / 7);
}
