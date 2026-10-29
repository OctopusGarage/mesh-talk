use super::*;

use crate::identity::{account::Account, device::DeviceIdentity};
use crate::node::{
    received_log::{ReceivedEntry, ReceivedLog},
    received_store::ReceivedStore,
    sent_store::SentStore,
    sentlog::{SentEntry, SentLog},
};
use crate::storage::{
    profile::{ProfileStorageLease, context::ProfileStorage},
    record_log::EncryptedRecordLog,
};
use std::sync::Arc;

fn conv(n: u8) -> ConversationId {
    ConversationId::new([n; 32])
}
fn profile(root: &Path, identity: &DeviceIdentity, account: &Account) -> Arc<ProfileStorage> {
    let profile = ProfileStorage::open_participant(
        ProfileStorageLease::acquire(root).unwrap(),
        &identity.public(),
        &account.public(),
        &root.join("messages.log"),
        &root.join("sent.log"),
        "pw",
    )
    .unwrap();
    profile.complete_participant("pw", None).unwrap();
    profile
}
fn sent(seq: u64) -> SentEntry {
    SentEntry {
        conversation: conv(1),
        seq,
        wall_clock: seq,
        plaintext: vec![42; 4096],
    }
}
fn received(n: u8) -> ReceivedEntry {
    ReceivedEntry {
        event_id: EventId::new([n; 32]),
        conversation: conv(1),
        from: "alice".into(),
        wall_clock: u64::from(n),
        plaintext: vec![42; 4096],
    }
}

#[test]
fn runtime_leaf_gap_revisions_survive_warm_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let path = dir.path().join("sent.log");
    let owner = profile(dir.path(), &identity, &account);
    let mut store = SentStore::from_profile(owner.clone(), &path, "pw").unwrap();
    store.record_durable(&sent(1)).unwrap();
    let conversation_revision = store.conversation_revision(&conv(1)).unwrap();
    let revision = store.revision();
    assert_ne!(revision, 0);
    store.remove_where(|_| true).unwrap();
    let empty_revision = store.conversation_revision(&conv(1)).unwrap();
    assert!(empty_revision > conversation_revision);
    assert!(store.conversation_page(None, 128).unwrap().is_empty());
    let removed_revision = store.revision();
    let rewritten = store.rewrite_revision();
    drop(store);
    drop(owner);
    crate::storage::record_log::frame_observations::reset();
    let owner = profile(dir.path(), &identity, &account);
    let mut store = SentStore::from_profile(owner, &path, "pw").unwrap();
    assert_eq!(store.revision(), removed_revision);
    assert_eq!(store.rewrite_revision(), rewritten);
    assert_eq!(
        store.conversation_revision(&conv(1)).unwrap(),
        empty_revision
    );
    assert_eq!(store.body_reads(), 0);
    assert_eq!(
        crate::storage::record_log::frame_observations::maximum_capacity(),
        0
    );
    store.record_durable(&sent(2)).unwrap();
    assert!(store.revision() > removed_revision);
    assert!(store.conversation_revision(&conv(1)).unwrap() > empty_revision);
}

#[test]
fn runtime_leaf_gap_recovered_ids_do_not_replay_on_warm_open() {
    use crate::node::recovered_sent_ids::{IndexedRecoveredSentIds, RecoveredSentIds};
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("recovery-sent-ids.log");
    let mut store = RecoveredSentIds::open(&path, "pw").unwrap();
    for seq in 0..256 {
        store.record(&sent(seq), EventId::new([7; 32])).unwrap();
    }
    drop(store);
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let mut store = IndexedRecoveredSentIds::from_profile(owner.clone(), &path, "pw").unwrap();
    store.record(&sent(255), EventId::new([7; 32])).unwrap();
    assert!(store.record(&sent(255), EventId::new([8; 32])).is_err());
    drop(store);
    drop(owner);
    crate::storage::record_log::frame_observations::reset();
    let owner = profile(dir.path(), &identity, &account);
    let store = IndexedRecoveredSentIds::from_profile(owner.clone(), &path, "pw").unwrap();
    assert_eq!(
        store.id_for(&sent(255)).unwrap(),
        Some(EventId::new([7; 32]))
    );
    assert_eq!(
        crate::storage::record_log::frame_observations::maximum_capacity(),
        0
    );
    let mut transcript =
        SentStore::from_profile(owner, &dir.path().join("sent.log"), "pw").unwrap();
    transcript.record_durable(&sent(255)).unwrap();
    let header = transcript
        .header_scan_page(None, None, TranscriptPageLimit::default())
        .unwrap()
        .rows
        .remove(0);
    assert_eq!(
        store.id_for_header(&header).unwrap(),
        Some(EventId::new([7; 32]))
    );
    assert_eq!(transcript.body_reads(), 0);
}

