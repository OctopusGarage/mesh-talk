use super::*;
use crate::file::FileKind;
use crate::identity::{account::Account, device::DeviceIdentity};
use base64::Engine;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

pub(super) fn open(dir: &Path, device: DeviceIdentity, account: Account) -> Arc<Node> {
    let (dm, _) = mpsc::unbounded_channel();
    let (ch, _) = mpsc::unbounded_channel();
    let (files, _) = mpsc::unbounded_channel();
    Node::open_with_account(
        device,
        account,
        Arc::new(Mutex::new(crate::discovery::Roster::default())),
        dm,
        ch,
        files,
        &dir.join("events.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap()
}

fn sticker(dir: &Path, color: u8) -> (std::path::PathBuf, Vec<u8>) {
    let name = format!(
        "mt-sticker-v1-{}.png",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode("pack:example:wave\n👋")
    );
    let bytes = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap();
    let path = dir.join(name);
    let mut bytes = bytes;
    // Trailing bytes preserve a decodable PNG but change exact content identity.
    bytes.push(color);
    std::fs::write(&path, &bytes).unwrap();
    (path, bytes)
}

#[tokio::test]
async fn repeated_sticker_reuses_bytes_and_history_survives_source_removal_and_restart() {
    let dir = tempfile::tempdir().unwrap();
    let device = DeviceIdentity::generate();
    let secret = device.secret_bytes();
    let account = Account::generate();
    let account_secret = account.secret_bytes();
    let target = account.account_id();
    let node = open(dir.path(), device, account);
    let (path, bytes) = sticker(dir.path(), 0);
    let (first, file) = node
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    let (second, repeated) = node
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(
        file, repeated,
        "repeated stickers reuse the verified file conversation"
    );
    assert_eq!(node.account_history(&target, 10).len(), 2);
    assert!(
        node.reusable_sticker(
            crate::eventlog::ConversationId::new([9; 32]),
            None,
            path.file_name().unwrap().to_str().unwrap(),
            &bytes,
            None
        )
        .is_none(),
        "a different host cannot reuse this file key"
    );
    assert_eq!(node.read_media(file).unwrap(), bytes);
    std::fs::remove_file(&path).unwrap();
    node.delete_message(
        super::conversation::account_conversation_id(&node.account.account_id(), &target),
        first,
        true,
    )
    .unwrap();
    assert_eq!(
        node.read_media(file).unwrap(),
        bytes,
        "deleting one reference retains the other"
    );
    drop(node);
    let reopened = open(
        dir.path(),
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Account::from_secret_bytes(account_secret),
    );
    assert_eq!(reopened.read_media(file).unwrap(), bytes);
    assert_eq!(reopened.account_history(&target, 10).len(), 1);
}

#[tokio::test]
async fn repeated_sticker_restages_after_transient_chunk_reclamation() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let target = account.account_id();
    let node = open(dir.path(), DeviceIdentity::generate(), account);
    let (path, bytes) = sticker(dir.path(), 0);
    let (first, file) = node
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    // Completion can reclaim this transient DAG while durable preview survives.
    node.log.lock().unwrap().drop_conversation(&file).unwrap();
    assert!(node.read_file_internal(file).is_err());
    assert_eq!(node.read_media(file).as_deref(), Some(bytes.as_slice()));
    let (second, restaged) = node
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    assert_ne!(first, second);
    assert_ne!(
        file, restaged,
        "reclaimed signed chunks require fresh staging"
    );
    assert_eq!(node.read_file_internal(restaged).unwrap(), bytes);
    assert_eq!(node.read_media(file).unwrap(), bytes);
    let rows = node.account_history(&target, 10);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows.iter().filter(|row| row.id == first).count(), 1);
    assert_eq!(rows.iter().filter(|row| row.id == second).count(), 1);
}

#[tokio::test]
async fn sticker_cache_does_not_alias_replaced_images_or_different_deadlines() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let target = account.account_id();
    let node = open(dir.path(), DeviceIdentity::generate(), account);
    let (path, first_bytes) = sticker(dir.path(), 1);
    let (_, first) = node
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    let (_, second_bytes) = sticker(dir.path(), 2);
    let (_, second) = node
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    assert_ne!(first, second);
    assert_eq!(node.read_media(first).unwrap(), first_bytes);
    assert_eq!(node.read_media(second).unwrap(), second_bytes);
    let deadline = super::node::now_millis() + 60_000;
    let (_, expiring) = node
        .enqueue_file_to_account_progress_expiring_if(
            &target,
            &path,
            FileKind::Media,
            |_| {},
            Some(deadline),
            |accept| accept(),
        )
        .await
        .unwrap();
    assert_ne!(second, expiring, "retention scopes never share a file key");
}

#[tokio::test]
async fn malformed_sticker_image_is_rejected_before_staging() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let target = account.account_id();
    let node = open(dir.path(), DeviceIdentity::generate(), account);
    let (path, _) = sticker(dir.path(), 0);
    std::fs::write(&path, b"not an image").unwrap();
    assert!(
        node.enqueue_file_to_account(&target, &path, FileKind::Media)
            .await
            .is_err()
    );
    assert!(node.account_history(&target, 10).is_empty());
}

