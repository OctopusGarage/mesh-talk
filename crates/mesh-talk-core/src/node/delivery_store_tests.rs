use super::*;

#[test]
fn conflicting_queued_or_completed_receipt_rejects_before_durable_intent() {
    for completed in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
        let tx = incoming(dir.path());
        let mut conflict: DeliveryTransaction =
            bincode::deserialize(&bincode::serialize(&tx).unwrap()).unwrap();
        if let DeliveryTransaction::Incoming {
            receipt: Some(receipt),
            ..
        } = &mut conflict
        {
            let bob = DeviceIdentity::from_secret_bytes([3; 32], [4; 32]);
            let old = &receipt.destination.event;
            receipt.destination.event = Event::new(
                &bob,
                old.conversation_id,
                old.seq + 1,
                vec![old.id],
                old.lamport + 1,
                102,
                old.kind,
                b"different encrypted receipt".to_vec(),
            );
        }
        store.begin(tx).unwrap();
        let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
        let mut ratchet =
            DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
        let mut sent = SentLog::open(&dir.path().join("sent"), "pw").unwrap();
        let mut received = ReceivedLog::open(&dir.path().join("received"), "pw").unwrap();
        store
            .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
            .unwrap();
        if completed {
            let receipt = store.retry_receipts(1)[0].clone();
            store
                .finish_receipt(receipt.conversation, receipt.original_event_id)
                .unwrap();
        }
        let state_before = std::fs::read(dir.path().join("sessions")).unwrap();
        let received_before = std::fs::read(dir.path().join("received")).unwrap();
        let journal_before = std::fs::read(dir.path().join("delivery-transactions.log")).unwrap();
        assert!(
            store.begin(conflict).is_err(),
            "conflicting receipt must reject before WAL acceptance"
        );
        assert!(store.pending_transactions().is_empty());
        assert_eq!(
            std::fs::read(dir.path().join("sessions")).unwrap(),
            state_before
        );
        assert_eq!(
            std::fs::read(dir.path().join("received")).unwrap(),
            received_before
        );
        assert_eq!(
            std::fs::read(dir.path().join("delivery-transactions.log")).unwrap(),
            journal_before
        );
        drop(store);
        assert!(DeliveryStore::open(dir.path(), "pw")
            .unwrap()
            .pending_transactions()
            .is_empty());
    }
}

#[test]
fn malformed_incoming_history_routing_and_receipt_ids_reject_before_install() {
    for case in 0..8 {
        let dir = tempfile::tempdir().unwrap();
        let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
        let mut tx = incoming_body(dir.path(), case < 5);
        if let DeliveryTransaction::Incoming {
            received,
            receipt: Some(receipt),
            ..
        } = &mut tx
        {
            match case {
                0 | 5 => received.from = "f".repeat(32),
                1 | 6 => {
                    received.conversation = ConversationId::new([99; 32]);
                    receipt.conversation = received.conversation;
                }
                2 | 7 => receipt.logical_id = EventId::new([99; 32]),
                3 => receipt.conversation = ConversationId::new([99; 32]),
                4 => receipt.wall_clock += 1,
                _ => unreachable!(),
            }
        }
        let before = std::fs::read(dir.path().join("sessions")).unwrap();
        assert!(
            store.begin(tx).is_err(),
            "malformed history/receipt binding case {case} must reject before WAL acceptance"
        );
        assert!(store.pending_transactions().is_empty());
        assert_eq!(std::fs::read(dir.path().join("sessions")).unwrap(), before);
        drop(store);
        assert!(DeliveryStore::open(dir.path(), "pw")
            .unwrap()
            .pending_transactions()
            .is_empty());
    }
}
use crate::eventlog::event::{ConversationId, EventKind};
use crate::identity::device::DeviceIdentity;
use crate::node::{
    conversation::dm_conversation_id, dm_envelope::DmEnvelope, ratchet_sessions::RatchetSessions,
};

fn account_conv() -> ConversationId {
    crate::node::conversation::account_conversation_id(&"a".repeat(32), &"b".repeat(32))
}