#[test]
fn metadata_pages_select_keys_before_exact_ordinal_body_fetch() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let mut store =
        SentStore::from_profile(owner.clone(), &dir.path().join("sent.log"), "pw").unwrap();
    let received =
        ReceivedStore::from_profile(owner, &dir.path().join("received.log"), "pw").unwrap();
    for seq in 1..=6 {
        store.record_durable(&sent(seq)).unwrap();
    }
    let page = store
        .header_page(
            Some(&conv(1)),
            None,
            TranscriptPageLimit::new(128, 512).unwrap(),
            true,
        )
        .unwrap();
    assert_eq!(
        page.rows.iter().map(|h| h.sequence).collect::<Vec<_>>(),
        vec![6, 5]
    );
    assert_eq!(store.body_reads(), 0);
    let older = store
        .header_page(
            Some(&conv(1)),
            page.next,
            TranscriptPageLimit::new(128, 512).unwrap(),
            true,
        )
        .unwrap();
    assert_eq!(
        older.rows.iter().map(|h| h.sequence).collect::<Vec<_>>(),
        vec![4, 3]
    );
    let header = &page.rows[0];
    assert_eq!(
        header.plaintext_sha256,
        <[u8; 32]>::from(Sha256::digest(&sent(6).plaintext))
    );
    assert_eq!(store.row_by_ordinal(header.ordinal).unwrap(), Some(sent(6)));
    assert!(received.row_by_ordinal(header.ordinal).unwrap().is_none());
    assert_eq!(store.body_reads(), 1);
    assert_eq!(received.body_reads(), 0);
    for limit in [
        TranscriptPageLimit {
            rows: usize::MAX,
            bytes: 1,
        },
        TranscriptPageLimit { rows: 1, bytes: 0 },
    ] {
        assert!(store.header_page(None, None, limit, false).is_err());
        assert!(store.header_scan_page(None, None, limit).is_err());
    }
    assert!(
        store
            .header_scan_page(None, None, TranscriptPageLimit::new(1, 255).unwrap())
            .is_err()
    );
    assert_eq!(store.body_reads(), 1);
}

#[test]
fn file_selector_is_scoped_metadata_and_deleted_ordinals_cascade() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let mut files = ReceivedStore::file_cards_from_profile(
        owner.clone(),
        &dir.path().join("received_files.log"),
        "pw",
    )
    .unwrap();
    let mut messages =
        ReceivedStore::from_profile(owner.clone(), &dir.path().join("received.log"), "pw").unwrap();
    let manifest = crate::file::FileManifest {
        name: "sample.bin".into(),
        size: 1,
        mime: "application/octet-stream".into(),
        checksum: [1; 32],
        file_key: [2; 32],
        file_conv: conv(9),
        chunk_count: 1,
    }
    .encode();
    let mut row = received(1);
    row.plaintext = manifest;
    files.record_durable(&row).unwrap();
    row.conversation = conv(2);
    files.record_file_alias_durable(&row).unwrap();
    messages.record_durable(&row).unwrap();
    files.record_durable(&received(2)).unwrap();
    let page = files
        .file_headers(&conv(9), None, TranscriptPageLimit::new(1, 256).unwrap())
        .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].conversation, conv(1));
    assert_eq!(page.rows[0].file_conversation, Some(conv(9)));
    let ordinal = page.rows[0].ordinal;
    let tail = files
        .file_headers(&conv(9), page.next, TranscriptPageLimit::default())
        .unwrap();
    assert_eq!(tail.rows.len(), 1);
    assert_eq!(tail.rows[0].conversation, conv(2));
    assert_eq!(files.body_reads(), 0);
    assert_eq!(
        messages
            .header_scan_page(None, None, TranscriptPageLimit::default())
            .unwrap()
            .rows[0]
            .file_conversation,
        None
    );
    assert!(
        messages
            .file_headers(&conv(9), None, TranscriptPageLimit::default())
            .is_err()
    );
    owner.database().transaction(|c| {
        c.execute_batch("CREATE TABLE test_file_projection(ordinal INTEGER PRIMARY KEY REFERENCES transcript_rows(ordinal) ON DELETE CASCADE)")?;
        c.execute("INSERT INTO test_file_projection VALUES(?1)",[ordinal])?;
        Ok(())
    }).unwrap();
    files
        .remove_where(|row| row.conversation == conv(1))
        .unwrap();
    assert!(files.row_by_ordinal(ordinal).unwrap().is_none());
    assert_eq!(
        owner
            .database()
            .read(|c| Ok(
                c.query_row("SELECT count(*) FROM test_file_projection", [], |r| r
                    .get::<_, i64>(0))?
            ))
            .unwrap(),
        0
    );
    assert_eq!(
        files
            .file_headers(&conv(9), None, TranscriptPageLimit::default())
            .unwrap()
            .rows
            .len(),
        1
    );
}