#[tokio::test]
async fn sticker_recall_account_keeps_other_reference_and_excludes_recalled_cache_sources() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), DeviceIdentity::generate(), Account::generate());
    let target = node.account_id();
    let (path, bytes) = sticker(dir.path(), 0);
    let (first, shared) = node
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    let (second, repeated) = node
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    assert_eq!(shared, repeated);
    node.recall_account(&target, first).await.unwrap();
    let history = node.account_history(&target, 10);
    assert!(history.iter().find(|row| row.id == first).unwrap().recalled);
    assert!(
        history
            .iter()
            .find(|row| row.id == second)
            .unwrap()
            .file
            .is_some()
    );
    assert_eq!(node.read_media(shared).unwrap(), bytes);
    node.recall_account(&target, second).await.unwrap();
    let (_, fresh) = node
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    assert_ne!(
        shared, fresh,
        "recalled account messages are not cache sources"
    );
}

#[tokio::test]
async fn sticker_recall_channel_preserves_other_message_references() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), DeviceIdentity::generate(), Account::generate());
    let channel = node.create_channel("recall", vec![]).await.unwrap();
    let (path, bytes) = sticker(dir.path(), 0);
    let (first, file) = node
        .enqueue_file_channel_progress_expiring_if(
            channel,
            &path,
            FileKind::Media,
            |_| {},
            None,
            |accept| accept(),
        )
        .await
        .unwrap();
    let (second, shared) = node
        .enqueue_file_channel_progress_expiring_if(
            channel,
            &path,
            FileKind::Media,
            |_| {},
            None,
            |accept| accept(),
        )
        .await
        .unwrap();
    assert_eq!(file, shared);
    node.recall_channel(channel, first).await.unwrap();
    let history = node.channel_history(channel, 10);
    assert!(history.iter().find(|row| row.id == first).unwrap().recalled);
    assert!(
        history
            .iter()
            .find(|row| row.id == second)
            .unwrap()
            .file
            .is_some()
    );
    assert_eq!(node.read_media(shared).unwrap(), bytes);
}

#[tokio::test]
async fn channel_sticker_reuses_only_its_own_group_transfer() {
    let dir = tempfile::tempdir().unwrap();
    let node = open(dir.path(), DeviceIdentity::generate(), Account::generate());
    let first_group = node.create_channel("first", vec![]).await.unwrap();
    let second_group = node.create_channel("second", vec![]).await.unwrap();
    let (path, bytes) = sticker(dir.path(), 0);
    let (_, first) = node
        .enqueue_file_channel_progress_expiring_if(
            first_group,
            &path,
            FileKind::Media,
            |_| {},
            None,
            |accept| accept(),
        )
        .await
        .unwrap();
    let (_, repeated) = node
        .enqueue_file_channel_progress_expiring_if(
            first_group,
            &path,
            FileKind::Media,
            |_| {},
            None,
            |accept| accept(),
        )
        .await
        .unwrap();
    let (_, different_group) = node
        .enqueue_file_channel_progress_expiring_if(
            second_group,
            &path,
            FileKind::Media,
            |_| {},
            None,
            |accept| accept(),
        )
        .await
        .unwrap();
    assert_eq!(first, repeated);
    assert_ne!(first, different_group);
    assert_eq!(node.read_media(first).unwrap(), bytes);
    assert_eq!(node.channel_history(first_group, 10).len(), 2);
    node.clear_conversation(first_group).unwrap();
    assert!(node.read_media(first).is_none());
    assert_eq!(node.read_media(different_group).unwrap(), bytes);
}

