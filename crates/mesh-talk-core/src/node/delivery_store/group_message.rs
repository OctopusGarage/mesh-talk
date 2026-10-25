//! Strict account-operation binding for ordinary sender-key text and sticker batches.

use super::{DeliveryTransaction, OperationReference, RequestRecord, invalid};
use crate::{
    eventlog::{EventKind, LogError},
    node::{
        AccountGroupOperation, AccountGroupOperationKind, AccountGroupRole, MessageDestination,
        PreparedAccountGroupMessage,
    },
};

/// Bind the narrow sender-key batch to its original signed account intent.
/// The caller retains profile/current-source authorization and the ordinary
/// ChannelOutgoing validator, including sender snapshot decoding and limits.
pub(super) fn validate(
    operation: &OperationReference,
    request: &RequestRecord,
    body: &DeliveryTransaction,
) -> Result<(), LogError> {
    let DeliveryTransaction::ChannelOutgoing { sent, .. } = body else {
        return Err(invalid("invalid group message effect kind"));
    };
    if !matches!(
        operation.kind,
        AccountGroupOperationKind::Text | AccountGroupOperationKind::Sticker
    ) {
        return Err(invalid("invalid group message operation kind or role"));
    }
    let prepared =
        PreparedAccountGroupMessage::from_plaintext(operation.kind, sent.plaintext.clone())
            .ok_or_else(|| invalid("invalid group message plaintext"))?;
    validate_prepared(
        operation,
        request,
        body,
        &AccountGroupOperation::Send { message: prepared },
        &sent.plaintext,
        EventKind::Message,
    )
}

