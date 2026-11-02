//! Public directory observations remain descriptive and source authenticated.
use super::*;
use crate::{
    channel::{ChannelMeta, new_channel_id},
    eventlog::{Event, EventKind},
    node::account_devices::AccountDevice,
};

#[test]
fn group_inventory_directory_cold_preparation_does_not_hydrate_retained_messages() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let external = node(&directory.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&external, &source, "Bounded directory");
    let mut parents = source.log.lock().unwrap().heads(&group).unwrap();
    for sequence in 2..=602 {
        let event = Event::new(
            &source.identity,
            group,
            sequence,
            parents,
            sequence,
            sequence,
            EventKind::Message,
            vec![7; 2048],
        );
        parents = vec![event.id];
        source.log.lock().unwrap().append(event).unwrap();
    }
    // Settle canonical authority before measuring the independent directory
    // selector: a directory read must not hydrate historical message bodies.
    for turn in 0..5000 {
        let log = source.log.lock().unwrap();
        let repository = log.repository().unwrap();
        let canonical = source
            .channel_checkpoints
            .project_step(repository, &source.identity, group)
            .unwrap();
        if canonical {
            source
                .channel_checkpoints
                .prepare_presentation_step(
                    repository,
                    &source.identity,
                    group,
                    crate::eventlog::sync::network::PreparationBudget::default(),
                )
                .unwrap();
            if source
                .channel_checkpoints
                .prepared_history_policy(group)
                .is_ok()
            {
                break;
            }
        }
        assert!(turn < 4999, "finite canonical fixture did not settle");
    }
    let before = source
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    source.publish_group_inventory_for(group);
    assert_eq!(
        source
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        before,
        "cold directory publication decoded retained signed message bodies"
    );
}

#[test]
fn group_inventory_causal_directory_ignores_another_sources_future_revision() {
    for stale_first in [false, true] {
        let directory = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let a = node(&directory.path().join("a"), &account);
        let b = node(&directory.path().join("b"), &account);
        let owner = node(&directory.path().join("owner"), &Account::generate());
        let requester = node(&directory.path().join("requester"), &account);
        for (source, sibling) in [(&a, &b), (&b, &a)] {
            source
                .account_devices
                .lock()
                .unwrap()
                .insert(AccountDevice {
                    public: sibling.identity.public(),
                    certificate: account.certify(&sibling.identity.public().ed25519_pub),
                })
                .unwrap();
        }
        let group = new_channel_id();
        let original_members = vec![
            owner.identity.public(),
            a.identity.public(),
            b.identity.public(),
        ];
        let initial = Event::new(
            &owner.identity,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "nameOld".into(),
                members: original_members.clone(),
                epoch: 0,
                owner: owner.user_id(),
            }
            .encode(),
        );
        a.log.lock().unwrap().append(initial.clone()).unwrap();
        a.process_channel(group);
        let stale = group_inventory_source_document(&a, group);
        let stale_meta = AccountDocumentMeta::new(
            &account,
            a.user_id(),
            stale.meta.key.clone(),
            9_007_199_254_740_991,
            stale.payload(),
            false,
        );
        let mut fresh_members = original_members;
        fresh_members.push(DeviceIdentity::generate().public());
        let membership = Event::new(
            &owner.identity,
            group,
            2,
            vec![initial.id],
            2,
            2,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "nameOld".into(),
                members: fresh_members,
                epoch: 1,
                owner: owner.user_id(),
            }
            .encode(),
        );
        let rename = Event::new(
            &owner.identity,
            group,
            3,
            vec![membership.id],
            3,
            3,
            EventKind::ChannelRename,
            b"nameNew".to_vec(),
        );
        for event in [initial, membership, rename] {
            b.log.lock().unwrap().append(event).unwrap();
        }
        b.process_channel(group);
        let fresh = group_inventory_source_document(&b, group);
        let fresh_reference: GroupInventoryReference =
            serde_json::from_slice(fresh.payload()).unwrap();
        assert_eq!(fresh_reference.name, "nameNew");
        assert_eq!(fresh_reference.member_count, 3);
        let observations = if stale_first {
            vec![
                (stale_meta, stale.payload()),
                (fresh.meta.clone(), fresh.payload()),
            ]
        } else {
            vec![
                (fresh.meta.clone(), fresh.payload()),
                (stale_meta, stale.payload()),
            ]
        };
        for (meta, payload) in observations {
            requester
                .account_documents
                .lock()
                .unwrap()
                .merge(meta, payload)
                .unwrap();
        }
        let reference = requester.channel_shared_reference(group).unwrap();
        assert_eq!(
            reference.name, "nameNew",
            "a different source's future document clock cannot hide a real public rename"
        );
        assert_eq!(reference.member_count, 3);
        assert_eq!(reference.owner, owner.user_id());
        let row = requester
            .list_channels()
            .unwrap()
            .into_iter()
            .find(|row| row.id == group)
            .unwrap();
        assert_eq!(
            (row.name, row.member_count, row.owner),
            (reference.name, reference.member_count, reference.owner)
        );
        assert!(!requester.channel_can_send(group).unwrap());
        assert!(!requester.channel_can_manage(group).unwrap());
        assert!(requester.channel_members(group).is_empty());
        assert!(
            requester
                .log
                .lock()
                .unwrap()
                .events(&group)
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn group_inventory_directory_requires_actual_source_device_signature() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let requester = node(&directory.path().join("requester"), &account);
    let owner = node(&directory.path().join("owner"), &Account::generate());
    let group = group_inventory_legacy_fixture(&owner, &source, "Source-authenticated name");
    let original = group_inventory_source_document(&source, group);
    let mut forged: serde_json::Value = serde_json::from_slice(original.payload()).unwrap();
    forged["name"] = serde_json::json!("Forged by a sibling holding the shared account key");
    let payload = serde_json::to_vec(&forged).unwrap();
    let meta = AccountDocumentMeta::new(
        &account,
        source.user_id(),
        original.meta.key.clone(),
        original.meta.revision + 1,
        &payload,
        false,
    );
    assert!(
        requester
            .account_documents
            .lock()
            .unwrap()
            .merge(meta, &payload)
            .is_err(),
        "the shared account signature cannot impersonate the actual directory source"
    );
    assert!(requester.channel_shared_reference(group).is_none());
}