fn outgoing(dir: &std::path::Path, n: u8) -> DeliveryTransaction {
    let alice = DeviceIdentity::from_secret_bytes([1; 32], [2; 32]);
    let bob = DeviceIdentity::from_secret_bytes([3; 32], [4; 32]);
    let ratchet = DmRatchet::new(RatchetSessions::open(&dir.join("sessions"), "pw").unwrap());
    let body = DmEnvelope::new(
        "a".repeat(32),
        "b".repeat(32),
        [n; 32],
        b"private body".to_vec(),
    )
    .encode();
    let (wire, prepared) = ratchet
        .prepare_encrypt(&alice, &bob.public(), &body)
        .unwrap();
    let event = Event::new(
        &alice,
        dm_conversation_id(&alice.public(), &bob.public()),
        1,
        vec![],
        1,
        100,
        EventKind::Message,
        wire,
    );
    DeliveryTransaction::Outgoing {
        message: OutgoingDelivery {
            logical_id: EventId::new([n; 32]),
            sender_account: "a".repeat(32),
            recipient_account: Some("b".repeat(32)),
            conversation: account_conv(),
            wall_clock: 100,
            destinations: vec![DeliveryDestination {
                device: bob.public(),
                account: Some("b".repeat(32)),
                event,
                receipt_eligible: true,
            }],
        },
        sent: SentEntry {
            conversation: account_conv(),
            seq: 1,
            wall_clock: 100,
            plaintext: body,
        },
        ratchets: vec![prepared],
    }
}

#[test]
fn outbox_capacity_reserves_metadata_for_every_accepted_intent() {
    let dir = tempfile::tempdir().unwrap();
    let first = outgoing(dir.path(), 7);
    let metadata_size =
        bincode::serialized_size(&metadata_for(&first).unwrap()).unwrap() + FRAME_OVERHEAD;
    let marker_size = bincode::serialized_size(&OutboxRecord::Delivered(CompletedDelivery {
        logical_id: EventId::new([7; 32]),
        conversation: account_conv(),
        wall_clock: 100,
    }))
    .unwrap()
        + FRAME_OVERHEAD;
    let cancel_size =
        bincode::serialized_size(&OutboxRecord::Cancel(account_conv(), EventId::new([7; 32])))
            .unwrap()
            + FRAME_OVERHEAD;
    let mut store = DeliveryStore::open_with_limits(
        dir.path(),
        "pw",
        DeliveryLimits {
            outbox_bytes: HEADER_BYTES + metadata_size + marker_size + cancel_size + 1,
            ..DeliveryLimits::default()
        },
    )
    .unwrap();
    store.begin(first).unwrap();
    assert!(
        store.begin(outgoing(dir.path(), 8)).is_err(),
        "accepted intents must reserve their eventual outbox bytes"
    );
    assert_eq!(store.status(EventId::new([8; 32])), None);
    assert_eq!(store.pending_transactions().len(), 1);
}

#[test]
fn tight_capacity_reserves_completion_and_cancel_and_reuses_freed_slot() {
    let dir = tempfile::tempdir().unwrap();
    let first = outgoing(dir.path(), 7);
    let metadata_size =
        bincode::serialized_size(&metadata_for(&first).unwrap()).unwrap() + FRAME_OVERHEAD;
    let marker_size = bincode::serialized_size(&OutboxRecord::Delivered(CompletedDelivery {
        logical_id: EventId::new([7; 32]),
        conversation: account_conv(),
        wall_clock: 100,
    }))
    .unwrap()
        + FRAME_OVERHEAD;
    let cancel_size =
        bincode::serialized_size(&OutboxRecord::Cancel(account_conv(), EventId::new([7; 32])))
            .unwrap()
            + FRAME_OVERHEAD;
    let limits = DeliveryLimits {
        messages: 1,
        outbox_bytes: HEADER_BYTES + metadata_size + marker_size + cancel_size,
        ..DeliveryLimits::default()
    };
    let mut store = DeliveryStore::open_with_limits(dir.path(), "pw", limits).unwrap();
    store.begin(first).unwrap();
    let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
    let mut ratchet =
        DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
    let mut sent = SentLog::open(&dir.path().join("sent"), "pw").unwrap();
    let mut received = ReceivedLog::open(&dir.path().join("received"), "pw").unwrap();
    store
        .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
        .unwrap();
    store.mark_delivered(EventId::new([7; 32])).unwrap();
    store.cancel(account_conv(), EventId::new([7; 32])).unwrap();
    assert_eq!(store.status(EventId::new([7; 32])), None);
    store.begin(outgoing(dir.path(), 8)).unwrap();
    assert_eq!(
        store.status(EventId::new([8; 32])),
        Some(DeliveryStatus::Awaiting)
    );
}