#[test]
fn recovered_source_restarts_each_publication_boundary_without_replay() {
    use crate::node::recovered_sent_ids::{IndexedRecoveredSentIds, RecoveredSentIds};
    for point in [
        "transcript-imported",
        "transcript-barrier",
        "transcript-published",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let path = dir.path().join("recovery-sent-ids.log");
        let mut legacy = RecoveredSentIds::open(&path, "pw").unwrap();
        legacy.record(&sent(1), EventId::new([8; 32])).unwrap();
        drop(legacy);
        let owner = profile(dir.path(), &identity, &account);
        publication::fail_at(Some(point));
        assert!(IndexedRecoveredSentIds::from_profile(owner.clone(), &path, "pw").is_err());
        publication::fail_at(None);
        drop(owner);
        crate::storage::record_log::frame_observations::reset();
        let owner = profile(dir.path(), &identity, &account);
        let store = IndexedRecoveredSentIds::from_profile(owner, &path, "pw").unwrap();
        assert_eq!(store.id_for(&sent(1)).unwrap(), Some(EventId::new([8; 32])));
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0
        );
    }
}

#[test]
fn channel_install_commits_body_evidence_and_effect_together() {
    use crate::node::reception_effects::{ArrivalKind, ReceptionEffects, ReceptionMetadata};
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let mut rows =
        ReceivedStore::from_profile(owner.clone(), &dir.path().join("received.log"), "pw").unwrap();
    let mut suppression =
        crate::node::account_suppression::IndexedAccountSuppression::from_profile(
            owner.clone(),
            &dir.path().join("account-suppression.log"),
            "pw",
        )
        .unwrap();
    let effects = ReceptionEffects::open(owner.clone()).unwrap();
    let metadata = ReceptionMetadata {
        from_device: "alice".into(),
        received_at: 55,
        arrival_kind: ArrivalKind::Live,
        own_device: false,
        batch_id: None,
        batch_complete: true,
        batch_size: 1,
        reception_only: false,
    };
    let row = received(10);
    rows.check_write().unwrap();
    let failed: Result<(), LogError> = owner.database().transaction(|c| {
        assert!(ReceivedStore::install_channel_on(c, &row, Some(&metadata))?);
        Err(std::io::Error::other("checkpoint installation failed").into())
    });
    assert!(failed.is_err());
    assert!(!rows.contains(row.event_id).unwrap());
    assert!(!rows.processed(row.event_id).unwrap());
    assert_eq!(effects.head().unwrap().sequence, 0);
    assert_eq!(rows.revision(), 0);
    let inserted = owner
        .database()
        .transaction(|c| ReceivedStore::install_channel_on(c, &row, Some(&metadata)))
        .unwrap();
    rows.note_channel_commit(usize::from(inserted));
    assert_eq!(rows.revision(), 1);
    assert_eq!(effects.head().unwrap().sequence, 1);
    assert!(rows.processed(row.event_id).unwrap());
    let inserted = owner
        .database()
        .transaction(|c| ReceivedStore::install_channel_on(c, &row, Some(&metadata)))
        .unwrap();
    rows.note_channel_commit(usize::from(inserted));
    assert_eq!(rows.revision(), 1);
    assert_eq!(effects.head().unwrap().sequence, 1);
    suppression
        .suppress(conv(1), EventId::new([11; 32]))
        .unwrap();
    assert!(
        !owner
            .database()
            .transaction(|c| ReceivedStore::install_channel_on(c, &received(11), Some(&metadata)))
            .unwrap()
    );
    assert!(!rows.contains(EventId::new([11; 32])).unwrap());
    assert_eq!(effects.head().unwrap().sequence, 1);
    rows.remove_where(|_| true).unwrap();
    assert!(!rows.contains(row.event_id).unwrap());
    assert!(rows.processed(row.event_id).unwrap());
    let sender = publication::PublishedHistorySource::open(
        owner,
        &dir.path().join("channel.senders"),
        "pw",
        CHANNEL_SENDERS,
        |_, _, _| Ok(()),
    )
    .unwrap();
    sender.check_write().unwrap();
}

#[test]
fn ordinary_reopen_does_not_decode_historical_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sent.log");
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let mut legacy = SentLog::open(&path, "pw").unwrap();
    for seq in 0..64 {
        legacy.record_durable(&sent(seq)).unwrap();
    }
    drop(legacy);
    let owner = profile(dir.path(), &identity, &account);
    let store = SentStore::from_profile(owner.clone(), &path, "pw").unwrap();
    assert_eq!(store.count(None).unwrap(), 64);
    drop(store);
    drop(owner);
    crate::storage::record_log::frame_observations::reset();
    let owner = profile(dir.path(), &identity, &account);
    let store = SentStore::from_profile(owner, &path, "pw").unwrap();
    assert_eq!(
        crate::storage::record_log::frame_observations::maximum_capacity(),
        0
    );
    assert_eq!(store.body_reads(), 0);
    assert_eq!(
        store.usage().unwrap(),
        TranscriptUsage {
            records: 64,
            body_bytes: 64 * 4096
        }
    );
    assert_eq!(store.body_reads(), 0);
    let page = store
        .page(
            Some(&conv(1)),
            None,
            TranscriptPageLimit::new(8, 8192).unwrap(),
        )
        .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(store.body_reads(), 2);
    assert!(page.next.is_some());
    let next = store
        .page(
            Some(&conv(1)),
            page.next,
            TranscriptPageLimit::new(8, 8192).unwrap(),
        )
        .unwrap();
    assert_eq!(next.rows[0].seq, 2);
}

