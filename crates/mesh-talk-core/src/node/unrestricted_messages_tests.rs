//! Upgrade regressions: retired preferences cannot restrict messages or hide retained text.
use super::*;
use crate::{
    discovery::Announce,
    identity::{account::Account, device::DeviceIdentity},
    storage::record_log::EncryptedRecordLog,
};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
enum RetiredLanguageRecord {
    Profile(profile::ProfilePayload),
    Decision {
        id: crate::eventlog::EventId,
        hidden: bool,
    },
}

fn seed_retired_preferences(
    dir: &std::path::Path,
    account: &Account,
    hidden: Option<crate::eventlog::EventId>,
) {
    let (mut log, _) = EncryptedRecordLog::<RetiredLanguageRecord>::open(
        &dir.join("language.log"),
        "pw",
        b"MTLANG",
    )
    .unwrap();
    log.append_durable(&RetiredLanguageRecord::Profile(
        profile::ProfilePayload::new_with_language(account, None, 100, Some("en".into())),
    ))
    .unwrap();
    if let Some(id) = hidden {
        log.append_durable(&RetiredLanguageRecord::Decision { id, hidden: true })
            .unwrap();
    }
}

#[tokio::test]
async fn retired_language_preferences_do_not_block_account_or_integration_sends() {
    let dir = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let account = Account::generate();
    let proof = Announce::new_with_account(&peer, &account, "peer", 9);
    seed_retired_preferences(dir.path(), &account, None);
    let (node, _, _) = test_support::node_with_files(
        dir.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    );
    let text = "你好，今天我们一起讨论这个项目。".as_bytes();
    assert!(
        node.enqueue_to_account(&account.account_id(), text, None)
            .await
            .is_ok(),
        "retired preferences must not block account sends"
    );
    let request = outgoing_message::OutgoingMessageRequest {
        request_id: [42; 32],
        destination: outgoing_message::MessageDestination::Account(account.account_id()),
        text: text.to_vec(),
        reply_to: None,
        expires_at_ms: None,
    };
    assert!(
        node.enqueue_message_once_if(&request, |accept| accept())
            .await
            .is_ok(),
        "retired preferences must not block integration sends"
    );
}

#[tokio::test]
async fn retired_hidden_decisions_do_not_hide_loopback_history_search_or_recovery() {
    use std::time::Duration;
    use tokio::{net::TcpListener, time::timeout};
    let ad = tempfile::tempdir().unwrap();
    let bd = tempfile::tempdir().unwrap();
    let restored_dir = tempfile::tempdir().unwrap();
    let alice = DeviceIdentity::generate();
    let aa = Account::generate();
    let bob = DeviceIdentity::generate();
    let bob_secret = bob.secret_bytes();
    let ba = Account::generate();
    let ba_secret = ba.secret_bytes();
    let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
    let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
    let (a, _, _) = test_support::node_with_files(ad.path(), alice, aa, &bp);
    let text = "你好，今天我们一起讨论这个项目。";
    let id = a
        .enqueue_to_account(&ba.account_id(), text.as_bytes(), None)
        .await
        .unwrap();
    seed_retired_preferences(bd.path(), &ba, Some(id));
    let (b, mut received, _) = test_support::node_with_files(bd.path(), bob, ba, &ap);
    let at = tokio::spawn(a.clone().run_accept_loop(al));
    let bt = tokio::spawn(b.clone().run_accept_loop(bl));
    timeout(Duration::from_secs(8), a.flush_delivery(id))
        .await
        .unwrap();
    let message = timeout(Duration::from_secs(8), received.recv())
        .await
        .unwrap()
        .unwrap();
    at.abort();
    bt.abort();
    let _ = at.await;
    let _ = bt.await;
    assert_eq!(message.text, text.as_bytes());
    assert_eq!(
        b.account_history(&a.account_id(), 10)[0].text,
        text.as_bytes()
    );
    assert!(!b.search("项目").is_empty());
    assert!(!b.export_account_backfill().is_empty());
    drop(b);
    let (reopened, _, _) = test_support::node_with_files(
        bd.path(),
        DeviceIdentity::from_secret_bytes(bob_secret.0, bob_secret.1),
        Account::from_secret_bytes(ba_secret),
        &ap,
    );
    assert_eq!(
        reopened.account_history(&a.account_id(), 10)[0].text,
        text.as_bytes()
    );
    let mut snapshot = Vec::new();
    reopened
        .write_recovery_snapshot(&mut snapshot, 100)
        .unwrap();
    use crate::recovery::snapshot::{SnapshotRecordV1, read_snapshot, write_snapshot};
    let mut records = Vec::new();
    let mut header = read_snapshot(std::io::Cursor::new(snapshot), |record| {
        assert!(
            !matches!(
                record,
                SnapshotRecordV1::DmLanguageProfile { .. }
                    | SnapshotRecordV1::DmLanguageDecision { .. }
            ),
            "new backups must omit retired metadata"
        );
        records.push(record);
        Ok(())
    })
    .unwrap();
    // Reconstruct a schema-2 backup containing both retired records, as shipped previously.
    records.push(SnapshotRecordV1::DmLanguageProfile {
        payload: profile::ProfilePayload::new_with_language(
            &Account::from_secret_bytes(ba_secret),
            None,
            100,
            Some("en".into()),
        )
        .encode(),
    });
    records.push(SnapshotRecordV1::DmLanguageDecision {
        id: *id.as_bytes(),
        hidden: true,
    });
    header.record_count = records.len() as u64;
    let mut legacy_snapshot = Vec::new();
    write_snapshot(&mut legacy_snapshot, &header, records).unwrap();
    let restored = Node::install_recovery_snapshot(
        std::io::Cursor::new(legacy_snapshot),
        &restored_dir.path().join("restored"),
        "pw",
    )
    .unwrap();
    assert_eq!(
        restored.account_history(&a.account_id(), 10)[0].text,
        text.as_bytes()
    );
    assert!(!restored.search("项目").is_empty());
}