#[test]
fn group_inventory_directory_keeps_v4_descriptive_payloads_readable() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let requester = node(&directory.path().join("requester"), &account);
    let owner = node(&directory.path().join("owner"), &Account::generate());
    let group = group_inventory_legacy_fixture(&owner, &source, "Retained version-four name");
    let original = group_inventory_source_document(&source, group);
    let mut legacy: serde_json::Value = serde_json::from_slice(original.payload()).unwrap();
    legacy.as_object_mut().unwrap().remove("directory");
    let payload = serde_json::to_vec(&legacy).unwrap();
    let meta = AccountDocumentMeta::new(
        &account,
        source.user_id(),
        original.meta.key.clone(),
        original.meta.revision + 1,
        &payload,
        false,
    );
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(meta, &payload)
        .unwrap();
    assert_eq!(
        requester.channel_shared_reference(group).unwrap().name,
        "Retained version-four name"
    );
    assert!(!requester.channel_can_send(group).unwrap());
}

#[test]
fn group_inventory_directory_restored_public_prefix_cannot_replace_its_own_newer_source() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let external = node(&directory.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&source, &external, "Before restore");
    copy_public_group(&external, &source, group);
    let secrets = source.identity.secret_bytes();
    let restored = node_with_identity(
        &directory.path().join("restored"),
        &account,
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
    );
    copy_public_group(&source, &restored, group);
    source
        .append_event(
            group,
            EventKind::ChannelRename,
            b"Actual later name".to_vec(),
        )
        .unwrap();
    source.process_channel(group);
    let current = group_inventory_source_document(&source, group);
    restored
        .account_documents
        .lock()
        .unwrap()
        .merge(current.meta.clone(), current.payload())
        .unwrap();
    let retained = group_inventory_source_document(&restored, group);
    assert_eq!(
        retained.meta, current.meta,
        "a restored public prefix cannot overwrite a later observation by the same source"
    );
    assert_eq!(retained.payload(), current.payload());
}