#[test]
fn received_lookup_alias_and_conflicts_remain_exact() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let mut rows =
        ReceivedStore::from_profile(owner.clone(), &dir.path().join("received.log"), "pw").unwrap();
    let original = received(1);
    rows.record_durable(&original).unwrap();
    rows.record_durable(&original).unwrap();
    let mut alias = original.clone();
    alias.conversation = conv(2);
    assert!(rows.record_durable(&alias).is_err());
    assert_eq!(
        rows.entry(original.event_id).unwrap(),
        Some(original.clone())
    );
    let mut files =
        ReceivedStore::file_cards_from_profile(owner, &dir.path().join("received_files.log"), "pw")
            .unwrap();
    files.record_durable(&original).unwrap();
    files.record_file_alias_durable(&alias).unwrap();
    files.record_file_alias_durable(&alias).unwrap();
    assert_eq!(files.count(None).unwrap(), 2);
    alias.from = "mallory".into();
    assert!(files.record_file_alias_durable(&alias).is_err());
    assert_eq!(
        files
            .remove_where(|row| row.conversation == conv(1))
            .unwrap(),
        1
    );
    assert_eq!(
        files
            .entry(original.event_id)
            .unwrap()
            .unwrap()
            .conversation,
        conv(2)
    );
}

#[test]
fn migration_conflict_rolls_back_and_preserves_legacy_source() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let path = dir.path().join("sent.log");
    let (mut legacy, _) = EncryptedRecordLog::<SentEntry>::open(&path, "pw", b"MTSENT").unwrap();
    legacy.append_durable(&sent(1)).unwrap();
    let mut conflict = sent(1);
    conflict.plaintext = vec![99];
    legacy.append_durable(&conflict).unwrap();
    drop(legacy);
    let original = std::fs::read(&path).unwrap();
    let owner = profile(dir.path(), &identity, &account);
    assert!(SentStore::from_profile(owner.clone(), &path, "pw").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    assert!(
        owner
            .database()
            .read(|c| Ok(
                c.query_row("SELECT count(*) FROM transcript_rows", [], |r| r
                    .get::<_, i64>(0))?
            ))
            .unwrap()
            == 0
    );
}

#[test]
fn independently_interrupted_publication_recovers_without_history_replay() {
    for point in [
        "transcript-imported",
        "transcript-barrier",
        "transcript-published",
    ] {
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let path = dir.path().join("sent.log");
        let mut legacy = SentLog::open(&path, "pw").unwrap();
        legacy.record_durable(&sent(1)).unwrap();
        drop(legacy);
        let owner = profile(dir.path(), &identity, &account);
        publication::fail_at(Some(point));
        assert!(SentStore::from_profile(owner.clone(), &path, "pw").is_err());
        publication::fail_at(None);
        drop(owner);
        crate::storage::record_log::frame_observations::reset();
        let owner = profile(dir.path(), &identity, &account);
        let mut reopened = SentStore::from_profile(owner.clone(), &path, "pw").unwrap();
        assert_eq!(
            crate::storage::record_log::frame_observations::maximum_capacity(),
            0
        );
        assert_eq!(reopened.count(None).unwrap(), 1);
        assert!(SentLog::open(&path, "pw").is_err());
        reopened.record_durable(&sent(2)).unwrap();
        let receive_path = dir.path().join("received.log");
        let mut receive_legacy = ReceivedLog::open(&receive_path, "pw").unwrap();
        receive_legacy.record_durable(&received(1)).unwrap();
        drop(receive_legacy);
        let receives = ReceivedStore::from_profile(owner, &receive_path, "pw").unwrap();
        assert_eq!(receives.count(None).unwrap(), 1);
    }
}

#[test]
fn page_rejects_oversized_row_before_fetching_payload() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let mut store = SentStore::from_profile(owner, &dir.path().join("sent.log"), "pw").unwrap();
    store.record_durable(&sent(1)).unwrap();
    assert!(
        store
            .page(None, None, TranscriptPageLimit::new(1, 4095).unwrap())
            .is_err()
    );
    assert_eq!(store.body_reads(), 0);
}