#[tokio::test]
async fn v2_sticker_media_delivers_label_after_source_removal_without_receiver_installation() {
    use crate::discovery::Announce;
    use tokio::net::TcpListener;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let bob_secret = bob.secret_bytes();
    let bob_account_secret = bob_account.secret_bytes();
    let target = bob_account.account_id();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 4000);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp = Announce::new_with_account(
        &bob,
        &bob_account,
        "Bob",
        listener.local_addr().unwrap().port(),
    );
    let (a, _, _) = super::test_support::node_with_files(ad.path(), alice, aa, &bp);
    let (b, _, mut received) =
        super::test_support::node_with_files(bd.path(), bob, bob_account, &ap);
    let (legacy_path, bytes) = sticker(ad.path(), 0);
    let name = "mt-sticker-v2-cGFjazpjYXRzOmNhbG0K8J-ZggrpnZnkuIvlv4M.png";
    let path = ad.path().join(name);
    std::fs::rename(legacy_path, &path).unwrap();
    // Accept while the receiver is not serving. No source path is needed afterward.
    let (id, file) = a
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    let (second_id, reused) = a
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    assert_eq!(
        file, reused,
        "two accepted messages share one verified transfer"
    );
    std::fs::remove_file(&path).unwrap();
    let serving = tokio::spawn(b.clone().run_accept_loop(listener));
    a.flush_delivery(id).await;
    a.flush_file_delivery(id).await;
    a.flush_delivery(second_id).await;
    a.flush_file_delivery(second_id).await;
    let offer = tokio::time::timeout(std::time::Duration::from_secs(5), received.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(offer.media);
    assert_eq!(offer.name, name);
    assert_eq!(offer.file_conv, file);
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            if b.read_media(file).as_deref() == Some(bytes.as_slice()) {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("automatic sticker bytes become durable without installing a pack");
    // Existing peers can decode this as ordinary MFM3 media, without new enum tags.
    assert!(matches!(
        b.files.lock().unwrap().manifest(&file),
        Some(crate::file::AnyManifest::V3(_))
    ));
    assert!(
        b.recall_account(&a.account_id(), id).await.is_err(),
        "a recipient cannot recall the sender's sticker"
    );
    a.recall_account(&target, id).await.unwrap();
    assert!(
        b.account_history(&a.account_id(), 10)
            .iter()
            .any(|row| row.id == id && row.recalled && row.file.is_none()),
        "the authenticated sender recall hides sticker media on the recipient"
    );
    assert!(
        b.account_history(&a.account_id(), 10)
            .iter()
            .any(|row| row.id == second_id
                && !row.recalled
                && row
                    .file
                    .as_ref()
                    .is_some_and(|media| media.file_conv == file)),
        "recalling the first message preserves the recipient's second reference"
    );
    serving.abort();
    let _ = serving.await;
    drop(b);
    let reopened = open(
        bd.path(),
        DeviceIdentity::from_secret_bytes(bob_secret.0, bob_secret.1),
        Account::from_secret_bytes(bob_account_secret),
    );
    assert_eq!(reopened.read_media(file).unwrap(), bytes);
    let rows = reopened.account_history(&a.account_id(), 10);
    let media = rows
        .iter()
        .find(|row| row.id == second_id)
        .unwrap()
        .file
        .as_ref()
        .unwrap();
    assert_eq!(media.name, name);
    let metadata = crate::file::sticker::from_name(&media.name).unwrap();
    assert_eq!(metadata.label.as_deref(), Some("静下心"));
}

#[tokio::test]
async fn sticker_alias_after_chunk_reclamation_gets_exact_completion_control() {
    use crate::discovery::Announce;
    use tokio::net::TcpListener;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let target = bob_account.account_id();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 4000);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp = Announce::new_with_account(
        &bob,
        &bob_account,
        "Bob",
        listener.local_addr().unwrap().port(),
    );
    let (a, _, _) = super::test_support::node_with_files(ad.path(), alice, aa, &bp);
    let (b, _, _) = super::test_support::node_with_files(bd.path(), bob, bob_account, &ap);
    let (path, bytes) = sticker(ad.path(), 0);
    let (first, file) = a
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    let manifest = match a.files.lock().unwrap().manifest(&file).unwrap().clone() {
        crate::file::AnyManifest::V3(manifest) => manifest,
        _ => unreachable!(),
    };
    let serving = tokio::spawn(b.clone().run_accept_loop(listener));
    a.flush_delivery(first).await;
    a.flush_file_delivery(first).await;
    assert_eq!(b.read_media(file).as_deref(), Some(bytes.as_slice()));
    assert!(
        b.log.lock().unwrap().events(&file).is_empty(),
        "first completion reclaims receiver chunks"
    );
    let prior = b.delivery.lock().unwrap().cards_for_file(file);
    assert_eq!(prior.len(), 1);
    assert!(b.delivery.lock().unwrap().file_completed(prior[0].id));
    let peer = a.routing_peer(&bp.user_id).unwrap();
    let second = a
        .accept_staged_manifest(
            &[peer],
            super::conversation::account_conversation_id(&a.account_id(), &target),
            Some(target),
            &manifest,
        )
        .unwrap();
    a.flush_delivery(second).await;
    let cards = b.delivery.lock().unwrap().cards_for_file(file);
    assert_eq!(cards.len(), 2);
    let alias = cards.iter().find(|card| card.id != prior[0].id).unwrap();
    let controls = b.file_consent_events(&ap.public()).unwrap();
    assert!(
        controls
            .iter()
            .filter_map(super::file_consent::Decision::open)
            .any(|decision| { decision.manifest == alias.id && decision.completion.is_some() }),
        "a new manifest must publish its own signed completion after durable-cache reuse"
    );
    assert!(b.delivery.lock().unwrap().file_completed(alias.id));
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
async fn sticker_alias_completion_requires_verified_bytes_and_retained_matching_ancestry() {
    use crate::discovery::Announce;
    use tokio::net::TcpListener;
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_account = Account::generate();
    let target = bob_account.account_id();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", 4000);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bp = Announce::new_with_account(
        &bob,
        &bob_account,
        "Bob",
        listener.local_addr().unwrap().port(),
    );
    let (a, _, _) = super::test_support::node_with_files(ad.path(), alice, aa, &bp);
    let (b, _, _) = super::test_support::node_with_files(bd.path(), bob, bob_account, &ap);
    let (path, bytes) = sticker(ad.path(), 0);
    let (first, file) = a
        .enqueue_file_to_account(&target, &path, FileKind::Media)
        .await
        .unwrap();
    let manifest = match a.files.lock().unwrap().manifest(&file).unwrap().clone() {
        crate::file::AnyManifest::V3(manifest) => manifest,
        _ => unreachable!(),
    };
    let serving = tokio::spawn(b.clone().run_accept_loop(listener));
    a.flush_delivery(first).await;
    a.flush_file_delivery(first).await;
    assert_eq!(b.read_media(file).as_deref(), Some(bytes.as_slice()));
    assert!(
        b.log.lock().unwrap().events(&file).is_empty(),
        "first completion reclaims receiver chunks"
    );
    let prior = b.delivery.lock().unwrap().cards_for_file(file);
    assert_eq!(prior.len(), 1);
    assert!(b.delivery.lock().unwrap().file_completed(prior[0].id));
    let peer = a.routing_peer(&bp.user_id).unwrap();
    let second = a
        .accept_staged_manifest(
            &[peer],
            super::conversation::account_conversation_id(&a.account_id(), &target),
            Some(target),
            &manifest,
        )
        .unwrap();
    b.media
        .store_bytes(file, manifest.v2.name.as_str(), b"corrupted cache")
        .unwrap();
    a.flush_delivery(second).await;
    let cards = b.delivery.lock().unwrap().cards_for_file(file);
    assert_eq!(cards.len(), 2);
    let alias = cards.iter().find(|card| card.id != prior[0].id).unwrap();
    let name = manifest.v2.name.as_str();
    assert!(
        !b.complete_sticker_aliases(file),
        "corrupted durable bytes cannot complete an alias"
    );
    assert!(!b.delivery.lock().unwrap().file_completed(alias.id));
    b.media.remove(file).unwrap();
    assert!(
        !b.complete_sticker_aliases(file),
        "a cache index without bytes is insufficient"
    );
    b.media.store_bytes(file, name, &bytes).unwrap();
    let original_row = b
        .received_files
        .lock()
        .unwrap()
        .entry(prior[0].id)
        .unwrap()
        .clone();
    b.received_files
        .lock()
        .unwrap()
        .remove_where(|row| row.event_id == prior[0].id)
        .unwrap();
    assert!(
        !b.complete_sticker_aliases(file),
        "the completed original manifest must remain retained"
    );
    for mismatch in 0..4 {
        let mut row = original_row.clone();
        match mismatch {
            0 => row.conversation = crate::eventlog::ConversationId::new([91; 32]),
            1 => row.from = "unrelated-source".to_owned(),
            2 => {
                row.plaintext =
                    crate::expiry::wrap(&row.plaintext, Some(super::node::now_millis() + 60_000))
            }
            _ => {
                let mut incompatible = manifest.clone();
                incompatible.v2.checksum[0] ^= 1;
                row.plaintext = incompatible.encode();
            }
        }
        b.received_files
            .lock()
            .unwrap()
            .record_durable(&row)
            .unwrap();
        assert!(
            !b.complete_sticker_aliases(file),
            "mismatched retained row {mismatch} cannot provide ancestry"
        );
        assert!(!b.delivery.lock().unwrap().file_completed(alias.id));
        b.received_files
            .lock()
            .unwrap()
            .remove_where(|row| row.event_id == prior[0].id)
            .unwrap();
    }
    b.received_files
        .lock()
        .unwrap()
        .record_durable(&original_row)
        .unwrap();
    b.delivery
        .lock()
        .unwrap()
        .erase_file(prior[0].conversation, prior[0].id)
        .unwrap();
    assert!(
        !b.complete_sticker_aliases(file),
        "erased completion is not reusable ancestry"
    );
    assert!(!b.delivery.lock().unwrap().file_completed(alias.id));
    serving.abort();
    let _ = serving.await;
}