#[test]
fn capacity_rejects_intent_when_completion_marker_headroom_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let first = outgoing(dir.path(), 7);
    let metadata_size =
        bincode::serialized_size(&metadata_for(&first).unwrap()).unwrap() + FRAME_OVERHEAD;
    let mut store = DeliveryStore::open_with_limits(
        dir.path(),
        "pw",
        DeliveryLimits {
            outbox_bytes: HEADER_BYTES + metadata_size,
            ..DeliveryLimits::default()
        },
    )
    .unwrap();
    assert!(
        store.begin(first).is_err(),
        "accepted messages must retain delivered/cancel headroom"
    );
    assert!(store.pending_transactions().is_empty());
}

#[test]
fn replay_rejects_pending_metadata_conflict_before_recovery() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    let tx = outgoing(dir.path(), 7);
    store.install_record(metadata_for(&tx).unwrap()).unwrap();
    let mut conflict: DeliveryTransaction =
        bincode::deserialize(&bincode::serialize(&tx).unwrap()).unwrap();
    if let DeliveryTransaction::Outgoing { message, sent, .. } = &mut conflict {
        let alice = DeviceIdentity::from_secret_bytes([1; 32], [2; 32]);
        let old = &message.destinations[0].event;
        message.destinations[0].event = Event::new(
            &alice,
            old.conversation_id,
            old.seq,
            old.parents.clone(),
            old.lamport,
            101,
            old.kind,
            old.ciphertext.clone(),
        );
        message.wall_clock = 101;
        sent.wall_clock = 101;
    }
    store.journal.append_durable(&conflict).unwrap();
    drop(store);
    assert!(
        DeliveryStore::open(dir.path(), "pw").is_err(),
        "pending metadata must match any previously installed commit"
    );
}

#[test]
fn retry_cursor_pages_past_earlier_awaiting_work_without_eviction() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    for id in [7, 8, 9] {
        store
            .install_record(metadata_for(&outgoing(dir.path(), id)).unwrap())
            .unwrap();
    }
    let first = store.retry_messages_after(None, 1);
    assert_eq!(first[0].logical_id, EventId::new([7; 32]));
    let second = store.retry_messages_after(Some(first[0].logical_id), 1);
    assert_eq!(second[0].logical_id, EventId::new([8; 32]));
    assert_eq!(
        store.retry_messages_after(Some(EventId::new([8; 32])), 2)[0].logical_id,
        EventId::new([9; 32])
    );
    assert!(store
        .retry_messages_after(Some(EventId::new([9; 32])), 2)
        .is_empty());
    assert_eq!(store.retry_messages(64).len(), 3);
}

#[test]
fn deleting_received_logical_id_does_not_cancel_other_conversation_outbound() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    store
        .install_record(metadata_for(&outgoing(dir.path(), 7)).unwrap())
        .unwrap();
    store
        .install_record(metadata_for(&incoming(dir.path())).unwrap())
        .unwrap();
    let receipt_conversation = store.retry_receipts(1)[0].conversation;
    store
        .cancel(receipt_conversation, EventId::new([7; 32]))
        .unwrap();
    assert_eq!(
        store.status(EventId::new([7; 32])),
        Some(DeliveryStatus::Awaiting),
        "incoming erasure must be scoped to its history conversation"
    );
}

#[test]
fn delivered_status_frees_awaiting_slot_and_compacts_without_ciphertext() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open_with_limits(
        dir.path(),
        "pw",
        DeliveryLimits {
            messages: 1,
            ..DeliveryLimits::default()
        },
    )
    .unwrap();
    store
        .install_record(metadata_for(&outgoing(dir.path(), 7)).unwrap())
        .unwrap();
    store.mark_delivered(EventId::new([7; 32])).unwrap();
    assert!(
        store.message(EventId::new([7; 32])).is_none(),
        "completed delivery should retain only history metadata"
    );
    store.outbox_records = store.limits.max_outbox_records();
    store
        .ensure_outbox_space(&OutboxRecord::Cancel(account_conv(), EventId::new([7; 32])))
        .unwrap();
    let (_, compacted) = EncryptedRecordLog::<OutboxRecord>::open(
        &dir.path().join("delivery-outbox.log"),
        "pw",
        OUTBOX_MAGIC,
    )
    .unwrap();
    assert_eq!(compacted.len(), 1);
    assert!(
        matches!(compacted[0], OutboxRecord::Delivered(_)),
        "completed snapshot must not retain full signed-event ciphertext"
    );
    store.begin(outgoing(dir.path(), 8)).unwrap();
    assert_eq!(
        store.status(EventId::new([7; 32])),
        Some(DeliveryStatus::Delivered)
    );
    drop(store);
    let store = DeliveryStore::open(dir.path(), "pw").unwrap();
    assert_eq!(
        store.status(EventId::new([7; 32])),
        Some(DeliveryStatus::Delivered)
    );
    assert_eq!(
        store.status(EventId::new([8; 32])),
        Some(DeliveryStatus::Awaiting)
    );
}

