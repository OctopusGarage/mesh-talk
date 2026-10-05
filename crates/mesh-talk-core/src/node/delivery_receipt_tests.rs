use super::*;

#[test]
fn control_ciphertext_is_not_legacy_chat_ciphertext() {
    let f = Fixture::new(true);
    let event = f.event_for(&f.payload());
    assert!(f.authenticate(&event).is_some());
    assert!(
        crate::dm::open(&f.alice, &f.bob.public().x25519_pub, &event.ciphertext).is_err(),
        "a legacy sealed-box chat opener must reject delivery controls"
    );
    let dir = tempfile::tempdir().unwrap();
    let mut ratchet =
        DmRatchet::new(RatchetSessions::open(&dir.path().join("legacy"), "pw").unwrap());
    assert!(ratchet
        .decrypt(&f.alice, &f.bob.public(), &event.ciphertext)
        .is_err());
    // A still older permissive bincode parser sees only an empty Vec; its
    // normal AEAD opener cannot authenticate that as a chat message either.
    let legacy: crate::dm::SealedEnvelope = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .allow_trailing_bytes()
        .deserialize(&event.ciphertext)
        .unwrap();
    assert!(legacy.ciphertext.is_empty());
    assert!(crate::dm::open(
        &f.alice,
        &f.bob.public().x25519_pub,
        &bincode::serialize(&legacy).unwrap()
    )
    .is_err());
}

#[test]
fn receipt_only_frame_is_exact_and_rejects_missing_wrong_nonzero_or_truncated_marker() {
    let f = Fixture::new(true);
    let valid = f.event_for(&f.payload());
    assert!(valid.ciphertext.starts_with(&RECEIPT_FRAME));
    assert_eq!(&valid.ciphertext[32..40], &[0; 8]);
    for case in 0..5 {
        let mut wire = valid.ciphertext.clone();
        match case {
            0 => wire = wire[40..].to_vec(),
            1 => wire[0] ^= 1,
            2 => wire[24] += 1,
            3 => wire[32] = 1,
            4 => wire.truncate(39),
            _ => unreachable!(),
        }
        let event = Event::new(
            &f.bob,
            valid.conversation_id,
            1,
            vec![],
            1,
            valid.wall_clock,
            EventKind::Message,
            wire,
        );
        assert!(event.verify_integrity() && event.verify_signature());
        assert!(
            f.authenticate(&event).is_none(),
            "receipt frame case {case}"
        );
    }
}
use crate::identity::account::Account;
use crate::node::delivery_store::DeliveryDestination;
use crate::node::{dm_ratchet::DmRatchet, ratchet_sessions::RatchetSessions};

fn authenticate_receipt(
    sender: &DeviceIdentity,
    sender_account: &str,
    target_proof: &Announce,
    message: &OutgoingDelivery,
    event: &Event,
) -> Option<AuthenticatedReceipt> {
    open_receipt(sender, sender_account, target_proof, event)?.authenticate(message)
}

struct Fixture {
    alice: DeviceIdentity,
    bob: DeviceIdentity,
    alice_account: Account,
    bob_account: Account,
    alice_proof: Announce,
    bob_proof: Announce,
    message: OutgoingDelivery,
    received: ReceivedEntry,
}

impl Fixture {
    fn new(account_addressed: bool) -> Self {
        Self::with_accounts(account_addressed, false)
    }