#[tokio::test]
async fn installed_sticker_headers_complete_exact_offer_without_image_transfer() {
    use crate::discovery::Announce;
    use crate::eventlog::sync::SyncStore;
    use tokio::net::TcpListener;
    for chunk_count in [1, 3] {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_account = Account::generate();
        let target = bob_account.account_id();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", 4000);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bp = Announce::new_with_account(
            &bob,
            &bob_account,
            "Bob",
            listener.local_addr().unwrap().port(),
        );
        let (a, _, _) = super::test_support::node_with_files(ad.path(), alice, aa, &bp);
        let (b, _, _) = super::test_support::node_with_files(bd.path(), bob, bob_account, &ap);
        let (path, mut bytes) = sticker(ad.path(), 0);
        if chunk_count > 1 {
            // Valid PNG trailing bytes exercise original-author parent chains.
            bytes.resize(crate::file::CHUNK_SIZE * (chunk_count - 1) + 1, 0);
            std::fs::write(&path, &bytes).unwrap();
        }
        let mut replaced = bytes.clone();
        replaced.push(19);
        b.register_sticker_source(path.file_name().unwrap().to_str().unwrap(), &replaced)
            .unwrap();
        let (id, file) = a
            .enqueue_file_to_account(&target, &path, FileKind::Media)
            .await
            .unwrap();
        let serving = tokio::spawn(b.clone().run_accept_loop(listener));
        a.flush_delivery(id).await;
        let card = a.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let destination = card.destinations.first().unwrap();
        let manifest = destination.binding.event_id;
        let proofs = a.sticker_chunk_proof(file).unwrap();
        assert_eq!(proofs.len(), chunk_count);
        assert!(proofs.iter().all(|event| event.ciphertext.is_empty()));
        assert!(b.read_media(file).is_none());
        let unrelated = DeviceIdentity::generate().public();
        assert!(
            b.sync_store(&unrelated)
                .lock()
                .unwrap()
                .sticker_file_consent(&file, &manifest, proofs.clone())
                .is_none()
        );
        assert!(
            b.sync_store(&ap.public())
                .lock()
                .unwrap()
                .sticker_file_consent(
                    &file,
                    &crate::eventlog::EventId::new([237; 32]),
                    proofs.clone()
                )
                .is_none()
        );
        let control = b
            .sync_store(&ap.public())
            .lock()
            .unwrap()
            .sticker_file_consent(&file, &manifest, proofs.clone())
            .unwrap();
        assert!(
            super::file_consent::Decision::open(&control)
                .unwrap()
                .completion
                .is_none()
        );
        assert!(b.read_media(file).is_none());
        b.register_sticker_source(path.file_name().unwrap().to_str().unwrap(), &bytes)
            .unwrap();
        let mut tampered = proofs.clone();
        tampered.last_mut().unwrap().sig[0] ^= 1;
        let control = b
            .sync_store(&ap.public())
            .lock()
            .unwrap()
            .sticker_file_consent(&file, &manifest, tampered)
            .unwrap();
        assert!(
            super::file_consent::Decision::open(&control)
                .unwrap()
                .completion
                .is_none()
        );
        assert!(b.read_media(file).is_none());
        assert!(b.log.lock().unwrap().events(&file).is_empty());
        let control = b
            .sync_store(&ap.public())
            .lock()
            .unwrap()
            .sticker_file_consent(&file, &manifest, proofs)
            .unwrap();
        assert_eq!(b.read_media(file).as_deref(), Some(bytes.as_slice()));
        a.accept_file_consent_event(&control, &bp.public()).unwrap();
        assert!(a.recipient_completed_file(&card, destination));
        assert!(b.log.lock().unwrap().events(&file).is_empty());
        serving.abort();
        let _ = serving.await;
    }
}