#[test]
fn bounded_replay_rejects_file_bytes_and_record_count_before_install() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    store.begin(outgoing(dir.path(), 7)).unwrap();
    store.begin(outgoing(dir.path(), 8)).unwrap();
    drop(store);
    assert!(DeliveryStore::open_with_limits(
        dir.path(),
        "pw",
        DeliveryLimits {
            transactions: 1,
            ..DeliveryLimits::default()
        }
    )
    .is_err());
    let path = dir.path().join("delivery-transactions.log");
    let size = std::fs::metadata(&path).unwrap().len();
    assert!(DeliveryStore::open_with_limits(
        dir.path(),
        "pw",
        DeliveryLimits {
            journal_bytes: size - 1,
            ..DeliveryLimits::default()
        }
    )
    .is_err());
    let store = DeliveryStore::open(dir.path(), "pw").unwrap();
    assert_eq!(store.pending_transactions().len(), 2);
}

#[test]
fn torn_secret_journal_tail_is_repaired_before_new_durable_append() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    store.begin(outgoing(dir.path(), 7)).unwrap();
    drop(store);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(dir.path().join("delivery-transactions.log"))
        .unwrap();
    file.write_all(&100u32.to_be_bytes()).unwrap();
    file.write_all(&[0; 5]).unwrap();
    drop(file);
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    store.begin(outgoing(dir.path(), 8)).unwrap();
    drop(store);
    let store = DeliveryStore::open(dir.path(), "pw").unwrap();
    assert_eq!(
        store
            .pending_transactions()
            .iter()
            .map(DeliveryTransaction::id)
            .collect::<Vec<_>>(),
        vec![EventId::new([7; 32]), EventId::new([8; 32])]
    );
}

#[test]
fn malformed_signed_event_is_rejected_before_intent_or_ratchet_advancement() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    let mut tx = outgoing(dir.path(), 7);
    let before = std::fs::read(dir.path().join("sessions")).unwrap();
    if let DeliveryTransaction::Outgoing { message, .. } = &mut tx {
        message.destinations[0].event.sig[0] ^= 1;
    }
    assert!(store.begin(tx).is_err());
    assert!(store.pending_transactions().is_empty());
    assert_eq!(std::fs::read(dir.path().join("sessions")).unwrap(), before);
}

#[test]
fn finished_receipt_frees_retry_slot_and_remembers_exact_original_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open_with_limits(
        dir.path(),
        "pw",
        DeliveryLimits {
            receipts: 1,
            ..DeliveryLimits::default()
        },
    )
    .unwrap();
    store
        .install_record(metadata_for(&incoming(dir.path())).unwrap())
        .unwrap();
    let receipt = store.retry_receipts(1)[0].clone();
    store
        .finish_receipt(receipt.conversation, receipt.original_event_id)
        .unwrap();
    store
        .finish_receipt(receipt.conversation, receipt.original_event_id)
        .unwrap();
    assert!(store.retry_receipts(1).is_empty());
    assert!(store.has_receipt_for(receipt.conversation, receipt.original_event_id));
    store
        .install_record(metadata_for(&incoming(dir.path())).unwrap())
        .unwrap();
    assert_eq!(store.retry_receipts(1).len(), 1);
    drop(store);
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    assert!(store.has_receipt_for(receipt.conversation, receipt.original_event_id));
    assert!(!store.has_receipt_for(account_conv(), receipt.original_event_id));
    assert_eq!(store.retry_receipts(1).len(), 1);
    store
        .cancel(receipt.conversation, receipt.original_event_id)
        .unwrap();
    assert!(!store.has_receipt_for(receipt.conversation, receipt.original_event_id));
    assert_eq!(store.retry_receipts(1).len(), 1);
}

