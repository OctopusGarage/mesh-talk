//! Executable evidence for the current identity-key compromise boundaries.
//!
//! These characterize limitations to remove in a versioned protocol upgrade;
//! successful retrospective decryption is not a security requirement to preserve.
//! Attackers receive recorded ciphertext and a later-compromised recipient
//! identity key, never the legitimate ratchet/session stores.

use mesh_talk_core::channel::sender_key::{
    SenderChain, SenderKey, SenderKeyDistribution, open_message, seal_message,
};
use mesh_talk_core::dm;
use mesh_talk_core::identity::device::DeviceIdentity;
use mesh_talk_core::node::{DmRatchet, RatchetSessions};

/// Authenticated TCP carries signed channel records; AEAD failure must not burn
/// either a fresh position or an older skipped key shared by text and manifests.
#[tokio::test]
async fn signed_channel_text_and_manifest_retry_over_noise_loopback() {
    use mesh_talk_core::channel::{ChannelMeta, ChannelState, new_channel_id};
    use mesh_talk_core::eventlog::{Event, EventKind};
    use mesh_talk_core::file::FileManifest;
    use mesh_talk_core::transport::SecureChannel;
    use tokio::net::{TcpListener, TcpStream};

    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let channel = new_channel_id();
    let meta = ChannelMeta {
        name: "crypto boundary".into(),
        members: vec![alice.public(), bob.public()],
        epoch: 7,
        owner: alice.user_id(),
    };
    let mut sender = ChannelState::from_meta(channel, meta.clone());
    sender.set_identity(alice.user_id());
    let mut receiver = ChannelState::from_meta(channel, meta);
    receiver.set_identity(bob.user_id());
    receiver.record_sender_chain(alice.user_id(), 7, &sender.my_sender_distribution());
    let manifest = FileManifest {
        name: "delayed.txt".into(),
        size: 3,
        mime: "text/plain".into(),
        checksum: [7; 32],
        file_key: [9; 32],
        file_conv: new_channel_id(),
        chunk_count: 1,
    };
    let text = sender.seal_sender_message(b"delayed text").unwrap();
    let file = sender.seal_sender_message(&manifest.encode()).unwrap();
    let latest = sender.seal_sender_message(b"latest text").unwrap();
    let mut forged_latest = latest.clone();
    *forged_latest.last_mut().unwrap() ^= 1;
    let mut forged_file = file.clone();
    *forged_file.last_mut().unwrap() ^= 1;
    let frames = [
        (EventKind::Message, forged_latest),
        (EventKind::Message, latest),
        (EventKind::FileManifest, forged_file),
        (EventKind::FileManifest, file),
        (EventKind::Message, text),
    ];
    let expected_author = alice.user_id();
    let peer = bob.public();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let receiving = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut transport = SecureChannel::accept(stream, &bob).await.unwrap();
        assert_eq!(transport.peer_identity().user_id(), expected_author);
        for index in 0..5 {
            let event: Event = bincode::deserialize(&transport.recv().await.unwrap()).unwrap();
            assert!(event.verify_integrity() && event.verify_signature());
            assert_eq!(event.conversation_id, channel);
            assert_eq!(event.author.user_id(), expected_author);
            let plaintext =
                receiver.open_sender_message(&event.author.user_id(), &event.ciphertext);
            match index {
                0 | 2 => assert!(plaintext.is_none()),
                1 => assert_eq!(plaintext.unwrap(), b"latest text"),
                3 => {
                    assert_eq!(event.kind, EventKind::FileManifest);
                    assert_eq!(FileManifest::decode(&plaintext.unwrap()).unwrap(), manifest);
                }
                4 => assert_eq!(plaintext.unwrap(), b"delayed text"),
                _ => unreachable!(),
            }
        }
    });
    let sending = async {
        let stream = TcpStream::connect(address).await.unwrap();
        let mut transport = SecureChannel::connect(stream, &alice, Some(&peer))
            .await
            .unwrap();
        for (index, (kind, wire)) in frames.into_iter().enumerate() {
            let event = Event::new(&alice, channel, index as u64, vec![], 0, 0, kind, wire);
            transport
                .send(&bincode::serialize(&event).unwrap())
                .await
                .unwrap();
        }
        receiving.await.unwrap();
    };
    tokio::time::timeout(std::time::Duration::from_secs(5), sending)
        .await
        .unwrap();
}