#[tokio::test]
async fn installed_sticker_wire_reuse_and_legacy_mismatch_fallback() {
    use crate::discovery::Announce;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    // Matching installed bytes skip all ciphertext. Replaced artwork and older
    // endpoints retain the ordinary transfer path.
    for (matching, legacy) in [(true, false), (false, false), (true, true)] {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_account = Account::generate();
        let target = bob_account.account_id();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", 4000);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bp = Announce::new_with_account(
            &bob,
            &bob_account,
            "Bob",
            listener.local_addr().unwrap().port(),
        );
        let (a, _, _) = super::test_support::node_with_files(ad.path(), alice, aa, &bp);
        let (b, _, _) = super::test_support::node_with_files(bd.path(), bob, bob_account, &ap);
        let (path, bytes) = sticker(ad.path(), 0);
        let mut installed = bytes.clone();
        if !matching {
            installed.push(23);
        }
        b.register_sticker_source(path.file_name().unwrap().to_str().unwrap(), &installed)
            .unwrap();
        let (first, file) = a
            .enqueue_file_to_account(&target, &path, FileKind::Media)
            .await
            .unwrap();
        let payload_bytes = Arc::new(AtomicUsize::new(0));
        let proofs_seen = Arc::new(AtomicUsize::new(0));
        let serving = sticker_wire_server(
            b.clone(),
            listener,
            file,
            legacy,
            payload_bytes.clone(),
            proofs_seen.clone(),
        );
        a.flush_delivery(first).await;
        // A closed optional-protocol socket must release its directional permit
        // before legacy fallback can acquire the sole remaining outbound slot.
        let reserved = if legacy {
            (0..super::session_pool::MAX_DIRECTION_CONNECTIONS - 1)
                .map(|_| {
                    a.sessions
                        .try_outbound()
                        .expect("reserve other outbound slots")
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), async {
            a.flush_file_delivery(first).await;
        })
        .await
        .unwrap();
        drop(reserved);
        assert_eq!(b.read_media(file).as_deref(), Some(bytes.as_slice()));
        assert!(proofs_seen.load(Ordering::Relaxed) > 0);
        if matching && !legacy {
            assert_eq!(
                payload_bytes.load(Ordering::Relaxed),
                0,
                "matching artwork sends no image ciphertext"
            );
            let card = a.delivery.lock().unwrap().file_card(first).unwrap().clone();
            assert!(a.recipient_completed_file(&card, &card.destinations[0]));
            let (second, reused) = a
                .enqueue_file_to_account(&target, &path, FileKind::Media)
                .await
                .unwrap();
            assert_ne!(first, second);
            assert_eq!(file, reused);
            a.flush_delivery(second).await;
            a.flush_file_delivery(second).await;
            let cards = b.delivery.lock().unwrap().cards_for_file(file);
            assert_eq!(cards.len(), 2);
            assert!(
                cards
                    .iter()
                    .all(|card| b.delivery.lock().unwrap().file_completed(card.id))
            );
            assert_eq!(payload_bytes.load(Ordering::Relaxed), 0);
        } else {
            assert!(payload_bytes.load(Ordering::Relaxed) >= bytes.len());
        }
        serving.abort();
        let _ = serving.await;
    }
}