    fn with_accounts(account_addressed: bool, own_copy: bool) -> Self {
        let alice = DeviceIdentity::generate();
        let bob = DeviceIdentity::generate();
        let alice_account = Account::generate();
        let bob_account = if own_copy {
            Account::from_secret_bytes(alice_account.secret_bytes())
        } else {
            Account::generate()
        };
        let alice_proof = Announce::new_with_account(&alice, &alice_account, "a", 1);
        let bob_proof = Announce::new_with_account(&bob, &bob_account, "b", 2);
        let dm_conv = dm_conversation_id(&alice.public(), &bob.public());
        let logical_id = EventId::new([7; 32]);
        let plaintext = if account_addressed {
            DmEnvelope::new(
                alice_account.account_id(),
                bob_account.account_id(),
                *logical_id.as_bytes(),
                b"message text absent from receipt".to_vec(),
            )
            .encode()
        } else {
            b"legacy device message".to_vec()
        };
        let dir = tempfile::tempdir().unwrap();
        let a_ratchet = DmRatchet::new(RatchetSessions::open(&dir.path().join("a"), "pw").unwrap());
        let b_ratchet = DmRatchet::new(RatchetSessions::open(&dir.path().join("b"), "pw").unwrap());
        let (wire, _) = a_ratchet
            .prepare_encrypt(&alice, &bob.public(), &plaintext)
            .unwrap();
        let (opened, _) = b_ratchet
            .prepare_decrypt(&bob, &alice.public(), &wire)
            .unwrap();
        assert_eq!(opened, plaintext);
        let original = Event::new(&alice, dm_conv, 1, vec![], 1, 100, EventKind::Message, wire);
        let history_conv = if account_addressed {
            account_conversation_id(&alice_account.account_id(), &bob_account.account_id())
        } else {
            dm_conv
        };
        let received = ReceivedEntry {
            event_id: original.id,
            conversation: history_conv,
            from: if account_addressed {
                alice_account.account_id()
            } else {
                alice.user_id()
            },
            wall_clock: original.wall_clock,
            plaintext: opened,
        };
        let message = OutgoingDelivery {
            logical_id: if account_addressed {
                logical_id
            } else {
                original.id
            },
            sender_account: alice_account.account_id(),
            recipient_account: Some(bob_account.account_id()),
            conversation: history_conv,
            wall_clock: original.wall_clock,
            destinations: vec![DeliveryDestination {
                device: bob.public(),
                account: Some(bob_account.account_id()),
                event: original,
                receipt_eligible: true,
            }],
        };
        Self {
            alice,
            bob,
            alice_account,
            bob_account,
            alice_proof,
            bob_proof,
            message,
            received,
        }
    }

    fn payload(&self) -> ReceiptPayload {
        ReceiptPayload::prepare(
            &self.bob.public(),
            &self.bob_account.account_id(),
            &self.alice_proof,
            &self.message.destinations[0].event,
            &self.received,
            50,
        )
        .unwrap()
    }

    fn event_for(&self, payload: &ReceiptPayload) -> Event {
        let wire = payload.seal(&self.bob).unwrap();
        Event::new(
            &self.bob,
            delivery_conversation_id(&self.alice.public(), &self.bob.public()),
            1,
            vec![],
            1,
            payload.confirmed_at,
            EventKind::Message,
            wire,
        )
    }

    fn authenticate(&self, event: &Event) -> Option<AuthenticatedReceipt> {
        authenticate_receipt(
            &self.alice,
            &self.alice_account.account_id(),
            &self.bob_proof,
            &self.message,
            event,
        )
    }
}

#[test]
fn control_scope_is_symmetric_separate_and_has_fixed_domain() {
    use sha2::{Digest, Sha256};
    let a = DeviceIdentity::generate().public();
    let b = DeviceIdentity::generate().public();
    let c = DeviceIdentity::generate().public();
    let scope = delivery_conversation_id(&a, &b);
    assert_eq!(scope, delivery_conversation_id(&b, &a));
    assert_ne!(scope, dm_conversation_id(&a, &b));
    assert_ne!(scope, delivery_conversation_id(&a, &c));
    let mut pair = [a.ed25519_pub, b.ed25519_pub];
    pair.sort();
    let mut hash = Sha256::new();
    hash.update(b"mesh-talk-dm-delivery-v1");
    hash.update(pair[0]);
    hash.update(pair[1]);
    assert_eq!(scope, ConversationId::new(hash.finalize().into()));
}

