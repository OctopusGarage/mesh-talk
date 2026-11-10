use super::*;

async fn copy_controls(source: &Arc<Node>, target: &Arc<Node>) {
    let page = source
        .account_documents(AccountDocumentDomain::GroupControl, None, 64)
        .unwrap();
    for meta in page.items {
        let document = source.account_document(&meta.key).unwrap().unwrap();
        target
            .commit_imported_document_for_test(
                meta,
                document.payload().to_vec(),
                target.device_sync_policy_snapshot().1,
                source.identity.public(),
            )
            .await
            .unwrap();
    }
}

fn direct(own: &str, peer: &str, id: u8) -> HistoryRecord {
    let envelope = DmEnvelope::new(peer.into(), own.into(), [id; 32], b"retained text".to_vec());
    HistoryRecord::Direct(BackfillRecord {
        conv: *account_conversation_id(own, peer).as_bytes(),
        from: peer.into(),
        wall_clock: 1,
        plaintext: envelope.encode(),
        event_id: [id; 32],
    })
}

#[tokio::test]
async fn buffered_archive_page_cannot_acquire_permission_from_a_later_cleanup_ack() {
    let (_dir, source, target) = super::selection_tests::sibling_nodes();
    let peer = "a".repeat(32);
    let other = "b".repeat(32);
    let own = target.account_id();
    target
        .delete_account_contact_if(&own, &peer, true, |commit| commit())
        .unwrap();
    // The request predates the source's cleanup; its response can already be
    // buffered while the ACK arrives on an independent document connection.
    let old_cut = target.capture_personal_history_cut(&source.identity.public());
    let buffered_page = vec![direct(&own, &peer, 71), direct(&own, &other, 72)];
    copy_controls(&target, &source).await;
    source.retry_conversation_personal_cleanup().unwrap();
    copy_controls(&source, &target).await;
    let proof = source.own_group_proof();
    target
        .apply_account_history_page_from_sibling(
            &buffered_page,
            AccountSyncVersion::V2,
            Some(target.device_sync_policy_snapshot().1),
            Some(&proof),
            Some(&old_cut),
        )
        .unwrap();
    assert!(target.account_history(&peer, 10).unwrap().is_empty());
    assert_eq!(target.account_history(&other, 10).unwrap().len(), 1);
    // A new request starts with the exact applied-source proof and can accept
    // genuinely new content without reopening any old buffered request.
    let fresh_cut = target.capture_personal_history_cut(&source.identity.public());
    target
        .apply_account_history_page_from_sibling(
            &[direct(&own, &peer, 73)],
            AccountSyncVersion::V2,
            Some(target.device_sync_policy_snapshot().1),
            Some(&proof),
            Some(&fresh_cut),
        )
        .unwrap();
    let rows = target.account_history(&peer, 10).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].id, EventId::new([73; 32]));
}