fn manager(dir: &std::path::Path, name: &str) -> DmRatchet {
    DmRatchet::new(RatchetSessions::open(&dir.join(name), "test-password").unwrap())
}

fn compromised_x25519(identity: &DeviceIdentity) -> DeviceIdentity {
    // Ed25519 secrets are unnecessary for either attack. Preserve the public
    // peer identity separately wherever the protocol needs an author identity.
    DeviceIdentity::from_secret_bytes([42; 32], identity.secret_bytes().1)
}

#[test]
fn recorded_group_distribution_exposes_old_chain_after_recipient_key_compromise() {
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let stranger = DeviceIdentity::generate();
    let mut sender = SenderKey::generate();
    // This is the exact SKD wrapping used by node::channel::seal_keys_for;
    // that orchestration helper is crate-private.
    let recorded_distribution = dm::seal(
        &alice,
        &bob.public().x25519_pub,
        &sender.distribution().encode(),
    )
    .unwrap();
    let (position, message_key) = sender.ratchet();
    let aad = b"recorded channel epoch/sender/position";
    let recorded_message = seal_message(&message_key, b"past group message", aad).unwrap();
    // Advance/delete the original sending key before the compromise.
    sender.ratchet();
    drop(sender);

    assert!(
        dm::open(
            &stranger,
            &alice.public().x25519_pub,
            &recorded_distribution
        )
        .is_err()
    );
    let stolen_identity = compromised_x25519(&bob);
    let recovered = dm::open(
        &stolen_identity,
        &alice.public().x25519_pub,
        &recorded_distribution,
    )
    .unwrap();
    let distribution = SenderKeyDistribution::decode(&recovered).unwrap();
    let mut reconstructed = SenderChain::from_distribution(&distribution);
    let recovered_key = reconstructed.message_key(position).unwrap();
    assert_eq!(
        open_message(&recovered_key, &recorded_message, aad).unwrap(),
        b"past group message"
    );
}

#[test]
fn recipient_identity_compromise_exposes_initial_dm_chain_but_not_fresh_dh_chain() {
    let dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let bob = DeviceIdentity::generate();
    let mut sending = manager(dir.path(), "alice");
    let mut receiving = manager(dir.path(), "bob");
    let initial = sending
        .encrypt(&alice, &bob.public(), b"initial chain")
        .unwrap();
    let initial_followup = sending
        .encrypt(&alice, &bob.public(), b"same chain")
        .unwrap();
    assert_eq!(
        receiving.decrypt(&bob, &alice.public(), &initial).unwrap(),
        b"initial chain"
    );
    assert_eq!(
        receiving
            .decrypt(&bob, &alice.public(), &initial_followup)
            .unwrap(),
        b"same chain"
    );
    let reply = receiving
        .encrypt(&bob, &alice.public(), b"fresh responder DH")
        .unwrap();
    assert_eq!(
        sending.decrypt(&alice, &bob.public(), &reply).unwrap(),
        b"fresh responder DH"
    );
    let later = sending
        .encrypt(&alice, &bob.public(), b"fresh sending chain")
        .unwrap();
    assert_eq!(
        receiving.decrypt(&bob, &alice.public(), &later).unwrap(),
        b"fresh sending chain"
    );

    let stolen_identity = compromised_x25519(&bob);
    let mut attacker = manager(dir.path(), "attacker-empty-store");
    assert_eq!(
        attacker
            .decrypt(&stolen_identity, &alice.public(), &initial)
            .unwrap(),
        b"initial chain"
    );
    assert_eq!(
        attacker
            .decrypt(&stolen_identity, &alice.public(), &initial_followup)
            .unwrap(),
        b"same chain"
    );
    // The attacker's bootstrap generates its own responder DH key, so it cannot
    // reconstruct the real responder's fresh private key from this transcript.
    assert!(
        attacker
            .decrypt(&stolen_identity, &alice.public(), &later)
            .is_err()
    );
    let mut wrong_key_attacker = manager(dir.path(), "wrong-key-empty-store");
    assert!(
        wrong_key_attacker
            .decrypt(&DeviceIdentity::generate(), &alice.public(), &initial)
            .is_err()
    );
}