#[test]
fn group_inventory_directory_signed_source_cannot_downgrade_to_legacy_payload() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let requester = node(&directory.path().join("requester"), &account);
    let owner = node(&directory.path().join("owner"), &Account::generate());
    let group = group_inventory_legacy_fixture(&owner, &source, "Signed source floor");
    let signed = group_inventory_source_document(&source, group);
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(signed.meta.clone(), signed.payload())
        .unwrap();
    let mut legacy: serde_json::Value = serde_json::from_slice(signed.payload()).unwrap();
    legacy.as_object_mut().unwrap().remove("directory");
    legacy["name"] = serde_json::json!("Unsigned downgrade impersonating the source");
    let payload = serde_json::to_vec(&legacy).unwrap();
    let meta = AccountDocumentMeta::new(
        &account,
        source.user_id(),
        signed.meta.key.clone(),
        signed.meta.revision + 1,
        &payload,
        false,
    );
    assert!(
        requester
            .account_documents
            .lock()
            .unwrap()
            .merge(meta, &payload)
            .is_err(),
        "v4 compatibility cannot erase an already observed actual-device signature"
    );
    assert_eq!(
        requester.channel_shared_reference(group).unwrap().name,
        "Signed source floor"
    );
}

#[test]
fn group_inventory_directory_normal_control_history_growth_stays_available() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let requester = node(&directory.path().join("requester"), &account);
    let external = node(&directory.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&source, &external, "Long-lived directory");
    copy_public_group(&external, &source, group);
    let mut parent = source.log.lock().unwrap().heads(&group).unwrap()[0];
    for seq in 2..=4098 {
        let event = Event::new(
            &source.identity,
            group,
            seq,
            vec![parent],
            seq,
            seq,
            EventKind::ChannelRename,
            format!("Directory revision {seq}").into_bytes(),
        );
        parent = event.id;
        source.log.lock().unwrap().append(event).unwrap();
    }
    source.process_channel(group);
    let current = group_inventory_source_document(&source, group);
    let body: serde_json::Value = serde_json::from_slice(current.payload()).unwrap();
    assert_eq!(
        body["directory"]["unavailable"], false,
        "normal history growth is not resource uncertainty"
    );
    assert!(current.payload().len() <= 128 * 1024);
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(current.meta.clone(), current.payload())
        .unwrap();
    let reference = requester.channel_shared_reference(group).unwrap();
    assert_eq!(reference.name, "Directory revision 4098");
    assert!(!reference.directory_reconciling);
    assert_eq!(
        group_inventory_source_document(&source, group).meta,
        current.meta
    );
}

#[test]
fn group_inventory_directory_wide_frontier_receipt_is_atomic_and_recovers_after_merge() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source_path = directory.path().join("source");
    let source = node(&source_path, &account);
    let external = node(&directory.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&source, &external, "Before wide frontier");
    copy_public_group(&external, &source, group);
    let previous = group_inventory_source_document(&source, group);
    let parents = source.log.lock().unwrap().heads(&group).unwrap();
    for seq in 2..=602 {
        source
            .log
            .lock()
            .unwrap()
            .append(Event::new(
                &source.identity,
                group,
                seq,
                parents.clone(),
                2,
                seq,
                EventKind::ChannelRename,
                format!("Wide branch {seq}").into_bytes(),
            ))
            .unwrap();
    }
    source.process_channel(group);
    source
        .account_documents
        .lock()
        .unwrap()
        .publication_budget_for_test(0);
    let rejected = group_inventory_source_document(&source, group);
    assert_eq!(
        rejected.meta, previous.meta,
        "insufficient atomic receipt budget must leave the prior committed source untouched"
    );
    source
        .account_documents
        .lock()
        .unwrap()
        .publication_budget_for_test(usize::MAX);
    let wide = group_inventory_source_document(&source, group);
    assert!(wide.meta.revision > previous.meta.revision);
    assert!(
        source
            .account_documents
            .lock()
            .unwrap()
            .local_publication_frontier(&wide.meta, &source.identity.public())
            .is_some_and(|frontier| frontier.len() > 512),
        "incomplete wire witnesses need an exact atomic local publication receipt"
    );
    let secrets = source.identity.secret_bytes();
    drop(source);
    let source = node_with_identity(
        &source_path,
        &account,
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
    );
    source
        .append_event(
            group,
            EventKind::ChannelRename,
            b"Merged current name".to_vec(),
        )
        .unwrap();
    source.process_channel(group);
    source
        .account_documents
        .lock()
        .unwrap()
        .publication_budget_for_test(0);
    let merged = group_inventory_source_document(&source, group);
    assert!(merged.meta.revision > wide.meta.revision);
    let body: serde_json::Value = serde_json::from_slice(merged.payload()).unwrap();
    assert_eq!(body["directory"]["unavailable"], false);
    assert_eq!(body["name"], "Merged current name");
    assert!(
        source
            .account_documents
            .lock()
            .unwrap()
            .local_publication_frontier(&merged.meta, &source.identity.public())
            .is_none(),
        "a complete replacement releases its old receipt atomically even at zero spare budget"
    );
}