#[test]
fn bounded_removal_and_usage_survive_reopen() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("sent.log");
    let mut store = SentStore::from_profile(owner.clone(), &path, "pw").unwrap();
    for seq in 0..300 {
        store.record_durable(&sent(seq)).unwrap();
    }
    assert_eq!(store.remove_where(|row| row.seq % 2 == 0).unwrap(), 150);
    assert_eq!(
        store.usage().unwrap(),
        TranscriptUsage {
            records: 150,
            body_bytes: 150 * 4096
        }
    );
    drop(store);
    drop(owner);
    let owner = profile(dir.path(), &identity, &account);
    let store = SentStore::from_profile(owner, &path, "pw").unwrap();
    assert_eq!(store.count(None).unwrap(), 150);
    assert_eq!(store.body_reads(), 0);
    assert!(store.entry(&conv(1), 0).unwrap().is_none());
    assert!(store.entry(&conv(1), 1).unwrap().is_some());
}

#[test]
fn exact_suppression_is_disk_backed_and_export_pages_resume() {
    use crate::node::account_suppression::IndexedAccountSuppression;
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("account-suppression.log");
    let mut store = IndexedAccountSuppression::from_profile(owner.clone(), &path, "pw").unwrap();
    for n in 0u16..300 {
        let mut id = [0; 32];
        id[..2].copy_from_slice(&n.to_be_bytes());
        store.suppress(conv(1), EventId::new(id)).unwrap();
    }
    assert_eq!(store.len().unwrap(), 300);
    assert_eq!(store.marker_bytes().unwrap(), 300 * 64);
    drop(store);
    drop(owner);
    crate::storage::record_log::frame_observations::reset();
    let owner = profile(dir.path(), &identity, &account);
    let store = IndexedAccountSuppression::from_profile(owner, &path, "pw").unwrap();
    assert_eq!(
        crate::storage::record_log::frame_observations::maximum_capacity(),
        0
    );
    let first = store.page(None, 128).unwrap();
    assert_eq!(first.len(), 128);
    let second = store.page(first.last().copied(), 128).unwrap();
    assert_eq!(second.len(), 128);
    let third = store.page(second.last().copied(), 128).unwrap();
    assert_eq!(third.len(), 44);
    assert!(store.contains(first[0].0, first[0].1).unwrap());
    assert!(!store.contains(conv(2), first[0].1).unwrap());
}

#[test]
fn received_membership_and_deadline_selection_do_not_fetch_plaintext() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("received.log");
    let mut store = ReceivedStore::from_profile(owner, &path, "pw").unwrap();
    for n in 1..100 {
        store.record_durable(&received(n)).unwrap();
    }
    let mut expiry = received(200);
    expiry.plaintext = crate::expiry::wrap(b"secret", Some(1000));
    store.record_durable(&expiry).unwrap();
    assert!(store.contains(expiry.event_id).unwrap());
    assert!(!store.contains(EventId::new([201; 32])).unwrap());
    assert!(store.deadline_page(Some(999), None, 16).unwrap().is_empty());
    let due = store.deadline_page(Some(1000), None, 16).unwrap();
    assert_eq!(due.len(), 1);
    assert_eq!(due[0].event_id, Some(expiry.event_id));
    assert_eq!(due[0].cursor.deadline, 1000);
    assert_eq!(store.body_reads(), 0);
}

#[test]
fn delete_failure_rolls_back_rows_and_metadata_together() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("sent.log");
    let mut store = SentStore::from_profile(owner.clone(), &path, "pw").unwrap();
    store.record_durable(&sent(1)).unwrap();
    store.record_durable(&sent(2)).unwrap();
    owner.database().transaction(|c| {c.execute_batch("CREATE TRIGGER fail_transcript_delete BEFORE DELETE ON transcript_rows WHEN OLD.ordinal=2 BEGIN SELECT RAISE(ABORT,'injected delete failure'); END;")?;Ok(())}).unwrap();
    assert!(store.remove_where(|_| true).is_err());
    assert_eq!(
        store.usage().unwrap(),
        TranscriptUsage {
            records: 2,
            body_bytes: 8192
        }
    );
    assert!(store.entry(&conv(1), 1).unwrap().is_some());
    owner
        .database()
        .transaction(|c| {
            c.execute_batch("DROP TRIGGER fail_transcript_delete")?;
            Ok(())
        })
        .unwrap();
    assert_eq!(store.remove_where(|_| true).unwrap(), 2);
    assert_eq!(
        store.usage().unwrap(),
        TranscriptUsage {
            records: 0,
            body_bytes: 0
        }
    );
}