fn sticker_wire_server(
    server_node: Arc<Node>,
    listener: tokio::net::TcpListener,
    file: crate::eventlog::ConversationId,
    legacy: bool,
    payload_counter: Arc<std::sync::atomic::AtomicUsize>,
    proof_counter: Arc<std::sync::atomic::AtomicUsize>,
) -> tokio::task::JoinHandle<()> {
    use std::sync::atomic::Ordering;
    tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (stream, _) = listener.accept().await.unwrap();
            let node = server_node.clone();
            let payload_counter = payload_counter.clone();
            let proof_counter = proof_counter.clone();
            connections.spawn(async move {
                let Ok(mut channel) = node.privacy_accept(stream).await else {
                    return;
                };
                let store = node.serve_sync_store(channel.peer_identity());
                while let Ok(frame) = channel.recv().await {
                    let (proof, payload) = super::session::sticker_test_frame(&frame, file);
                    payload_counter.fetch_add(payload, Ordering::Relaxed);
                    // A recovery file-round request asks the source for image bytes.
                    // Count it as traffic as well, so zero excludes both directions.
                    if super::session::round_request_conversation(&frame) == Some(file) {
                        payload_counter.fetch_add(1, Ordering::Relaxed);
                    }
                    if proof {
                        proof_counter.fetch_add(1, Ordering::Relaxed);
                        if legacy {
                            return;
                        }
                    }
                    match super::session::serve_wire_bytes(&mut channel, &store, &frame).await {
                        Ok(super::session::Served::Handled(conversation)) => {
                            node.emit_new_messages(conversation);
                            node.process_file_events(conversation);
                        }
                        _ => return,
                    }
                }
            });
        }
    })
}

#[tokio::test]
async fn installed_sticker_receiver_recovery_requests_only_authorized_headers() {
    use crate::discovery::Announce;
    use crate::eventlog::sync::SyncStore;
    use futures_util::FutureExt;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::net::TcpListener;
    for (matching, legacy) in [(true, false), (false, false), (true, true)] {
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob = DeviceIdentity::generate();
        let bob_account = Account::generate();
        let target = bob_account.account_id();
        let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
        let bp =
            Announce::new_with_account(&bob, &bob_account, "Bob", bl.local_addr().unwrap().port());
        let (a, _, _) = super::test_support::node_with_files(ad.path(), alice, aa, &bp);
        let (b, _, _) = super::test_support::node_with_files(bd.path(), bob, bob_account, &ap);
        let (path, bytes) = sticker(ad.path(), 0);
        let mut installed = bytes.clone();
        if !matching {
            installed.push(23);
        }
        let (id, file) = a
            .enqueue_file_to_account(&target, &path, FileKind::Media)
            .await
            .unwrap();
        let payloads = Arc::new(AtomicUsize::new(0));
        let proofs = Arc::new(AtomicUsize::new(0));
        let sender = sticker_wire_server(
            a.clone(),
            al,
            file,
            legacy,
            payloads.clone(),
            proofs.clone(),
        );
        let receiver = tokio::spawn(b.clone().run_accept_loop(bl));
        a.flush_delivery(id).await;
        assert!(b.read_media(file).is_none());
        // Registration may follow manifest delivery during startup.
        let _ = b.file_pull_notify.notified().now_or_never();
        b.register_sticker_source(path.file_name().unwrap().to_str().unwrap(), &installed)
            .unwrap();
        assert!(b.file_pull_notify.notified().now_or_never().is_some());
        let card = a.delivery.lock().unwrap().file_card(id).unwrap().clone();
        let manifest = card.destinations[0].binding.event_id;
        assert!(
            a.serve_sync_store(&DeviceIdentity::generate().public())
                .lock()
                .unwrap()
                .sticker_file_proof(&file, &manifest)
                .is_none()
        );
        assert!(
            a.serve_sync_store(&bp.public())
                .lock()
                .unwrap()
                .sticker_file_proof(&file, &crate::eventlog::EventId::new([226; 32]))
                .is_none()
        );
        tokio::time::timeout(std::time::Duration::from_secs(10), b.pull_pending_files())
            .await
            .unwrap();
        assert_eq!(b.read_media(file).as_deref(), Some(bytes.as_slice()));
        if matching && !legacy {
            assert!(proofs.load(Ordering::Relaxed) > 0);
            assert_eq!(payloads.load(Ordering::Relaxed), 0);
            assert!(
                a.serve_sync_store(&bp.public())
                    .lock()
                    .unwrap()
                    .sticker_file_proof(&file, &manifest)
                    .unwrap()
                    .iter()
                    .all(|event| event.ciphertext.is_empty())
            );
        } else {
            assert!(
                payloads.load(Ordering::Relaxed) > 0,
                "fallback requests ordinary file bytes"
            );
        }
        // Recovery publishes the same exact signed completion consumed by senders.
        let control = b
            .direct_file_consent_control(&ap.public(), file, manifest)
            .unwrap()
            .unwrap();
        a.accept_file_consent_event(&control, &bp.public()).unwrap();
        assert!(a.recipient_completed_file(&card, &card.destinations[0]));
        sender.abort();
        receiver.abort();
        let _ = sender.await;
        let _ = receiver.await;
    }
}