#[test]
fn group_inventory_directory_mutual_acknowledgements_reach_a_fixed_point() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let a = node(&directory.path().join("a"), &account);
    let b = node(&directory.path().join("b"), &account);
    let reference_only = node(&directory.path().join("reference"), &account);
    for (source, sibling) in [(&a, &b), (&b, &a)] {
        source
            .account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: sibling.identity.public(),
                certificate: account.certify(&sibling.identity.public().ed25519_pub),
            })
            .unwrap();
        source.roster.lock().unwrap().update(
            &sibling.own_group_proof(),
            "127.0.0.1".parse().unwrap(),
            &source.user_id(),
        );
    }
    let group = group_inventory_legacy_fixture(&a, &b, "Stable shared directory");
    copy_public_group(&b, &a, group);
    let exchange = || {
        let first = group_inventory_source_document(&a, group);
        let second = group_inventory_source_document(&b, group);
        a.account_documents
            .lock()
            .unwrap()
            .merge(second.meta.clone(), second.payload())
            .unwrap();
        b.account_documents
            .lock()
            .unwrap()
            .merge(first.meta.clone(), first.payload())
            .unwrap();
    };
    // Give each authenticated source the other's complete, unchanged public
    // observation before measuring convergence. No membership or policy changes.
    for _ in 0..3 {
        exchange();
    }
    let first = group_inventory_source_document(&a, group);
    let second = group_inventory_source_document(&b, group);
    // Synchronize the final observed heads so both digests cover the same keys.
    a.account_documents
        .lock()
        .unwrap()
        .merge(second.meta.clone(), second.payload())
        .unwrap();
    b.account_documents
        .lock()
        .unwrap()
        .merge(first.meta.clone(), first.payload())
        .unwrap();
    let first_digest = a
        .account_documents
        .lock()
        .unwrap()
        .digest(AccountDocumentDomain::GroupInventory);
    let second_digest = b
        .account_documents
        .lock()
        .unwrap()
        .digest(AccountDocumentDomain::GroupInventory);
    for _ in 0..6 {
        exchange();
        for document in [&first, &second] {
            reference_only
                .account_documents
                .lock()
                .unwrap()
                .merge(document.meta.clone(), document.payload())
                .unwrap();
        }
        // A new sibling with no public causal cut consumes the directory but
        // must not manufacture another source or acknowledgements of its own.
        reference_only.publish_group_inventory_for(group);
        assert!(reference_only.channel_shared_reference(group).is_some());
        assert!(
            reference_only
                .account_documents
                .lock()
                .unwrap()
                .meta(&AccountDocumentKey::GroupInventory {
                    channel: hex::encode(group.as_bytes()),
                    source_device: reference_only.user_id(),
                })
                .is_none()
        );
        assert_eq!(
            group_inventory_source_document(&a, group).meta,
            first.meta,
            "ordinary acknowledgement exchange must not create another A publication"
        );
        assert_eq!(
            group_inventory_source_document(&b, group).meta,
            second.meta,
            "ordinary acknowledgement exchange must not create another B publication"
        );
        assert_eq!(
            a.account_documents
                .lock()
                .unwrap()
                .digest(AccountDocumentDomain::GroupInventory),
            first_digest
        );
        assert_eq!(
            b.account_documents
                .lock()
                .unwrap()
                .digest(AccountDocumentDomain::GroupInventory),
            second_digest
        );
    }
}