#[test]
fn sent_logical_keys_and_unsigned_history_order_survive_restart() {
    use crate::node::dm_envelope::DmEnvelope;
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("sent.log");
    let mut store = SentStore::from_profile(owner.clone(), &path, "pw").unwrap();
    let mut first = sent(7);
    first.wall_clock = u64::MAX;
    first.plaintext = DmEnvelope::new("a".into(), "b".into(), [1; 32], b"one".to_vec()).encode();
    let mut second = first.clone();
    second.wall_clock = 1;
    second.plaintext = DmEnvelope::new("a".into(), "b".into(), [2; 32], b"two".to_vec()).encode();
    store.record_durable(&first).unwrap();
    store.record_durable(&second).unwrap();
    let mut conflict = first.clone();
    conflict.seq = 8;
    assert!(store.record_durable(&conflict).is_err());
    drop(store);
    drop(owner);
    let owner = profile(dir.path(), &identity, &account);
    let store = SentStore::from_profile(owner, &path, "pw").unwrap();
    let page = store
        .page(
            Some(&conv(1)),
            None,
            TranscriptPageLimit::new(1, 4096).unwrap(),
        )
        .unwrap();
    assert_eq!(page.rows, vec![second]);
    let next = store
        .page(Some(&conv(1)), page.next, TranscriptPageLimit::default())
        .unwrap();
    assert_eq!(next.rows, vec![first.clone()]);
    assert_eq!(store.account_entry(&conv(1), [1; 32]).unwrap(), Some(first));
}

#[test]
fn divergent_unpublished_legacy_source_is_preserved_and_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("sent.log");
    let mut legacy = SentLog::open(&path, "pw").unwrap();
    legacy.record_durable(&sent(1)).unwrap();
    drop(legacy);
    publication::fail_at(Some("transcript-imported"));
    assert!(SentStore::from_profile(owner.clone(), &path, "pw").is_err());
    publication::fail_at(None);
    let mut legacy = SentLog::open(&path, "pw").unwrap();
    legacy.record_durable(&sent(2)).unwrap();
    drop(legacy);
    let bytes = std::fs::read(&path).unwrap();
    assert!(SentStore::from_profile(owner, &path, "pw").is_err());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
}

#[test]
fn recent_sparse_history_selects_last_keys_before_fetching_large_bodies() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let path = dir.path().join("sent.log");
    let (mut legacy, _) = EncryptedRecordLog::<SentEntry>::open(&path, "pw", b"MTSENT").unwrap();
    for seq in 0..1024 {
        let row = SentEntry {
            conversation: if seq % 97 == 0 { conv(1) } else { conv(2) },
            seq,
            wall_clock: seq,
            plaintext: vec![17; 32 * 1024],
        };
        legacy.append(&row).unwrap();
    }
    legacy.sync().unwrap();
    drop(legacy);
    let owner = profile(dir.path(), &identity, &account);
    let store = SentStore::from_profile(owner.clone(), &path, "pw").unwrap();
    drop(store);
    drop(owner);
    let owner = profile(dir.path(), &identity, &account);
    let store = SentStore::from_profile(owner, &path, "pw").unwrap();
    assert_eq!(store.body_reads(), 0);
    let limit = TranscriptPageLimit::new(3, 3 * 32 * 1024).unwrap();
    let newest = store.recent_page(Some(&conv(1)), None, limit).unwrap();
    assert_eq!(
        newest.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![970, 873, 776]
    );
    assert_eq!(store.body_reads(), 3);
    let older = store
        .recent_page(Some(&conv(1)), newest.next, limit)
        .unwrap();
    assert_eq!(
        older.rows.iter().map(|row| row.seq).collect::<Vec<_>>(),
        vec![679, 582, 485]
    );
    assert_eq!(store.body_reads(), 6);
}

#[test]
fn review_gap_invalid_page_budgets_fail_before_body_reads_or_panics() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let mut store = SentStore::from_profile(owner, &dir.path().join("sent.log"), "pw").unwrap();
    store.record_durable(&sent(1)).unwrap();
    for (rows, bytes) in [
        (0, 4096),
        (129, 4096),
        (1, 0),
        (1, MAX_PAGE_BYTES + 1),
        (usize::MAX, usize::MAX),
    ] {
        for order in 0..3 {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let limit = TranscriptPageLimit { rows, bytes };
                match order {
                    0 => store.page(None, None, limit).map(|page| page.rows.len()),
                    1 => store
                        .recent_page(None, None, limit)
                        .map(|page| page.rows.len()),
                    _ => store
                        .scan_page(None, None, limit)
                        .map(|page| page.rows.len()),
                }
            }));
            assert!(
                result.is_ok(),
                "invalid budget panicked for order {order}, rows {rows}, bytes {bytes}"
            );
            assert!(
                result.unwrap().is_err(),
                "invalid budget accepted for order {order}, rows {rows}, bytes {bytes}"
            );
            assert_eq!(store.body_reads(), 0);
        }
    }
}