#[tokio::test]
async fn installed_sticker_proof_rejects_untrusted_headers_before_chunk_or_media_admission() {
    use crate::discovery::Announce;
    use crate::eventlog::{Author, ConversationId, EventId, EventKind};
    use std::sync::atomic::Ordering;
    let source_dir = tempfile::tempdir().unwrap();
    let receiver_dir = tempfile::tempdir().unwrap();
    let source_identity = DeviceIdentity::generate();
    let source_account = Account::generate();
    let receiver_identity = DeviceIdentity::generate();
    let receiver_account = Account::generate();
    let source_announce =
        Announce::new_with_account(&source_identity, &source_account, "Source", 9);
    let receiver_announce =
        Announce::new_with_account(&receiver_identity, &receiver_account, "Receiver", 9);
    let (source, _, _) = super::test_support::node_with_files(
        source_dir.path(),
        source_identity,
        source_account,
        &receiver_announce,
    );
    let (receiver, _, _) = super::test_support::node_with_files(
        receiver_dir.path(),
        receiver_identity,
        receiver_account,
        &source_announce,
    );
    let (path, mut bytes) = sticker(source_dir.path(), 0);
    bytes.resize(crate::file::CHUNK_SIZE + 1, 0);
    std::fs::write(&path, &bytes).unwrap();
    let (offer, file) = source
        .enqueue_file_to_account(&receiver.account_id(), &path, FileKind::Media)
        .await
        .unwrap();
    let event = source
        .delivery
        .lock()
        .unwrap()
        .message(offer)
        .unwrap()
        .destinations[0]
        .event
        .clone();
    receiver
        .log
        .lock()
        .unwrap()
        .append_durable(event.clone())
        .unwrap();
    receiver.process_file_events(event.conversation_id);
    let headers = source.sticker_chunk_proof(file).unwrap();
    assert_eq!(headers.len(), 2);
    assert!(
        receiver.sticker_chunk_proof(file).is_none(),
        "an offered manifest is not retained chunk evidence"
    );
    assert!(
        receiver
            .sticker_recovery_offer(file, &source_announce.public())
            .is_none(),
        "recovery cannot claim an uninstalled image"
    );
    assert!(
        receiver
            .reconstruct_sticker_proof(file, offer, &source_announce.public(), headers.clone())
            .is_none()
    );
    receiver
        .register_sticker_source(path.file_name().unwrap().to_str().unwrap(), &bytes)
        .unwrap();
    assert_eq!(
        receiver.sticker_recovery_offer(file, &source_announce.public()),
        Some(offer)
    );
    let foreign = DeviceIdentity::generate().public();
    assert!(
        receiver
            .reconstruct_sticker_proof(file, offer, &foreign, headers.clone())
            .is_none()
    );
    assert!(
        receiver
            .reconstruct_sticker_proof(
                file,
                EventId::new([91; 32]),
                &source_announce.public(),
                headers.clone()
            )
            .is_none()
    );
    assert!(
        receiver
            .reconstruct_sticker_proof(
                ConversationId::new([92; 32]),
                offer,
                &source_announce.public(),
                headers.clone()
            )
            .is_none()
    );
    assert!(
        receiver
            .reconstruct_sticker_proof(file, offer, &source_announce.public(), vec![])
            .is_none()
    );
    assert!(
        receiver
            .reconstruct_sticker_proof(
                file,
                offer,
                &source_announce.public(),
                vec![headers[0].clone(); 65]
            )
            .is_none()
    );
    receiver.delivery_suspended.store(true, Ordering::Release);
    assert!(
        receiver
            .reconstruct_sticker_proof(file, offer, &source_announce.public(), headers.clone())
            .is_none()
    );
    receiver.delivery_suspended.store(false, Ordering::Release);
    let mut invalid_proofs = Vec::new();
    let mut mutated = headers.clone();
    mutated.reverse();
    invalid_proofs.push(("out-of-order signed ancestry", mutated));
    let mut mutated = headers.clone();
    mutated[0].ciphertext.push(1);
    invalid_proofs.push(("image payload hidden in a header", mutated));
    let mut mutated = headers.clone();
    mutated[0].conversation_id = ConversationId::new([93; 32]);
    invalid_proofs.push(("another transfer scope", mutated));
    let mut mutated = headers.clone();
    mutated[0].author = Author::from_ed25519(foreign.ed25519_pub);
    invalid_proofs.push(("another signing device", mutated));
    let mut mutated = headers.clone();
    mutated[0].seq = 2;
    invalid_proofs.push(("incorrect chunk position", mutated));
    let mut mutated = headers.clone();
    mutated[0].kind = EventKind::FileManifest;
    invalid_proofs.push(("a non-chunk event kind", mutated));
    let mut mutated = headers.clone();
    mutated[0].parents.push(EventId::new([94; 32]));
    invalid_proofs.push(("an invented first parent", mutated));
    let mut mutated = headers.clone();
    mutated[1].parents.clear();
    invalid_proofs.push(("a broken later parent", mutated));
    let mut mutated = headers.clone();
    mutated[1].sig[0] ^= 1;
    invalid_proofs.push(("a forged original-author signature", mutated));
    let mut mutated = headers.clone();
    mutated[1].id = EventId::new([95; 32]);
    invalid_proofs.push(("an invalid original event ID", mutated));
    for (case, invalid) in invalid_proofs {
        assert!(
            receiver
                .reconstruct_sticker_proof(file, offer, &source_announce.public(), invalid)
                .is_none(),
            "{case}"
        );
        assert!(
            receiver.log.lock().unwrap().events(&file).is_empty(),
            "rejected proof must not admit partial chunks"
        );
        assert!(
            receiver.read_media(file).is_none(),
            "rejected proof must not expose artwork"
        );
    }
    let rebuilt = receiver
        .reconstruct_sticker_proof(file, offer, &source_announce.public(), headers.clone())
        .unwrap();
    let retained: Vec<_> = source
        .log
        .lock()
        .unwrap()
        .events(&file)
        .into_iter()
        .cloned()
        .collect();
    assert_eq!(
        rebuilt, retained,
        "valid local bytes must reproduce the exact signed ciphertext events"
    );
    assert!(
        receiver.log.lock().unwrap().events(&file).is_empty(),
        "reconstruction alone is not durable admission"
    );
    assert!(receiver.read_media(file).is_none());
    let crate::file::AnyManifest::V3(base) = source
        .files
        .lock()
        .unwrap()
        .manifest(&file)
        .unwrap()
        .clone()
    else {
        panic!("fixture must use deployed MFM3")
    };
    for case in 0..3 {
        let mut alternate = base.clone();
        alternate.v2.file_conv = ConversationId::new([101 + case; 32]);
        if case == 1 {
            alternate.v2.name = "ordinary-image.png".into();
        }
        if case == 2 {
            alternate.v2.chunk_size = (crate::file::CHUNK_SIZE / 2) as u32;
            alternate.v2.chunk_count =
                bytes.len().div_ceil(alternate.v2.chunk_size as usize) as u32;
            alternate.v2.chunk_hashes = bytes
                .chunks(alternate.v2.chunk_size as usize)
                .map(crate::file::file_checksum)
                .collect();
        }
        let alternate_file = alternate.v2.file_conv;
        let wire = if case == 0 {
            alternate.v2.chunk_hashes.clear();
            crate::file::FileManifestV4 { v3: alternate }.encode()
        } else {
            alternate.encode()
        };
        source
            .append_event(
                event.conversation_id,
                EventKind::FileManifest,
                crate::dm::seal(
                    &source.identity,
                    &receiver.identity.public().x25519_pub,
                    &wire,
                )
                .unwrap(),
            )
            .unwrap();
        let next_offer = source
            .log
            .lock()
            .unwrap()
            .events(&event.conversation_id)
            .last()
            .map(|event| (**event).clone())
            .unwrap();
        receiver
            .log
            .lock()
            .unwrap()
            .append_durable(next_offer.clone())
            .unwrap();
        receiver.process_file_events(next_offer.conversation_id);
        assert!(
            receiver
                .files
                .lock()
                .unwrap()
                .manifest(&alternate_file)
                .is_some(),
            "alternate media is a valid admitted offer"
        );
        assert!(
            receiver
                .reconstruct_sticker_proof(
                    alternate_file,
                    next_offer.id,
                    &source_announce.public(),
                    headers.clone()
                )
                .is_none(),
            "unsupported format, ordinary image intent and nonstandard chunk size retain normal transfer fallback"
        );
        assert!(receiver.read_media(alternate_file).is_none());
    }
}