#[test]
fn group_inventory_directory_private_history_growth_reaches_a_fixed_point() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let external = node(&directory.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&source, &external, "Stable directory");
    copy_public_group(&external, &source, group);
    let previous = group_inventory_source_document(&source, group);
    for kind in [EventKind::Message, EventKind::React, EventKind::Profile] {
        source
            .append_event(
                group,
                kind,
                b"private ciphertext must not enter directory metadata".to_vec(),
            )
            .unwrap();
        source.process_channel(group);
        assert_eq!(
            group_inventory_source_document(&source, group).meta,
            previous.meta
        );
    }
    assert_eq!(
        group_inventory_source_document(&source, group).payload(),
        previous.payload()
    );
}

#[test]
fn group_inventory_directory_uses_full_reducer_when_local_book_is_delayed() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let external = node(&directory.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&source, &external, "Delayed local book");
    copy_public_group(&external, &source, group);
    source
        .append_event(
            group,
            EventKind::ChannelRename,
            b"Actual full reducer name".to_vec(),
        )
        .unwrap();
    let current = group_inventory_source_document(&source, group);
    let reference: GroupInventoryReference = serde_json::from_slice(current.payload()).unwrap();
    assert_eq!(
        reference.name, "Actual full reducer name",
        "the source cannot sign old cached facts with a newer public-control witness"
    );
}

#[test]
fn group_inventory_directory_large_binding_envelope_publishes_bounded_invalidation() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let requester = node(&directory.path().join("requester"), &account);
    let group = new_channel_id();
    let mut members = vec![source.identity.public()];
    for _ in 0..255 {
        let identity = DeviceIdentity::generate();
        let proof = crate::discovery::Announce::new(&identity, "N".repeat(512), 1);
        source.roster.lock().unwrap().update(
            &proof,
            "127.0.0.1".parse().unwrap(),
            &source.user_id(),
        );
        members.push(identity.public());
    }
    let genesis = Event::new(
        &source.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Large fixed binding envelope".into(),
            members,
            epoch: 0,
            owner: source.user_id(),
        }
        .encode(),
    );
    source.log.lock().unwrap().append(genesis).unwrap();
    source.process_channel(group);
    let _ = group_inventory_source_document(&source, group);
    let key = AccountDocumentKey::GroupInventory {
        channel: hex::encode(group.as_bytes()),
        source_device: source.user_id(),
    };
    let current = source.account_document(&key).unwrap();
    assert!(
        current.is_some(),
        "an oversized fixed envelope must publish a valid bounded invalidation rather than silently retaining an old source"
    );
    let current = current.unwrap();
    let body: serde_json::Value = serde_json::from_slice(current.payload()).unwrap();
    assert_eq!(body["directory"]["unavailable"], true);
    assert!(current.payload().len() <= 128 * 1024);
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(current.meta.clone(), current.payload())
        .unwrap();
    let reference = requester.channel_shared_reference(group).unwrap();
    assert!(reference.directory_reconciling);
    assert!(
        reference
            .members
            .iter()
            .any(|member| member.public == source.identity.public()),
        "a bounded source invalidation retains the actual observer and owner"
    );
}