#[test]
fn review_gap_source_append_after_import_preserves_original_and_rolls_back() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let path = dir.path().join("sent.log");
    let (mut legacy, _) = EncryptedRecordLog::<SentEntry>::open(&path, "pw", b"MTSENT").unwrap();
    legacy.append_durable(&sent(1)).unwrap();
    drop(legacy);
    let owner = profile(dir.path(), &identity, &account);
    let mut changed_bytes = Vec::new();
    let result = TranscriptRepository::open(
        owner.clone(),
        &path,
        "pw",
        SENT,
        |c, read_path, password| {
            let writer = EncryptedRecordLog::<SentEntry>::open_with_visitor(
                read_path,
                password,
                b"MTSENT",
                |entry| {
                    let mut key = vec![0];
                    key.extend(entry.seq.to_be_bytes());
                    TranscriptRepository::put_on(
                        c,
                        SENT,
                        entry.conversation,
                        &key,
                        entry.wall_clock,
                        entry.seq,
                        "",
                        &entry.plaintext,
                        false,
                        false,
                    )?;
                    Ok(())
                },
            )?;
            drop(writer);
            let (mut concurrent, _) =
                EncryptedRecordLog::<SentEntry>::open(&path, password, b"MTSENT")?;
            concurrent.append_durable(&sent(2))?;
            drop(concurrent);
            changed_bytes = std::fs::read(&path)?;
            Ok(())
        },
    );
    assert!(
        result.is_err(),
        "an appended source record was published without being imported"
    );
    assert_eq!(std::fs::read(&path).unwrap(), changed_bytes);
    let count = owner
        .database()
        .read(|c| {
            Ok(
                c.query_row("SELECT count(*) FROM transcript_rows", [], |row| {
                    row.get::<_, i64>(0)
                })?,
            )
        })
        .unwrap();
    assert_eq!(count, 0);
}

#[test]
fn interrupted_snapshot_import_preserves_original_torn_tail_until_publication() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("sent.log");
    let (mut legacy, _) = EncryptedRecordLog::<SentEntry>::open(&path, "pw", b"MTSENT").unwrap();
    legacy.append_durable(&sent(1)).unwrap();
    drop(legacy);
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    file.write_all(&[0, 0, 4, 0, 1, 2]).unwrap();
    file.sync_all().unwrap();
    drop(file);
    let original = std::fs::read(&path).unwrap();
    publication::fail_at(Some("transcript-imported"));
    assert!(SentStore::from_profile(owner.clone(), &path, "pw").is_err());
    publication::fail_at(None);
    assert_eq!(std::fs::read(&path).unwrap(), original);
    let reopened = SentStore::from_profile(owner, &path, "pw").unwrap();
    assert_eq!(reopened.count(None).unwrap(), 1);
    assert_eq!(reopened.entry(&conv(1), 1).unwrap(), Some(sent(1)));
}

#[test]
fn interrupted_fresh_import_keeps_absent_source_and_restarts_without_replay() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("sent.log");
    publication::fail_at(Some("transcript-imported"));
    assert!(SentStore::from_profile(owner.clone(), &path, "pw").is_err());
    publication::fail_at(None);
    assert!(!path.exists());
    crate::storage::record_log::frame_observations::reset();
    let reopened = SentStore::from_profile(owner, &path, "pw").unwrap();
    assert_eq!(reopened.count(None).unwrap(), 0);
    assert_eq!(
        crate::storage::record_log::frame_observations::maximum_capacity(),
        0
    );
    assert!(SentLog::open(&path, "pw").is_err());
}

#[test]
fn preflight_authenticates_without_publication_and_warm_reads_no_frames() {
    use publication::PublishedHistorySource;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sent.log");
    let mut legacy = SentLog::open(&path, "pw").unwrap();
    legacy.record_durable(&sent(1)).unwrap();
    drop(legacy);
    let original = std::fs::read(&path).unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = ProfileStorage::open_participant(
        ProfileStorageLease::acquire(dir.path()).unwrap(),
        &identity.public(),
        &account.public(),
        &dir.path().join("messages.log"),
        &path,
        "pw",
    )
    .unwrap();
    assert!(owner.participant_database().is_none());
    assert!(
        PublishedHistorySource::authenticate_legacy_with::<SentEntry>(
            &owner,
            &path,
            "pw",
            SENT,
            b"MTSENT",
            |_| Err(invalid("rejected semantic source"))
        )
        .is_err()
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);

    assert!(
        PublishedHistorySource::authenticate_legacy::<SentEntry>(
            &owner, &path, "wrong", SENT, b"MTSENT"
        )
        .is_err()
    );
    PublishedHistorySource::authenticate_legacy::<SentEntry>(&owner, &path, "pw", SENT, b"MTSENT")
        .unwrap();
    assert!(owner.participant_database().is_none());
    assert_eq!(std::fs::read(&path).unwrap(), original);
    owner.complete_participant("pw", None).unwrap();
    let store = SentStore::from_profile(owner.clone(), &path, "pw").unwrap();
    drop(store);
    crate::storage::record_log::frame_observations::reset();
    PublishedHistorySource::authenticate_legacy::<SentEntry>(&owner, &path, "pw", SENT, b"MTSENT")
        .unwrap();
    assert_eq!(
        crate::storage::record_log::frame_observations::maximum_capacity(),
        0
    );
    std::fs::write(&path, original).unwrap();
    assert!(
        PublishedHistorySource::authenticate_legacy::<SentEntry>(
            &owner, &path, "pw", SENT, b"MTSENT"
        )
        .is_err()
    );
}