#[tokio::test]
async fn installed_sticker_header_export_refuses_unsupported_manifests_and_foreign_authors() {
    use crate::eventlog::{ConversationId, Event, EventKind};
    use crate::file::{AnyManifest, FileManifestV4};
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let node = open(
        dir.path(),
        DeviceIdentity::generate(),
        Account::from_secret_bytes(account.secret_bytes()),
    );
    let (path, _) = sticker(dir.path(), 0);
    let (_, original) = node
        .enqueue_file_to_account(&account.account_id(), &path, FileKind::Media)
        .await
        .unwrap();
    let manifest = node
        .files
        .lock()
        .unwrap()
        .manifest(&original)
        .unwrap()
        .clone();
    let AnyManifest::V3(base) = manifest else {
        panic!("fixture must use deployed MFM3")
    };
    let mut unsupported = base.clone();
    unsupported.v2.file_conv = ConversationId::new([71; 32]);
    unsupported.v2.chunk_hashes.clear();
    node.files
        .lock()
        .unwrap()
        .record(AnyManifest::V4(FileManifestV4 { v3: unsupported }));
    assert!(
        node.sticker_chunk_proof(ConversationId::new([71; 32]))
            .is_none(),
        "unsupported proof format keeps ordinary transfer fallback"
    );
    let mut missing = base.clone();
    missing.v2.file_conv = ConversationId::new([72; 32]);
    node.files.lock().unwrap().record(AnyManifest::V3(missing));
    assert!(
        node.sticker_chunk_proof(ConversationId::new([72; 32]))
            .is_none(),
        "a manifest cannot substitute for missing signed chunks"
    );
    let mut ordinary = base.clone();
    ordinary.v2.file_conv = ConversationId::new([73; 32]);
    ordinary.kind = FileKind::File;
    node.files.lock().unwrap().record(AnyManifest::V3(ordinary));
    assert!(
        node.sticker_chunk_proof(ConversationId::new([73; 32]))
            .is_none(),
        "ordinary file offers do not opt into sticker proof reuse"
    );
    let foreign = DeviceIdentity::generate();
    let mut copied = base;
    copied.v2.file_conv = ConversationId::new([74; 32]);
    let ciphertext = node.log.lock().unwrap().events(&original)[0]
        .ciphertext
        .clone();
    let event = Event::new(
        &foreign,
        copied.v2.file_conv,
        1,
        vec![],
        1,
        0,
        EventKind::Message,
        ciphertext,
    );
    node.log.lock().unwrap().append_durable(event).unwrap();
    node.files.lock().unwrap().record(AnyManifest::V3(copied));
    assert!(
        node.sticker_chunk_proof(ConversationId::new([74; 32]))
            .is_none(),
        "retaining another author's ciphertext does not let this node export it as its own signed proof"
    );
}