#[test]
fn group_inventory_directory_conflicts_reconcile_and_exact_ack_never_hides_a_new_head() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let a = node(&directory.path().join("a"), &account);
    let b = node(&directory.path().join("b"), &account);
    let requester = node(&directory.path().join("requester"), &account);
    let group = group_inventory_legacy_fixture(&a, &b, "Common root");
    copy_public_group(&b, &a, group);
    let root = a.log.lock().unwrap().heads(&group).unwrap()[0];
    for (target, sequence, name) in [(&a, 2, "Branch A"), (&b, 3, "Branch B")] {
        target
            .log
            .lock()
            .unwrap()
            .append(Event::new(
                &a.identity,
                group,
                sequence,
                vec![root],
                2,
                sequence,
                EventKind::ChannelRename,
                name.as_bytes().to_vec(),
            ))
            .unwrap();
        target.process_channel(group);
    }
    let first_a = group_inventory_source_document(&a, group);
    let first_b = group_inventory_source_document(&b, group);
    for document in [&first_a, &first_b] {
        requester
            .account_documents
            .lock()
            .unwrap()
            .merge(document.meta.clone(), document.payload())
            .unwrap();
    }
    let reference = requester.channel_shared_reference(group).unwrap();
    assert!(
        reference.directory_reconciling,
        "incomparable real rename observations must not be presented as agreed current truth"
    );
    assert!(matches!(reference.name.as_str(), "Branch A" | "Branch B"));
    copy_public_group(&a, &b, group);
    b.account_documents
        .lock()
        .unwrap()
        .merge(first_a.meta.clone(), first_a.payload())
        .unwrap();
    let merged_b = group_inventory_source_document(&b, group);
    let body: serde_json::Value = serde_json::from_slice(merged_b.payload()).unwrap();
    assert!(
        body["directory"]["acknowledgements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|ack| ack["source"] == a.user_id()
                && ack["hash"] == hex::encode(first_a.meta.content_hash))
    );
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(merged_b.meta.clone(), merged_b.payload())
        .unwrap();
    assert!(
        !requester
            .channel_shared_reference(group)
            .unwrap()
            .directory_reconciling
    );
    copy_public_group(&b, &a, group);
    a.append_event(
        group,
        EventKind::ChannelRename,
        b"Later actual head".to_vec(),
    )
    .unwrap();
    a.process_channel(group);
    let later_a = group_inventory_source_document(&a, group);
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(later_a.meta.clone(), later_a.payload())
        .unwrap();
    assert!(
        !requester
            .account_documents
            .lock()
            .unwrap()
            .merge(first_a.meta.clone(), first_a.payload())
            .unwrap()
    );
    let reference = requester.channel_shared_reference(group).unwrap();
    assert_eq!(
        reference.name, "Later actual head",
        "an ACK of A1 cannot suppress the independently newer A2 source"
    );
    assert!(!reference.directory_reconciling);
}

#[test]
fn group_inventory_directory_tombstone_cannot_erase_a_signed_source_floor() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let requester_path = directory.path().join("requester");
    let requester = node(&requester_path, &account);
    let external = node(&directory.path().join("external"), &Account::generate());
    let group =
        group_inventory_legacy_fixture(&external, &source, "Authenticated before retraction");
    let signed = group_inventory_source_document(&source, group);
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(signed.meta.clone(), signed.payload())
        .unwrap();
    let tombstone = AccountDocumentMeta::new(
        &account,
        source.user_id(),
        signed.meta.key.clone(),
        signed.meta.revision + 1,
        &[],
        true,
    );
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(tombstone, &[])
        .unwrap();
    let secret = requester.identity.secret_bytes();
    drop(requester);
    let requester = node_with_identity(
        &requester_path,
        &account,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
    );
    let mut legacy: serde_json::Value = serde_json::from_slice(signed.payload()).unwrap();
    legacy.as_object_mut().unwrap().remove("directory");
    legacy["name"] = serde_json::json!("Unsigned resurrection after tombstone");
    let payload = serde_json::to_vec(&legacy).unwrap();
    let meta = AccountDocumentMeta::new(
        &account,
        source.user_id(),
        signed.meta.key.clone(),
        signed.meta.revision + 2,
        &payload,
        false,
    );
    assert!(
        requester
            .account_documents
            .lock()
            .unwrap()
            .merge(meta, &payload)
            .is_err(),
        "a retained tombstone must not launder an authenticated source into unsigned v4 metadata after restart"
    );
    assert!(requester.channel_shared_reference(group).is_none());
}