#[test]
fn authenticated_metadata_routes_lookup_but_only_exact_outbox_binding_confirms() {
    let f = Fixture::new(true);
    let payload = f.payload();
    assert_eq!(
        payload.conversation(),
        delivery_conversation_id(&f.alice.public(), &f.bob.public())
    );
    assert_eq!(payload.confirmed_at(), 50);
    let event = f.event_for(&payload);
    let opened = open_receipt(
        &f.alice,
        &f.alice_account.account_id(),
        &f.bob_proof,
        &event,
    )
    .unwrap();
    assert_eq!(opened.logical_id(), f.message.logical_id);
    assert_eq!(
        opened.authenticate(&f.message).unwrap().logical_id(),
        f.message.logical_id
    );
    let mut other = f.message.clone();
    other.logical_id = EventId::new([4; 32]);
    assert!(opened.authenticate(&other).is_none());
    assert!(payload.seal(&f.alice).is_err());
}

#[test]
fn actual_signed_sealed_receipt_binds_account_and_raw_device_messages() {
    for account_addressed in [true, false] {
        let f = Fixture::new(account_addressed);
        let payload = f.payload();
        let event = f.event_for(&payload);
        assert!(event.verify_integrity() && event.verify_signature());
        let proof = f.authenticate(&event).unwrap();
        assert_eq!(proof.logical_id(), f.message.logical_id);
        assert_eq!(proof.original_event_id(), f.received.event_id);
        // Clock skew must not invalidate receipt confirmation.
        assert_eq!(proof.confirmed_at(), 50);
        assert_eq!(
            f.authenticate(&event).unwrap().logical_id(),
            proof.logical_id()
        );
        assert!(!payload
            .encode()
            .unwrap()
            .windows(12)
            .any(|w| w == b"message text"));
        let outsider = DeviceIdentity::generate();
        assert!(crate::dm::open(&outsider, &f.bob.public().x25519_pub, &event.ciphertext).is_err());
    }
}

#[test]
fn signed_sealed_payload_changes_cannot_confirm_a_different_binding() {
    let f = Fixture::new(true);
    for case in 0..11 {
        let mut payload = f.payload();
        match case {
            0 => payload.original_event_id = EventId::new([9; 32]),
            1 => payload.logical_id = EventId::new([9; 32]),
            2 => payload.original_conversation = ConversationId::new([9; 32]),
            3 => payload.sender_account = "9".repeat(32),
            4 => payload.recipient_account = "9".repeat(32),
            5 => payload.sender_device.x25519_pub = [9; 32],
            6 => payload.recipient_device.x25519_pub = [9; 32],
            7 => payload.sender_device.ed25519_pub = [9; 32],
            8 => payload.recipient_device.ed25519_pub = [9; 32],
            9 => payload.original_wall_clock += 1,
            10 => payload.original_kind = EventKind::FileManifest,
            _ => unreachable!(),
        }
        // Adversarial recipient can sign arbitrary payload bytes; avoid the safe mint API.
        let bytes = bincode::serialize(&payload).unwrap();
        let wire =
            frame_receipt(crate::dm::seal(&f.bob, &f.alice.public().x25519_pub, &bytes).unwrap());
        let event = Event::new(
            &f.bob,
            delivery_conversation_id(&f.alice.public(), &f.bob.public()),
            1,
            vec![],
            1,
            payload.confirmed_at,
            EventKind::Message,
            wire,
        );
        assert!(f.authenticate(&event).is_none(), "binding case {case}");
    }
}