#[test]
fn runtime_metadata_account_routes_use_first_envelope_metadata_and_zero_warm_body_reads() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("sent.log");
    let mut store = SentStore::from_profile(owner.clone(), &path, "pw").unwrap();
    for n in 0..257 {
        store.record_durable(&sent(n)).unwrap();
    }
    let own = account.account_id();
    let peer = "a".repeat(32);
    let mut row = sent(300);
    row.plaintext =
        crate::node::DmEnvelope::new(own.clone(), peer.clone(), [19; 32], b"body".to_vec())
            .encode();
    store.record_durable(&row).unwrap();
    let mut later = sent(301);
    later.plaintext =
        crate::node::DmEnvelope::new(own.clone(), "b".repeat(32), [20; 32], b"later".to_vec())
            .encode();
    store.record_durable(&later).unwrap();
    let routes = store.account_routes_page(None, 128).unwrap();
    assert_eq!(routes.len(), 1);
    assert_eq!(routes[0].1.as_ref().unwrap().recipient_account, peer);
    assert_eq!(store.body_reads(), 0);
    drop(store);
    drop(owner);
    let owner = profile(dir.path(), &identity, &account);
    let mut store = SentStore::from_profile(owner, &path, "pw").unwrap();
    assert_eq!(store.account_routes_page(None, 128).unwrap(), routes);
    assert_eq!(store.body_reads(), 0);
    let first = store
        .header_scan_page(None, Some(257), TranscriptPageLimit::default())
        .unwrap()
        .rows[0]
        .ordinal;
    assert_eq!(store.remove_ordinals(&[first]).unwrap(), 1);
    let promoted = store.account_routes_page(None, 128).unwrap();
    assert_eq!(
        promoted[0].1.as_ref().unwrap().recipient_account,
        "b".repeat(32)
    );
    let mut cursor = None;
    loop {
        let page = store
            .header_scan_page(None, cursor, TranscriptPageLimit::default())
            .unwrap();
        if page.rows.is_empty() {
            break;
        }
        let ordinals: Vec<_> = page
            .rows
            .iter()
            .filter(|h| h.logical_id.is_some())
            .map(|h| h.ordinal)
            .collect();
        cursor = page.rows.last().map(|h| h.ordinal);
        store.remove_ordinals(&ordinals).unwrap();
    }
    assert!(store.account_routes_page(None, 128).unwrap().is_empty());
    assert_eq!(store.body_reads(), 0);
}

#[test]
fn runtime_metadata_suppressed_receive_installs_exact_evidence_without_a_body() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let mut rows =
        ReceivedStore::from_profile(owner.clone(), &dir.path().join("received.log"), "pw").unwrap();
    let mut suppression =
        crate::node::account_suppression::IndexedAccountSuppression::from_profile(
            owner,
            &dir.path().join("account-suppression.log"),
            "pw",
        )
        .unwrap();
    let row = received(71);
    suppression
        .suppress(row.conversation, row.event_id)
        .unwrap();
    rows.record_durable(&row).unwrap();
    assert!(!rows.contains(row.event_id).unwrap());
    assert!(
        rows.processed(row.event_id).unwrap(),
        "an erased signed event must leave exact processing evidence"
    );
    assert_eq!(rows.revision(), 0);
    assert_eq!(rows.body_reads(), 0);
}

#[test]
fn channel_consumed_evidence_is_atomic_and_survives_restart_without_body() {
    let dir = tempfile::tempdir().unwrap();
    let identity = DeviceIdentity::generate();
    let account = Account::generate();
    let owner = profile(dir.path(), &identity, &account);
    let path = dir.path().join("received.log");
    let store = ReceivedStore::from_profile(owner.clone(), &path, "pw").unwrap();
    let id = EventId::new([91; 32]);
    let host = conv(9);
    let result: Result<(), LogError> = owner.database().transaction(|c| {
        ReceivedStore::install_evidence_on(c, host, id)?;
        Err(invalid("injected receiver rollback"))
    });
    assert!(result.is_err());
    assert!(!store.processed(id).unwrap());
    owner
        .database()
        .transaction(|c| ReceivedStore::install_evidence_on(c, host, id))
        .unwrap();
    assert!(store.processed(id).unwrap());
    assert_eq!(store.usage().unwrap().records, 0);
    drop(store);
    drop(owner);
    let store =
        ReceivedStore::from_profile(profile(dir.path(), &identity, &account), &path, "pw").unwrap();
    assert!(store.processed(id).unwrap());
    assert_eq!(store.usage().unwrap().records, 0);
    assert_eq!(store.body_reads(), 0);
}
