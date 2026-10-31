//! Selected signed file-consent revisions and authenticated direct observations.
//! Record variants preserve the existing encrypted source format; production
//! opens its published current projection on the participant database.
use super::ReceiveDecision;
#[cfg(test)]
use super::file_consent::Decision;
use crate::eventlog::{Event, EventId};
use serde::{Deserialize, Serialize};
mod indexed;
pub(super) use indexed::IndexedFileConsentStore as FileConsentStore;
const MAGIC: &[u8; 6] = b"MTFCT1";
#[derive(Serialize, Deserialize)]
enum Record {
    Published(Event),
    Received(Event),
    Observed {
        manifest: EventId,
        decision: ReceiveDecision,
    },
    ReceivedDirect(Event),
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{eventlog::ConversationId, identity::device::DeviceIdentity};

    #[test]
    fn indexed_file_consent_warm_open_does_not_decode_retained_controls() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("file-consent-controls.log");
        let sender = DeviceIdentity::generate();
        let recipient = DeviceIdentity::generate();
        {
            let mut store = FileConsentStore::open(&path, "pw").unwrap();
            for number in 0u64..512 {
                let mut id = [0; 32];
                id[..8].copy_from_slice(&number.to_be_bytes());
                let payload = Decision {
                    file: ConversationId::new(id),
                    manifest: EventId::new(id),
                    sender: sender.public(),
                    recipient: recipient.public(),
                    sender_account: Some("a".repeat(32)),
                    recipient_account: "b".repeat(32),
                    decision: ReceiveDecision::Accepted,
                    completion: None,
                };
                store
                    .receive_control(payload.event(&recipient, 1).unwrap())
                    .unwrap();
                if number == 0 {
                    store
                        .observe_remote_decision(payload.manifest, ReceiveDecision::Declined)
                        .unwrap();
                }
            }
        }
        crate::storage::record_log::frame_observations::reset();
        let store = FileConsentStore::open(&path, "pw").unwrap();
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0,
            "warm signed consent must read selected current facts without replaying retained control bodies"
        );
        assert_eq!(
            store.remote_decision(EventId::new([0; 32])).unwrap(),
            Some(ReceiveDecision::Declined)
        );
    }

    #[test]
    fn recipients_of_one_manifest_keep_independent_revisions_and_completion() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("consent");
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let mut bob_payload = Decision {
            file: ConversationId::new([1; 32]),
            manifest: EventId::new([2; 32]),
            sender: DeviceIdentity::generate().public(),
            recipient: bob.public(),
            sender_account: Some("a".repeat(32)),
            recipient_account: "b".repeat(32),
            decision: ReceiveDecision::Accepted,
            completion: None,
        };
        let mut carol_payload = bob_payload.clone();
        carol_payload.recipient = carol.public();
        carol_payload.recipient_account = "c".repeat(32);
        carol_payload.decision = ReceiveDecision::Declined;
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        assert!(
            store
                .receive_control(bob_payload.event(&bob, 2).unwrap())
                .unwrap()
        );
        assert!(
            store
                .receive_control(carol_payload.event(&carol, 1).unwrap())
                .unwrap(),
            "another recipient's higher revision must not discard this recipient's refusal"
        );
        bob_payload.completion = Some(EventId::new([3; 32]));
        assert!(
            store
                .receive_control(bob_payload.event(&bob, 3).unwrap())
                .unwrap()
        );
        assert!(
            store
                .remote_decision(bob_payload.manifest)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .received_control(bob_payload.manifest)
                .unwrap()
                .is_none()
        );
        assert_eq!(store.received_controls().count(), 2);
        assert!(
            Decision::open(
                &store
                    .received_control_for(bob_payload.manifest, &bob.public())
                    .unwrap()
                    .unwrap()
            )
            .as_ref()
                == Some(&bob_payload)
        );
        assert!(
            Decision::open(
                &store
                    .received_control_for(carol_payload.manifest, &carol.public())
                    .unwrap()
                    .unwrap()
            )
            .as_ref()
                == Some(&carol_payload)
        );
        assert!(
            store
                .received_control_for(bob_payload.manifest, &DeviceIdentity::generate().public())
                .unwrap()
                .is_none()
        );

        // Current SQL facts need no replay compaction.
        drop(store);
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        assert_eq!(store.received_controls().count(), 2);
        assert!(
            store
                .received_control(bob_payload.manifest)
                .unwrap()
                .is_none()
        );
        assert!(
            store
                .received_controls()
                .any(|event| Decision::open(&event).as_ref() == Some(&bob_payload))
        );
        assert!(
            store
                .received_controls()
                .any(|event| Decision::open(&event).as_ref() == Some(&carol_payload))
        );
        assert!(
            !store
                .receive_control(carol_payload.event(&carol, 1).unwrap())
                .unwrap()
        );
        carol_payload.decision = ReceiveDecision::Accepted;
        assert!(
            store
                .receive_control(carol_payload.event(&carol, 2).unwrap())
                .unwrap()
        );
        assert_eq!(store.received_controls().count(), 2);
        assert!(
            store
                .received_controls()
                .any(|event| Decision::open(&event).as_ref() == Some(&bob_payload))
        );
        assert!(
            store
                .remote_decision(bob_payload.manifest)
                .unwrap()
                .is_none()
        );
        // Current SQL facts need no replay compaction.
        drop(store);
        let store = FileConsentStore::open(&path, "pw").unwrap();
        assert!(
            Decision::open(
                &store
                    .received_control_for(bob_payload.manifest, &bob.public())
                    .unwrap()
                    .unwrap()
            )
            .as_ref()
                == Some(&bob_payload)
        );
        assert!(
            Decision::open(
                &store
                    .received_control_for(carol_payload.manifest, &carol.public())
                    .unwrap()
                    .unwrap()
            )
            .as_ref()
                == Some(&carol_payload)
        );
    }

    #[test]
    fn direct_recipient_revision_does_not_override_another_recipient() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("consent");
        let bob = DeviceIdentity::generate();
        let carol = DeviceIdentity::generate();
        let bob_payload = Decision {
            file: ConversationId::new([11; 32]),
            manifest: EventId::new([12; 32]),
            sender: DeviceIdentity::generate().public(),
            recipient: bob.public(),
            sender_account: Some("a".repeat(32)),
            recipient_account: "b".repeat(32),
            decision: ReceiveDecision::Declined,
            completion: None,
        };
        let mut carol_payload = bob_payload.clone();
        carol_payload.recipient = carol.public();
        carol_payload.recipient_account = "c".repeat(32);
        carol_payload.decision = ReceiveDecision::Accepted;
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        assert!(
            store
                .receive_direct_control(bob_payload.event(&bob, 9).unwrap())
                .unwrap()
        );
        assert!(
            store
                .receive_control(carol_payload.event(&carol, 1).unwrap())
                .unwrap()
        );
        assert_eq!(store.received_controls().count(), 2);
        assert!(
            store
                .received_controls()
                .any(|event| Decision::open(&event).as_ref() == Some(&bob_payload))
        );
        assert!(
            store
                .received_controls()
                .any(|event| Decision::open(&event).as_ref() == Some(&carol_payload))
        );
    }

    #[test]
    fn direct_decline_survives_compaction_and_replayed_mailbox_acceptance() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("consent");
        let recipient = DeviceIdentity::generate();
        let payload = Decision {
            file: ConversationId::new([1; 32]),
            manifest: EventId::new([2; 32]),
            sender: DeviceIdentity::generate().public(),
            recipient: recipient.public(),
            sender_account: Some("a".repeat(32)),
            recipient_account: "b".repeat(32),
            decision: ReceiveDecision::Accepted,
            completion: None,
        };
        let accepted = payload.event(&recipient, 1).unwrap();
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        store
            .observe_remote_decision(payload.manifest, ReceiveDecision::Declined)
            .unwrap();
        // Current SQL facts need no replay compaction.
        drop(store);
        let mut store = FileConsentStore::open(&path, "pw").unwrap();
        store.receive_control(accepted).unwrap();
        assert_eq!(
            store.remote_decision(payload.manifest).unwrap(),
            Some(ReceiveDecision::Declined)
        );
        store
            .receive_control(payload.event(&recipient, 2).unwrap())
            .unwrap();
        assert_eq!(
            store.remote_decision(payload.manifest).unwrap(),
            Some(ReceiveDecision::Declined),
            "unversioned direct refusal must not be undone by delayed mailbox approvals"
        );
        store
            .observe_remote_decision(payload.manifest, ReceiveDecision::Accepted)
            .unwrap();
        assert_eq!(
            store.remote_decision(payload.manifest).unwrap(),
            Some(ReceiveDecision::Accepted)
        );
    }
}