#[test]
fn crash_after_metadata_before_secret_scrub_replays_without_duplicate_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    store.begin(outgoing(dir.path(), 7)).unwrap();
    let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
    let mut ratchet =
        DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
    let mut sent = SentLog::open(&dir.path().join("sent"), "pw").unwrap();
    let mut received = ReceivedLog::open(&dir.path().join("received"), "pw").unwrap();
    let blocker = dir.path().join("delivery-transactions.compact-tmp");
    std::fs::create_dir(&blocker).unwrap();
    assert!(store
        .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
        .is_err());
    assert_eq!(store.pending_transactions().len(), 1);
    assert_eq!(sent.entries(&account_conv()).len(), 1);
    let outbox_bytes = std::fs::metadata(dir.path().join("delivery-outbox.log"))
        .unwrap()
        .len();
    drop(store);
    drop(ratchet);
    std::fs::remove_dir(&blocker).unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    let mut ratchet =
        DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
    store
        .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
        .unwrap();
    assert_eq!(sent.entries(&account_conv()).len(), 1);
    assert_eq!(
        std::fs::metadata(dir.path().join("delivery-outbox.log"))
            .unwrap()
            .len(),
        outbox_bytes
    );
    assert!(store.pending_transactions().is_empty());
    assert_eq!(store.retry_messages(1).len(), 1);
}

#[test]
fn cancellation_requires_recovery_and_removes_status_and_retry_after_restart() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    store.begin(outgoing(dir.path(), 7)).unwrap();
    assert!(store.cancel(account_conv(), EventId::new([7; 32])).is_err());
    let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
    let mut ratchet =
        DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
    let mut sent = SentLog::open(&dir.path().join("sent"), "pw").unwrap();
    let mut received = ReceivedLog::open(&dir.path().join("received"), "pw").unwrap();
    store
        .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
        .unwrap();
    store.cancel(account_conv(), EventId::new([7; 32])).unwrap();
    drop(store);
    let store = DeliveryStore::open(dir.path(), "pw").unwrap();
    assert_eq!(store.status(EventId::new([7; 32])), None);
    assert!(store.retry_messages(8).is_empty());
}

fn incoming(dir: &std::path::Path) -> DeliveryTransaction {
    incoming_body(dir, true)
}

fn incoming_body(dir: &std::path::Path, account_envelope: bool) -> DeliveryTransaction {
    let alice = DeviceIdentity::from_secret_bytes([1; 32], [2; 32]);
    let bob = DeviceIdentity::from_secret_bytes([3; 32], [4; 32]);
    let mut sender =
        DmRatchet::new(RatchetSessions::open(&dir.join("sender.sessions"), "pw").unwrap());
    let receiver = DmRatchet::new(RatchetSessions::open(&dir.join("sessions"), "pw").unwrap());
    let body = if account_envelope {
        DmEnvelope::new(
            "c".repeat(32),
            "d".repeat(32),
            [7; 32],
            b"received private body".to_vec(),
        )
        .encode()
    } else {
        b"received private body".to_vec()
    };
    let wire = sender.encrypt(&alice, &bob.public(), &body).unwrap();
    let (plaintext, ratchet) = receiver
        .prepare_decrypt(&bob, &alice.public(), &wire)
        .unwrap();
    let original = Event::new(
        &alice,
        dm_conversation_id(&alice.public(), &bob.public()),
        1,
        vec![],
        1,
        100,
        EventKind::Message,
        wire,
    );
    let receipt = Event::new(
        &bob,
        ConversationId::new([8; 32]),
        1,
        vec![],
        1,
        101,
        EventKind::Message,
        b"encrypted metadata".to_vec(),
    );
    DeliveryTransaction::Incoming {
        sender: alice.public(),
        original: Box::new(original.clone()),
        ratchet,
        received: Box::new(ReceivedEntry {
            event_id: original.id,
            conversation: if account_envelope {
                crate::node::conversation::account_conversation_id(&"c".repeat(32), &"d".repeat(32))
            } else {
                original.conversation_id
            },
            from: if account_envelope {
                "c".repeat(32)
            } else {
                alice.public().user_id()
            },
            wall_clock: 100,
            plaintext,
        }),
        receipt: Some(Box::new(ReceiptDelivery {
            logical_id: if account_envelope {
                EventId::new([7; 32])
            } else {
                original.id
            },
            original_event_id: original.id,
            conversation: if account_envelope {
                crate::node::conversation::account_conversation_id(&"c".repeat(32), &"d".repeat(32))
            } else {
                original.conversation_id
            },
            wall_clock: original.wall_clock,
            destination: DeliveryDestination {
                device: alice.public(),
                account: Some(if account_envelope {
                    "c".repeat(32)
                } else {
                    "a".repeat(32)
                }),
                event: receipt,
                receipt_eligible: false,
            },
        })),
    }
}