pub(super) fn validate_prepared(
    operation: &OperationReference,
    request: &RequestRecord,
    body: &DeliveryTransaction,
    input: &AccountGroupOperation,
    plaintext: &[u8],
    kind: EventKind,
) -> Result<(), LogError> {
    let DeliveryTransaction::ChannelOutgoing {
        events,
        sent,
        senders,
    } = body
    else {
        return Err(invalid("invalid group message effect kind"));
    };
    if operation.kind != input.kind()
        || !matches!(
            operation.executor.role,
            AccountGroupRole::Member | AccountGroupRole::Owner
        )
    {
        return Err(invalid("invalid group message operation kind or role"));
    }
    let (rotation, message) = match events.as_slice() {
        [message] if message.kind == kind => (None, message),
        [rotation, message] if rotation.kind == EventKind::KeyRotation && message.kind == kind => {
            (Some(rotation), message)
        }
        _ => return Err(invalid("invalid group message effect batch")),
    };
    if events.iter().any(|event| {
        event.conversation_id != operation.group
            || event.author.ed25519_pub() != &operation.executor.device.ed25519_pub
            || event.seq == 0
            || event.lamport == 0
            || event.parents.is_empty()
            || !event.is_canonical()
            || !event.verify_integrity()
            || !event.verify_signature()
            || !crate::node::session::event_fits_frame(event)
    }) {
        return Err(invalid("invalid group message source event"));
    }
    if let Some(rotation) = rotation {
        let sealed = crate::node::channel::SealedKeys::decode(&rotation.ciphertext)
            .ok_or_else(|| invalid("invalid group message sender distribution"))?;
        let mut recipients = std::collections::HashSet::new();
        if sealed.epoch != operation.executor.authority.epoch
            || sealed.sender != operation.executor.device.user_id()
            || sealed.entries.is_empty()
            || sealed
                .entries
                .iter()
                .any(|entry| entry.sealed.is_empty() || !recipients.insert(entry.user_id.as_str()))
            || !recipients.contains(operation.executor.device.user_id().as_str())
            || message.parents != [rotation.id]
            || rotation.seq.checked_add(1) != Some(message.seq)
            || rotation.lamport.checked_add(1) != Some(message.lamport)
            || rotation.wall_clock != message.wall_clock
        {
            return Err(invalid("invalid group message rotation binding"));
        }
    }
    let mut epochs = std::collections::HashSet::new();
    if senders.iter().any(|(epoch, bytes)| {
        !epochs.insert(*epoch)
            || crate::channel::sender_key::SenderKey::deserialize(bytes).is_none()
    }) || !epochs.contains(&operation.executor.authority.epoch)
        || sent.conversation != operation.group
        || sent.seq != message.seq
        || sent.wall_clock != message.wall_clock
        || request.request_id != *operation.id.as_bytes()
        || request.digest != operation.digest
        || request.message_id != message.id
        || request.destination != MessageDestination::Channel(operation.group)
    {
        return Err(invalid("invalid group message sidecar binding"));
    }
    if sent.plaintext != plaintext
        || crate::node::account_group::operations::journal::operation_digest(operation.group, input)
            != operation.digest
    {
        return Err(invalid(
            "invalid group message operation digest or exact plaintext",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::*;
    use crate::{
        channel::{ChannelMeta, ChannelState},
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
        node::{
            AccountGroupExecution, AccountGroupOperationId, AccountGroupOperationKind,
            AccountGroupRole, AccountGroupSource, AuthorityExpectation, MembershipLifetime,
            MessageDestination,
        },
    };

    struct Fixture {
        dir: tempfile::TempDir,
        owner: DeviceIdentity,
        origin: DeviceIdentity,
        account: Account,
        genesis: Event,
        tx: DeliveryTransaction,
    }

    fn fixture(kind: AccountGroupOperationKind) -> Fixture {
        fixture_with_role(kind, AccountGroupRole::Owner)
    }

    fn fixture_with_role(kind: AccountGroupOperationKind, role: AccountGroupRole) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let owner = DeviceIdentity::generate();
        let admin = DeviceIdentity::generate();
        let genesis_author = if role == AccountGroupRole::Owner {
            &owner
        } else {
            &admin
        };
        let origin = DeviceIdentity::generate();
        let account = Account::generate();
        let group = ConversationId::new([0xa1; 32]);
        let meta = ChannelMeta {
            name: "Send WAL".into(),
            members: if role == AccountGroupRole::Owner {
                vec![owner.public()]
            } else {
                vec![admin.public(), owner.public()]
            },
            epoch: 0,
            owner: genesis_author.user_id(),
        };
        let genesis = Event::new(
            genesis_author,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            meta.encode(),
        );
        let plaintext = if kind == AccountGroupOperationKind::Sticker {
            crate::node::message::MessageBody::sticker("1f602".into(), "😂".as_bytes().to_vec())
                .encode()
        } else {
            crate::node::message::MessageBody::new(b"bounded send".to_vec(), None).encode()
        };
        let mut state = ChannelState::from_meta(group, meta);
        state.set_identity(owner.user_id());
        let (payload, distribution, senders) = state.prepare_sender_message(&plaintext).unwrap();
        let sealed = crate::node::channel::seal_keys_for(
            &owner,
            &state.recipient_members(),
            &distribution.unwrap(),
            0,
        )
        .unwrap();
        let first_seq = if role == AccountGroupRole::Owner {
            2
        } else {
            1
        };
        let rotation = Event::new(
            &owner,
            group,
            first_seq,
            vec![genesis.id],
            2,
            2,
            EventKind::KeyRotation,
            sealed.encode(),
        );
        let event = Event::new(
            &owner,
            group,
            first_seq + 1,
            vec![rotation.id],
            3,
            2,
            EventKind::Message,
            payload,
        );
        // Use the original operation's serde shape without requiring broader field visibility.
        let operation: crate::node::AccountGroupOperation =
            serde_json::from_value(serde_json::json!({
                "Send": {"message": {"kind": kind, "encoded_body": plaintext, "staged_assets": []}}
            }))
            .unwrap();
        let mut reference = OperationReference {
            id: AccountGroupOperationId::for_origin(&origin.public(), [0xa2; 16]),
            group,
            kind,
            digest: crate::node::account_group::operations::journal::operation_digest(
                group, &operation,
            ),
            origin: Announce::new_with_account(&origin, &account, "Origin", 9),
            executor: AccountGroupSource {
                device: owner.public(),
                role,
                lifetime: MembershipLifetime {
                    member: owner.public(),
                    membership: genesis.id,
                    acceptance: None,
                },
                authority: AuthorityExpectation {
                    authority: None,
                    epoch: 0,
                },
                execution: AccountGroupExecution::Local,
            },
            signature: Vec::new(),
        };
        reference.signature = origin.sign(&reference.message()).to_vec();
        let tx = DeliveryTransaction::GroupRequested {
            operation: reference.clone(),
            transaction: Box::new(DeliveryTransaction::Requested {
                request: RequestRecord {
                    request_id: *reference.id.as_bytes(),
                    digest: reference.digest,
                    destination: MessageDestination::Channel(group),
                    message_id: event.id,
                    accepted_at_ms: crate::node::node::now_millis(),
                },
                transaction: Box::new(DeliveryTransaction::ChannelOutgoing {
                    events: vec![rotation, event],
                    sent: crate::node::sentlog::SentEntry {
                        conversation: group,
                        seq: first_seq + 1,
                        wall_clock: 2,
                        plaintext,
                    },
                    senders,
                }),
            }),
        };
        Fixture {
            dir,
            owner,
            origin,
            account,
            genesis,
            tx,
        }
    }

    fn store(f: &Fixture) -> DeliveryStore {
        let mut store = DeliveryStore::open(f.dir.path(), "pw").unwrap();
        let path = f.dir.path().join("events");
        let mut log = PersistentEventLog::open(&path, "pw").unwrap();
        log.append_durable(f.genesis.clone()).unwrap();
        store
            .bind_profile(&path, "pw", &f.owner.public(), &f.account.account_id())
            .unwrap();
        store
    }

    #[test]
    fn typed_text_enters_original_requested_sender_wal() {
        let f = fixture(AccountGroupOperationKind::Text);
        let mut store = store(&f);
        let id = f.tx.id();
        assert_eq!(
            store
                .begin(f.tx)
                .expect("typed text sender batch must enter WAL"),
            id
        );
        assert_eq!(store.pending_transactions()[0].id(), id);
    }

    #[test]
    fn typed_sticker_enters_original_requested_sender_wal() {
        let f = fixture(AccountGroupOperationKind::Sticker);
        let mut store = store(&f);
        store
            .begin(f.tx)
            .expect("typed sticker sender batch must enter WAL");
    }

    #[test]
    fn physical_member_text_enters_original_requested_sender_wal() {
        let f = fixture_with_role(AccountGroupOperationKind::Text, AccountGroupRole::Member);
        let mut store = store(&f);
        store
            .begin(f.tx)
            .expect("physical members can send without owner authority");
    }

    #[test]
    fn typed_send_without_rotation_enters_original_requested_sender_wal() {
        let mut f = fixture(AccountGroupOperationKind::Text);
        let mut store = store(&f);
        let DeliveryTransaction::GroupRequested { transaction, .. } = &mut f.tx else {
            unreachable!()
        };
        let DeliveryTransaction::Requested {
            request,
            transaction,
        } = transaction.as_mut()
        else {
            unreachable!()
        };
        let DeliveryTransaction::ChannelOutgoing { events, .. } = transaction.as_mut() else {
            unreachable!()
        };
        let rotation = events.remove(0);
        let mut log = PersistentEventLog::open(&f.dir.path().join("events"), "pw").unwrap();
        log.append_durable(rotation).unwrap();
        drop(log);
        assert_eq!(request.message_id, events[0].id);
        store
            .begin(f.tx)
            .expect("existing sender chains need no rotation batch");
    }

    #[test]
    fn typed_send_rejects_missing_duplicate_or_invalid_sender_snapshots() {
        for case in 0..3 {
            let mut f = fixture(AccountGroupOperationKind::Text);
            let mut store = store(&f);
            let DeliveryTransaction::GroupRequested { transaction, .. } = &mut f.tx else {
                unreachable!()
            };
            let DeliveryTransaction::Requested { transaction, .. } = transaction.as_mut() else {
                unreachable!()
            };
            let DeliveryTransaction::ChannelOutgoing { senders, .. } = transaction.as_mut() else {
                unreachable!()
            };
            match case {
                0 => senders.clear(),
                1 => senders.push(senders[0].clone()),
                _ => senders[0].1 = vec![0],
            }
            assert!(store.begin(f.tx).is_err());
            assert!(store.pending_transactions().is_empty());
        }
    }

    #[test]
    fn typed_send_rejects_signed_body_kind_mismatch() {
        let mut f = fixture(AccountGroupOperationKind::Text);
        let mut store = store(&f);
        let DeliveryTransaction::GroupRequested { operation, .. } = &mut f.tx else {
            unreachable!()
        };
        operation.kind = AccountGroupOperationKind::Sticker;
        operation.signature = f.origin.sign(&operation.message()).to_vec();
        assert!(operation.valid(&f.account.public().ed25519_pub));
        assert!(store.begin(f.tx).is_err());
        assert!(store.pending_transactions().is_empty());
    }

    #[test]
    fn typed_send_rejects_changed_body_and_inner_request_id() {
        for change_body in [false, true] {
            let mut f = fixture(AccountGroupOperationKind::Text);
            let mut store = store(&f);
            let DeliveryTransaction::GroupRequested { transaction, .. } = &mut f.tx else {
                unreachable!()
            };
            let DeliveryTransaction::Requested {
                request,
                transaction,
            } = transaction.as_mut()
            else {
                unreachable!()
            };
            if change_body {
                let DeliveryTransaction::ChannelOutgoing { sent, .. } = transaction.as_mut() else {
                    unreachable!()
                };
                sent.plaintext.push(0);
            } else {
                request.request_id = [0xcc; 32];
            }
            assert!(store.begin(f.tx).is_err());
            assert!(store.pending_transactions().is_empty());
        }
    }

    #[test]
    fn typed_send_rejects_invalid_rotation_and_sent_clock() {
        for change_clock in [false, true] {
            let mut f = fixture(AccountGroupOperationKind::Text);
            let mut store = store(&f);
            let DeliveryTransaction::GroupRequested { transaction, .. } = &mut f.tx else {
                unreachable!()
            };
            let DeliveryTransaction::Requested { transaction, .. } = transaction.as_mut() else {
                unreachable!()
            };
            let DeliveryTransaction::ChannelOutgoing { events, sent, .. } = transaction.as_mut()
            else {
                unreachable!()
            };
            if change_clock {
                sent.wall_clock += 1;
            } else {
                let old = &events[0];
                events[0] = Event::new(
                    &f.owner,
                    old.conversation_id,
                    old.seq,
                    old.parents.clone(),
                    old.lamport,
                    old.wall_clock,
                    EventKind::KeyRotation,
                    b"invalid sealed keys".to_vec(),
                );
            }
            assert!(store.begin(f.tx).is_err());
        }
    }
}