#[test]
fn wrong_signature_ciphertext_scope_kind_and_confirmation_time_are_ignored() {
    let f = Fixture::new(true);
    let valid = f.event_for(&f.payload());
    for case in 0..7 {
        let mut event = valid.clone();
        match case {
            0 => event.sig[0] ^= 1,
            1 => event.ciphertext[45] ^= 1,
            2 => event.conversation_id = f.message.destinations[0].event.conversation_id,
            3 => event.kind = EventKind::ReadMarker,
            4 => event.wall_clock += 1,
            5 => {
                let outsider = DeviceIdentity::generate();
                event = Event::new(
                    &outsider,
                    valid.conversation_id,
                    1,
                    vec![],
                    1,
                    50,
                    EventKind::Message,
                    valid.ciphertext.clone(),
                );
            }
            6 => {
                let bytes = f.payload().encode().unwrap();
                let outsider = DeviceIdentity::generate();
                let wire = frame_receipt(
                    crate::dm::seal(&outsider, &f.alice.public().x25519_pub, &bytes).unwrap(),
                );
                event = Event::new(
                    &f.bob,
                    valid.conversation_id,
                    1,
                    vec![],
                    1,
                    50,
                    EventKind::Message,
                    wire,
                );
            }
            _ => unreachable!(),
        }
        if (1..=4).contains(&case) {
            event = Event::new(
                &f.bob,
                event.conversation_id,
                1,
                vec![],
                1,
                event.wall_clock,
                event.kind,
                event.ciphertext,
            );
        }
        assert!(f.authenticate(&event).is_none(), "event case {case}");
    }
}

