use super::*;
use crate::file::FileKind;
use crate::identity::{account::Account, device::DeviceIdentity};
use base64::Engine;
use std::path::Path;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

fn open(dir: &Path, device: DeviceIdentity, account: Account) -> Arc<Node> {
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
    assert_eq!(node.account_history(&target, 10).unwrap().len(), 2);
    assert!(
        node.reusable_sticker(
            crate::eventlog::ConversationId::new([9; 32]),
            None,
            path.file_name().unwrap().to_str().unwrap(),
            &bytes,
            None
        )
        .unwrap()
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
    assert_eq!(reopened.account_history(&target, 10).unwrap().len(), 1);
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
    assert!(node.account_history(&target, 10).unwrap().is_empty());
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
    let history = node.account_history(&target, 10).unwrap();
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
    let history = node.channel_history(channel, 10).unwrap();
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
    assert_eq!(node.channel_history(first_group, 10).unwrap().len(), 2);
    node.clear_conversation(first_group).unwrap();
    assert!(node.read_media(first).is_none());
    assert_eq!(node.read_media(different_group).unwrap(), bytes);
}

#[tokio::test]
async fn sticker_media_delivers_after_source_removal_without_receiver_installation() {
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
    let (path, bytes) = sticker(ad.path(), 0);
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
            .unwrap()
            .iter()
            .any(|row| row.id == id && row.recalled && row.file.is_none()),
        "the authenticated sender recall hides sticker media on the recipient"
    );
    assert!(
        b.account_history(&a.account_id(), 10)
            .unwrap()
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
        !b.complete_sticker_aliases(file).unwrap(),
        "corrupted durable bytes cannot complete an alias"
    );
    assert!(!b.delivery.lock().unwrap().file_completed(alias.id));
    b.media.remove(file).unwrap();
    assert!(
        !b.complete_sticker_aliases(file).unwrap(),
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
        !b.complete_sticker_aliases(file).unwrap(),
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
            !b.complete_sticker_aliases(file).unwrap(),
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
        !b.complete_sticker_aliases(file).unwrap(),
        "erased completion is not reusable ancestry"
    );
    assert!(!b.delivery.lock().unwrap().file_completed(alias.id));
    serving.abort();
    let _ = serving.await;
}