#[test]
fn receive_work_survives_restart_and_receipt_waits_for_durable_history() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    let tx = incoming(dir.path());
    let id = tx.id();
    store.begin(tx).unwrap();
    assert!(store.retry_receipts(1).is_empty());
    drop(store);
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
    let mut ratchet =
        DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
    let mut sent = SentLog::open(&dir.path().join("sent"), "pw").unwrap();
    let mut received = ReceivedLog::open(&dir.path().join("received"), "pw").unwrap();
    store
        .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
        .unwrap();
    let receipt = &store.retry_receipts(1)[0];
    assert_eq!(receipt.original_event_id, id);
    assert_eq!(received.entries(&receipt.conversation).len(), 1);
    assert!(store.pending_transactions().is_empty());
    drop(store);
    assert_eq!(
        DeliveryStore::open(dir.path(), "pw")
            .unwrap()
            .retry_receipts(1)
            .len(),
        1
    );
}

#[cfg(unix)]
#[test]
fn receive_history_write_failure_keeps_intent_and_never_queues_receipt() {
    let dir = tempfile::tempdir().unwrap();
    let receive_dir = dir.path().join("receive-dir");
    let moved = dir.path().join("moved");
    std::fs::create_dir(&receive_dir).unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    store.begin(incoming(dir.path())).unwrap();
    let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
    let mut ratchet =
        DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
    let mut sent = SentLog::open(&dir.path().join("sent"), "pw").unwrap();
    let mut received = ReceivedLog::open(&receive_dir.join("received"), "pw").unwrap();
    std::fs::rename(&receive_dir, &moved).unwrap();
    assert!(store
        .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
        .is_err());
    assert_eq!(store.pending_transactions().len(), 1);
    assert!(store.retry_receipts(1).is_empty());
    std::fs::rename(&moved, &receive_dir).unwrap();
    drop(store);
    drop(ratchet);
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    let mut ratchet =
        DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
    store
        .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
        .unwrap();
    assert_eq!(store.retry_receipts(1).len(), 1);
}

#[test]
fn durable_intent_is_awaiting_and_reopens_before_install() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    store.begin(outgoing(dir.path(), 7)).unwrap();
    assert_eq!(
        store.status(EventId::new([7; 32])),
        Some(DeliveryStatus::Awaiting)
    );
    assert!(
        store.retry_messages(8).is_empty(),
        "network waits for local install"
    );
    assert_eq!(store.pending_transactions().len(), 1);
    drop(store);
    let store = DeliveryStore::open(dir.path(), "pw").unwrap();
    assert_eq!(
        store.status(EventId::new([7; 32])),
        Some(DeliveryStatus::Awaiting)
    );
    assert_eq!(store.pending_transactions()[0].id(), EventId::new([7; 32]));
}

#[test]
fn recovery_installs_once_scrubs_secrets_and_keeps_exact_retry_event() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    let tx = outgoing(dir.path(), 7);
    let exact = match &tx {
        DeliveryTransaction::Outgoing { message, .. } => message.destinations[0].event.clone(),
        _ => unreachable!(),
    };
    store.begin(tx).unwrap();
    let mut log = PersistentEventLog::open(&dir.path().join("events"), "pw").unwrap();
    let mut ratchet =
        DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
    let mut sent = SentLog::open(&dir.path().join("sent"), "pw").unwrap();
    let mut received = ReceivedLog::open(&dir.path().join("received"), "pw").unwrap();
    assert_eq!(
        store
            .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
            .unwrap(),
        Some(EventId::new([7; 32]))
    );
    assert_eq!(
        store
            .recover_next(&mut ratchet, &mut log, &mut sent, &mut received)
            .unwrap(),
        None
    );
    assert_eq!(sent.entries(&account_conv()).len(), 1);
    assert_eq!(log.get(&exact.id), Some(&exact));
    assert_eq!(
        std::fs::metadata(dir.path().join("delivery-transactions.log"))
            .unwrap()
            .len(),
        22
    );
    drop(store);
    let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
    assert!(store.pending_transactions().is_empty());
    assert_eq!(store.retry_messages(1)[0].destinations[0].event, exact);
    store.mark_delivered(EventId::new([7; 32])).unwrap();
    store.mark_delivered(EventId::new([7; 32])).unwrap();
    assert!(store.retry_messages(1).is_empty());
    drop(store);
    assert_eq!(
        DeliveryStore::open(dir.path(), "pw")
            .unwrap()
            .status(EventId::new([7; 32])),
        Some(DeliveryStatus::Delivered)
    );
}