#[test]
fn group_inventory_directory_known_unavailable_source_can_be_acknowledged_by_another_source() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let a = node(&directory.path().join("a"), &account);
    let b = node(&directory.path().join("b"), &account);
    let requester = node(&directory.path().join("requester"), &account);
    for (target, sibling) in [(&a, &b), (&b, &a)] {
        target
            .account_devices
            .lock()
            .unwrap()
            .insert(AccountDevice {
                public: sibling.identity.public(),
                certificate: account.certify(&sibling.identity.public().ed25519_pub),
            })
            .unwrap();
    }
    let group = new_channel_id();
    let mut members = vec![a.identity.public(), b.identity.public()];
    for _ in 0..254 {
        let device = DeviceIdentity::generate();
        let proof = crate::discovery::Announce::new(&device, "N".repeat(512), 1);
        a.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &a.user_id());
        members.push(device.public());
    }
    a.log
        .lock()
        .unwrap()
        .append(Event::new(
            &a.identity,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Known public directory".into(),
                members,
                epoch: 0,
                owner: a.user_id(),
            }
            .encode(),
        ))
        .unwrap();
    a.process_channel(group);
    let unavailable = group_inventory_source_document(&a, group);
    let body: serde_json::Value = serde_json::from_slice(unavailable.payload()).unwrap();
    assert_eq!(body["directory"]["unavailable"], true);
    copy_public_group(&a, &b, group);
    b.account_documents
        .lock()
        .unwrap()
        .merge(unavailable.meta.clone(), unavailable.payload())
        .unwrap();
    let available = group_inventory_source_document(&b, group);
    let body: serde_json::Value = serde_json::from_slice(available.payload()).unwrap();
    assert_eq!(body["directory"]["unavailable"], false);
    assert!(
        body["directory"]["acknowledgements"]
            .as_array()
            .unwrap()
            .iter()
            .any(|ack| ack["source"] == a.user_id()
                && ack["hash"] == hex::encode(unavailable.meta.content_hash)),
        "a real source with the complete actual public frontier can supersede a resource-limited observation"
    );
    for document in [&unavailable, &available] {
        requester
            .account_documents
            .lock()
            .unwrap()
            .merge(document.meta.clone(), document.payload())
            .unwrap();
    }
    assert!(
        !requester
            .channel_shared_reference(group)
            .unwrap()
            .directory_reconciling
    );
    // Resource-limited sources clear their acknowledgements rather than
    // reciprocating an available source's exact acknowledgement forever.
    a.account_documents
        .lock()
        .unwrap()
        .merge(available.meta.clone(), available.payload())
        .unwrap();
    for _ in 0..3 {
        let next_unavailable = group_inventory_source_document(&a, group);
        assert_eq!(next_unavailable.meta, unavailable.meta);
        b.account_documents
            .lock()
            .unwrap()
            .merge(next_unavailable.meta.clone(), next_unavailable.payload())
            .unwrap();
        let next_available = group_inventory_source_document(&b, group);
        assert_eq!(next_available.meta, available.meta);
        a.account_documents
            .lock()
            .unwrap()
            .merge(next_available.meta.clone(), next_available.payload())
            .unwrap();
    }
}

#[test]
fn group_inventory_directory_current_verified_binding_retires_the_previous_account_source() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let requester_path = directory.path().join("requester");
    let requester = node(&requester_path, &account);
    let external = node(&directory.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&external, &source, "Previous account source");
    let signed = group_inventory_source_document(&source, group);
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(signed.meta.clone(), signed.payload())
        .unwrap();
    requester
        .configure_privacy(
            &requester_path,
            "pw",
            &requester.signed_announce("Requester", 1),
            std::sync::Arc::new(crate::discovery::DiscoveryVisibility::default()),
        )
        .unwrap();
    let old = source.signed_announce("Before adoption", 1);
    let current_account = Account::generate();
    let current = crate::discovery::Announce::new_with_account(
        &source.identity,
        &current_account,
        "After verified adoption",
        1,
    );
    {
        let mut privacy = requester.privacy.state.write().unwrap();
        let proofs = &mut privacy.as_mut().unwrap().proofs;
        proofs.record(&old).unwrap();
        proofs.record_explicit_account(&current).unwrap();
    }
    assert!(
        requester.channel_shared_reference(group).is_none(),
        "the previous account's directory observation cannot outlive a current independently verified binding"
    );
}