#[test]
fn strict_decoder_rejects_versions_domains_trailing_oversize_and_noncanonical_accounts() {
    let f = Fixture::new(true);
    let valid = f.payload();
    assert_eq!(
        ReceiptPayload::decode(&valid.encode().unwrap()),
        Some(valid.clone())
    );
    for case in 0..6 {
        let mut payload = valid.clone();
        match case {
            0 => payload.version += 1,
            1 => payload.domain[0] ^= 1,
            2 => payload.sender_account = "A".repeat(32),
            3 => payload.recipient_account = "z".repeat(32),
            4 => payload.sender_account = "a".repeat(33),
            5 => payload.recipient_account.clear(),
            _ => unreachable!(),
        }
        assert!(
            ReceiptPayload::decode(&bincode::serialize(&payload).unwrap()).is_none(),
            "decode case {case}"
        );
    }
    let mut trailing = valid.encode().unwrap();
    trailing.push(0);
    assert!(ReceiptPayload::decode(&trailing).is_none());
    assert!(ReceiptPayload::decode(&vec![0; MAX_RECEIPT_PLAINTEXT + 1]).is_none());
    let mut forged_length = valid.encode().unwrap();
    // Fixed header: version + domain + kind + 3 IDs + timestamp + 2 full devices.
    let account_offset = 1 + 24 + 4 + 32 * 3 + 8 + 64 * 2;
    forged_length[account_offset..account_offset + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(ReceiptPayload::decode(&forged_length).is_none());
    let mut event = f.event_for(&valid);
    event.ciphertext = vec![0; MAX_RECEIPT_WIRE + 1];
    assert!(f.authenticate(&event).is_none());
}

#[test]
fn mint_rejects_wrong_recipient_own_copies_malformed_routes_and_history() {
    let f = Fixture::new(true);
    for case in 0..9 {
        let mut received = f.received.clone();
        let mut original = f.message.destinations[0].event.clone();
        match case {
            0 => received.event_id = EventId::new([8; 32]),
            1 => received.wall_clock += 1,
            2 => received.conversation = ConversationId::new([8; 32]),
            3 => received.from = "f".repeat(32),
            4 | 5 => {
                let mut env = DmEnvelope::decode(&received.plaintext).unwrap();
                if case == 4 {
                    env.route.recipient_account = "f".repeat(32);
                } else {
                    env.route.sender_account = "f".repeat(32);
                }
                received.plaintext = env.encode();
            }
            6 => received.plaintext = b"MTDE1malformed".to_vec(),
            7 => original.sig[0] ^= 1,
            8 => original.conversation_id = ConversationId::new([8; 32]),
            _ => unreachable!(),
        }
        assert!(
            ReceiptPayload::prepare(
                &f.bob.public(),
                &f.bob_account.account_id(),
                &f.alice_proof,
                &original,
                &received,
                50
            )
            .is_none(),
            "mint case {case}"
        );
    }
    assert!(ReceiptPayload::prepare(
        &f.alice.public(),
        &f.alice_account.account_id(),
        &f.alice_proof,
        &f.message.destinations[0].event,
        &f.received,
        50
    )
    .is_none());
    assert!(ReceiptPayload::prepare(
        &f.bob.public(),
        &f.alice_account.account_id(),
        &f.alice_proof,
        &f.message.destinations[0].event,
        &f.received,
        50
    )
    .is_none());
}

#[test]
fn proof_requires_device_signed_account_and_exact_immutable_eligible_target() {
    let mut f = Fixture::new(true);
    let event = f.event_for(&f.payload());
    for case in 0..13 {
        let mut message = f.message.clone();
        let mut proof = f.bob_proof.clone();
        match case {
            0 => message.destinations[0].receipt_eligible = false,
            1 => message.destinations[0].account = None,
            2 => message.destinations[0].device.x25519_pub = [8; 32],
            3 => message.sender_account = "f".repeat(32),
            4 => message.recipient_account = Some("f".repeat(32)),
            5 => {
                proof.account_cert = Some(Account::generate().certify(&f.bob.public().ed25519_pub))
            }
            6 => message.conversation = ConversationId::new([8; 32]),
            7 => message.destinations[0].event.sig[0] ^= 1,
            8 => message.wall_clock += 1,
            9 => {
                let original = &message.destinations[0].event;
                message.destinations[0].event = Event::new(
                    &f.alice,
                    original.conversation_id,
                    original.seq,
                    original.parents.clone(),
                    original.lamport,
                    original.wall_clock,
                    EventKind::FileManifest,
                    original.ciphertext.clone(),
                );
            }
            10 => message.destinations[0].event.id = EventId::new([8; 32]),
            11 => {
                // Same Ed25519 key, a different actual X25519 identity, with a
                // completely valid device-signed announcement and account certificate.
                let substituted =
                    DeviceIdentity::from_secret_bytes(f.bob.secret_bytes().0, [8; 32]);
                proof = Announce::new_with_account(&substituted, &f.bob_account, "changed DH", 2);
                assert!(proof.verify());
            }
            12 => {
                let original = &message.destinations[0].event;
                message.destinations[0].event = Event::new(
                    &f.alice,
                    original.conversation_id,
                    original.seq,
                    original.parents.clone(),
                    original.lamport,
                    original.wall_clock + 1,
                    original.kind,
                    original.ciphertext.clone(),
                );
            }
            _ => unreachable!(),
        }
        assert!(
            authenticate_receipt(
                &f.alice,
                &f.alice_account.account_id(),
                &proof,
                &message,
                &event
            )
            .is_none(),
            "proof case {case}"
        );
    }
    f.bob_proof = Announce::new_with_account(&f.bob, &Account::generate(), "rebound", 2);
    assert!(f.authenticate(&event).is_none());
    f.bob_proof = Announce::new(&f.bob, "legacy", 2);
    assert!(f.authenticate(&event).is_none());
}

#[test]
fn sealed_box_frame_length_is_bounded_before_the_existing_decoder_allocates() {
    let f = Fixture::new(true);
    let event = f.event_for(&f.payload());
    assert!(valid_sealed_wire(&event.ciphertext));
    for case in 0..4 {
        let mut wire = event.ciphertext.clone();
        match case {
            0 => wire[72..80].copy_from_slice(&u64::MAX.to_le_bytes()),
            1 => wire.push(0),
            2 => wire.truncate(39),
            3 => {
                wire = vec![0; 40 + MAX_RECEIPT_PLAINTEXT + 17];
                wire[32..40].copy_from_slice(&((MAX_RECEIPT_PLAINTEXT + 17) as u64).to_le_bytes());
                wire = frame_receipt(wire);
            }
            _ => unreachable!(),
        }
        assert!(!valid_sealed_wire(&wire));
        let malformed = Event::new(
            &f.bob,
            event.conversation_id,
            1,
            vec![],
            1,
            50,
            EventKind::Message,
            wire,
        );
        assert!(malformed.verify_integrity() && malformed.verify_signature());
        assert!(f.authenticate(&malformed).is_none());
    }
}

#[test]
fn authenticated_own_account_copy_never_mints_or_confirms_delivery() {
    let own = Fixture::with_accounts(true, true);
    assert!(own.alice_proof.verify() && own.bob_proof.verify());
    assert_eq!(own.alice_account.account_id(), own.bob_account.account_id());
    assert!(ReceiptPayload::prepare(
        &own.bob.public(),
        &own.bob_account.account_id(),
        &own.alice_proof,
        &own.message.destinations[0].event,
        &own.received,
        50
    )
    .is_none());
    let mut payload = Fixture::new(true).payload();
    payload.sender_device = own.alice.public();
    payload.recipient_device = own.bob.public();
    payload.sender_account = own.alice_account.account_id();
    payload.recipient_account = own.bob_account.account_id();
    payload.original_conversation = own.message.destinations[0].event.conversation_id;
    payload.original_event_id = own.received.event_id;
    payload.logical_id = own.message.logical_id;
    let wire = frame_receipt(
        crate::dm::seal(
            &own.bob,
            &own.alice.public().x25519_pub,
            &bincode::serialize(&payload).unwrap(),
        )
        .unwrap(),
    );
    let event = Event::new(
        &own.bob,
        payload.conversation(),
        1,
        vec![],
        1,
        50,
        EventKind::Message,
        wire,
    );
    assert!(own.authenticate(&event).is_none());
}

#[test]
fn framed_message_trailing_and_forged_lengths_do_not_fall_back_to_raw_receipts() {
    let f = Fixture::new(true);
    for case in 0..3 {
        let mut received = f.received.clone();
        match case {
            0 => received.plaintext.push(0),
            1 => received.plaintext[5..13].copy_from_slice(&u64::MAX.to_le_bytes()),
            2 => received.plaintext.resize(MAX_DM_PLAINTEXT + 1, 0),
            _ => unreachable!(),
        }
        assert!(ReceiptPayload::prepare(
            &f.bob.public(),
            &f.bob_account.account_id(),
            &f.alice_proof,
            &f.message.destinations[0].event,
            &received,
            50
        )
        .is_none());
    }
    let legacy_source = Announce::new(&f.alice, "accountless", 1);
    assert!(ReceiptPayload::prepare(
        &f.bob.public(),
        &f.bob_account.account_id(),
        &legacy_source,
        &f.message.destinations[0].event,
        &f.received,
        50
    )
    .is_none());
}

#[test]
fn valid_signature_does_not_override_canonical_parents_or_raw_logical_id() {
    let f = Fixture::new(false);
    let event = f.event_for(&f.payload());
    let mut message = f.message.clone();
    message.logical_id = EventId::new([8; 32]);
    assert!(authenticate_receipt(
        &f.alice,
        &f.alice_account.account_id(),
        &f.bob_proof,
        &message,
        &event
    )
    .is_none());
    let mut noncanonical = event.clone();
    noncanonical.parents = vec![EventId::new([2; 32]), EventId::new([1; 32])];
    noncanonical.id = noncanonical.recompute_id();
    let mut signing_input = b"mesh-talk-event-v1".to_vec();
    signing_input.extend_from_slice(noncanonical.id.as_bytes());
    noncanonical.sig = f.bob.sign(&signing_input).to_vec();
    assert!(noncanonical.verify_signature() && noncanonical.verify_integrity());
    assert!(!noncanonical.is_canonical());
    assert!(f.authenticate(&noncanonical).is_none());
}