#[test]
fn capacity_and_conflicts_reject_before_ratchet_install() {
    let dir = tempfile::tempdir().unwrap();
    let mut store = DeliveryStore::open_with_limits(
        dir.path(),
        "pw",
        DeliveryLimits {
            messages: 1,
            ..DeliveryLimits::default()
        },
    )
    .unwrap();
    let tx = outgoing(dir.path(), 7);
    let mut conflict: DeliveryTransaction =
        bincode::deserialize(&bincode::serialize(&tx).unwrap()).unwrap();
    store.begin(tx).unwrap();
    let before = std::fs::read(dir.path().join("sessions")).unwrap();
    assert!(store.begin(outgoing(dir.path(), 8)).is_err());
    if let DeliveryTransaction::Outgoing { sent, .. } = &mut conflict {
        sent.wall_clock = 99;
    }
    assert!(store.begin(conflict).is_err());
    assert_eq!(std::fs::read(dir.path().join("sessions")).unwrap(), before);
    assert_eq!(store.pending_transactions().len(), 1);
    assert!(DeliveryStore::open_with_limits(
        dir.path(),
        "pw",
        DeliveryLimits {
            journal_bytes: 22,
            ..DeliveryLimits::default()
        }
    )
    .is_err());
}

#[test]
fn legacy_device_transaction_preserves_optional_certified_account_binding() {
    for certified in [true, false] {
        let dir = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::from_secret_bytes([1; 32], [2; 32]);
        let bob = DeviceIdentity::from_secret_bytes([3; 32], [4; 32]);
        let ratchet =
            DmRatchet::new(RatchetSessions::open(&dir.path().join("sessions"), "pw").unwrap());
        let (wire, prepared) = ratchet
            .prepare_encrypt(&alice, &bob.public(), b"legacy body")
            .unwrap();
        let conv = dm_conversation_id(&alice.public(), &bob.public());
        let event = Event::new(&alice, conv, 1, vec![], 1, 100, EventKind::Message, wire);
        let id = event.id;
        let account = certified.then(|| "b".repeat(32));
        let tx = DeliveryTransaction::Outgoing {
            message: OutgoingDelivery {
                logical_id: id,
                sender_account: "a".repeat(32),
                recipient_account: account.clone(),
                conversation: conv,
                wall_clock: 100,
                destinations: vec![DeliveryDestination {
                    device: bob.public(),
                    account: account.clone(),
                    event,
                    receipt_eligible: certified,
                }],
            },
            sent: SentEntry {
                conversation: conv,
                seq: 1,
                wall_clock: 100,
                plaintext: b"legacy body".to_vec(),
            },
            ratchets: vec![prepared],
        };
        let mut store = DeliveryStore::open(dir.path(), "pw").unwrap();
        store.begin(tx).unwrap();
        drop(store);
        let store = DeliveryStore::open(dir.path(), "pw").unwrap();
        assert_eq!(store.status(id), Some(DeliveryStatus::Awaiting));
        match &store.pending_transactions()[0] {
            DeliveryTransaction::Outgoing { message, .. } => {
                assert_eq!(message.destinations[0].account, account)
            }
            _ => unreachable!(),
        }
    }
}

#[cfg(unix)]
#[test]
fn failed_durable_intent_does_not_install_or_publish_status() {
    let dir = tempfile::tempdir().unwrap();
    let parent = dir.path().join("profile");
    let moved = dir.path().join("moved");
    let mut store = DeliveryStore::open(&parent, "pw").unwrap();
    let tx = outgoing(&parent, 7);
    std::fs::rename(&parent, &moved).unwrap();
    assert!(store.begin(tx).is_err());
    assert_eq!(store.status(EventId::new([7; 32])), None);
    std::fs::rename(&moved, &parent).unwrap();
    drop(store);
    assert!(DeliveryStore::open(&parent, "pw")
        .unwrap()
        .pending_transactions()
        .is_empty());
}