#[test]
fn group_inventory_directory_adoption_deduplicates_certified_current_account_members() {
    let directory = tempfile::tempdir().unwrap();
    let previous = Account::generate();
    let current = Account::generate();
    let source_path = directory.path().join("source");
    let source = node(&source_path, &previous);
    let sibling = node(&directory.path().join("sibling"), &current);
    let owner = node(&directory.path().join("owner"), &Account::generate());
    let requester = node(&directory.path().join("requester"), &current);
    let group = new_channel_id();
    source
        .log
        .lock()
        .unwrap()
        .append(Event::new(
            &owner.identity,
            group,
            1,
            vec![],
            1,
            1,
            EventKind::MembershipChange,
            ChannelMeta {
                name: "Retained physical membership".into(),
                members: vec![
                    owner.identity.public(),
                    source.identity.public(),
                    sibling.identity.public(),
                ],
                epoch: 0,
                owner: owner.user_id(),
            }
            .encode(),
        ))
        .unwrap();
    source.process_channel(group);
    source.prepare_account_transition(&current).unwrap();
    source
        .persist_account_adoption(&current.public(), || Ok(()), || Ok(source.account.public()))
        .unwrap();
    let secret = source.identity.secret_bytes();
    drop(source);
    let source = node_with_identity(
        &source_path,
        &current,
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
    );
    source
        .account_devices
        .lock()
        .unwrap()
        .insert(AccountDevice {
            public: sibling.identity.public(),
            certificate: current.certify(&sibling.identity.public().ed25519_pub),
        })
        .unwrap();
    let signed = group_inventory_source_document(&source, group);
    requester
        .account_documents
        .lock()
        .unwrap()
        .merge(signed.meta.clone(), signed.payload())
        .unwrap();
    let reference = requester.channel_shared_reference(group).unwrap();
    assert_eq!(
        reference.member_count, 2,
        "two independently certified devices in the adopted current account are one displayed principal"
    );
    for public in [source.identity.public(), sibling.identity.public()] {
        assert_eq!(
            reference
                .members
                .iter()
                .find(|member| member.public == public)
                .unwrap()
                .account_id,
            Some(current.account_id())
        );
    }
    assert!(!requester.channel_can_send(group).unwrap());
    assert!(requester.channel_members(group).is_empty());
}

#[test]
fn group_inventory_restored_exact_wide_head_recovers_its_local_receipt() {
    let directory = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&directory.path().join("source"), &account);
    let external = node(&directory.path().join("external"), &Account::generate());
    let group = group_inventory_legacy_fixture(&source, &external, "Recovered wide head");
    copy_public_group(&external, &source, group);
    let parents = source.log.lock().unwrap().heads(&group).unwrap();
    for seq in 2..=602 {
        source
            .log
            .lock()
            .unwrap()
            .append(Event::new(
                &source.identity,
                group,
                seq,
                parents.clone(),
                2,
                seq,
                EventKind::ChannelRename,
                format!("Recovered branch {seq}").into_bytes(),
            ))
            .unwrap();
    }
    source.process_channel(group);
    let wide = group_inventory_source_document(&source, group);
    let secrets = source.identity.secret_bytes();
    let restored_path = directory.path().join("restored");
    let restored = node_with_identity(
        &restored_path,
        &account,
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
    );
    copy_public_group(&source, &restored, group);
    restored
        .account_documents
        .lock()
        .unwrap()
        .merge(wide.meta.clone(), wide.payload())
        .unwrap();
    assert!(
        restored
            .account_documents
            .lock()
            .unwrap()
            .local_publication_frontier(&wide.meta, &restored.identity.public())
            .is_none()
    );
    let repaired = group_inventory_source_document(&restored, group);
    assert!(
        restored
            .account_documents
            .lock()
            .unwrap()
            .local_publication_frontier(&repaired.meta, &restored.identity.public())
            .is_some_and(|frontier| frontier.len() > 512),
        "the exact restored source view must durably reconstruct its missing local receipt"
    );
    drop(restored);
    let restored = node_with_identity(
        &restored_path,
        &account,
        DeviceIdentity::from_secret_bytes(secrets.0, secrets.1),
    );
    assert!(
        restored
            .account_documents
            .lock()
            .unwrap()
            .local_publication_frontier(&repaired.meta, &restored.identity.public())
            .is_some_and(|frontier| frontier.len() > 512),
        "same-head receipt reconstruction must survive encrypted index replay"
    );
    restored
        .append_event(
            group,
            EventKind::ChannelRename,
            b"Recovered successor".to_vec(),
        )
        .unwrap();
    restored.process_channel(group);
    let successor = group_inventory_source_document(&restored, group);
    let body: serde_json::Value = serde_json::from_slice(successor.payload()).unwrap();
    assert_eq!(body["name"], "Recovered successor");
    assert!(successor.meta.revision > repaired.meta.revision);
}
