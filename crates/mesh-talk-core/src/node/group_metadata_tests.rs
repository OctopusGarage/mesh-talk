use super::super::*;
use super::{Control, Field, encode};
use crate::discovery::Roster;
use crate::identity::device::DeviceIdentity;
use base64::Engine;
use std::sync::{Arc, Mutex};

fn cover() -> String {
    let mut bytes = std::io::Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, image::ImageFormat::Png)
        .unwrap();
    format!(
        "data:image/png;base64,{}",
        base64::engine::general_purpose::STANDARD.encode(bytes.into_inner())
    )
}

// The selected query reports unfinished canonical/index work explicitly. Tests
// wait for that same public continuation and keep all final semantic assertions.
fn metadata(node: &Node, channel: crate::eventlog::ConversationId) -> super::ChannelMetadata {
    // Each publication can need several bounded ancestry/authority phases.
    // Bound the fixture's total work by its signed input, rather than treating
    // one query page as a completed projection.
    let turns = (node.log.lock().unwrap().conversation_len(&channel).unwrap() + 1) * 64;
    for _ in 0..turns {
        let before = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        let value = node.channel_metadata(channel).unwrap();
        let after = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert!(
            after.1 - before.1 <= 64,
            "one metadata request must retain its body budget"
        );
        if !value.query_pending {
            return value;
        }
        assert!(value.cover.is_none());
        assert!(value.announcement.is_none());
        assert!(value.announcement_history.is_empty());
    }
    panic!("selected group metadata did not finish its bounded continuation");
}

// Direct signed-event copies bypass the participant worker. Complete its
// canonical/presentation continuation before invoking a settled-state API.
fn settle_channel(node: &Node, channel: crate::eventlog::ConversationId) {
    let turns = (node.log.lock().unwrap().conversation_len(&channel).unwrap() + 1) * 64;
    node.process_channel(channel);
    for _ in 0..turns {
        node.retry_channel_plaintext();
        match node.channel_checkpoints.prepared_history_policy(channel) {
            Ok(_) => return,
            Err(crate::eventlog::LogError::StaleCursor) => {}
            Err(_) => panic!("channel fixture preparation failed"),
        }
    }
    panic!("channel fixture must complete its bounded continuation");
}

// The worker owns one bounded authority turn; a missing release is not a
// completed empty result until its selected preparation has finished.
fn release_metadata(node: &Node, channel: crate::eventlog::ConversationId) {
    for _ in 0..128 {
        let released = node.retry_group_metadata().unwrap();
        if !released.is_empty() {
            assert_eq!(released, vec![channel]);
            return;
        }
    }
    panic!("current metadata release did not finish its bounded continuation");
}

#[tokio::test]
async fn independent_cover_and_announcement_survive_restart_with_clear_tombstone() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(dir.path());
    let group = owner.create_channel("metadata", vec![]).await.unwrap();
    let image = cover();
    owner
        .set_channel_cover_if(group, Some(&image), None, |commit| commit())
        .await
        .unwrap();
    let cover_version = metadata(&owner, group).cover.unwrap().revision;
    owner
        .set_channel_announcement_if(group, Some("Welcome"), None, |commit| commit())
        .await
        .unwrap();
    assert_eq!(
        metadata(&owner, group).cover.as_ref().unwrap().revision,
        cover_version
    );
    let announcement = metadata(&owner, group).announcement.unwrap().revision;
    owner
        .set_channel_announcement_if(
            group,
            None,
            Some(crate::eventlog::EventId::new(
                hex::decode(announcement).unwrap().try_into().unwrap(),
            )),
            |commit| commit(),
        )
        .await
        .unwrap();
    let before = metadata(&owner, group);
    let (ed, x) = owner.identity.secret_bytes();
    drop(owner);
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (messages, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    let reopened = Node::open(
        DeviceIdentity::from_secret_bytes(ed, x),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        messages,
        files,
        &dir.path().join("events"),
        &dir.path().join("sent"),
        "pw",
    )
    .unwrap();
    assert_eq!(metadata(&reopened, group), before);
    assert!(before.announcement.unwrap().content.is_none());
    assert_eq!(
        before.cover.unwrap().content.as_deref(),
        Some(image.as_str())
    );
}

#[tokio::test]
async fn adopted_account_cannot_read_historical_devices_encrypted_group_metadata() {
    let dir = tempfile::tempdir().unwrap();
    let adopted_dir = tempfile::tempdir().unwrap();
    let owner = node(dir.path());
    let group = owner
        .create_channel_invited("metadata", vec![])
        .await
        .unwrap();
    let mut work = super::super::channel_account_maintenance::ChannelAccountMaintenance::default();
    for _ in 0..12 {
        owner.maintain_account_channels(&mut work).await;
        settle_channel(&owner, group);
    }
    assert!(
        owner
            .account_group_projection(group)
            .unwrap()
            .authority
            .current_event()
            .is_some()
    );
    owner
        .set_channel_announcement_if(group, Some("Private old account"), None, |commit| commit())
        .await
        .unwrap();
    let (ed, x) = owner.identity.secret_bytes();
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (messages, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    let adopted = Node::open_with_account(
        DeviceIdentity::from_secret_bytes(ed, x),
        crate::identity::account::Account::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        messages,
        files,
        &adopted_dir.path().join("events"),
        &adopted_dir.path().join("sent"),
        "pw",
    )
    .unwrap();
    copy(&owner, &adopted, group);
    assert!(adopted.channel_metadata(group).is_err());
}

#[tokio::test]
async fn ownership_transfer_pins_observed_metadata_and_rejects_backdated_owner_fork() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let owner = node(a.path());
    let next = node(b.path());
    let group = owner
        .create_channel("metadata", vec![next.identity.public()])
        .await
        .unwrap();
    owner
        .set_channel_announcement_if(group, Some("Observed"), None, |commit| commit())
        .await
        .unwrap();
    let original = owner
        .metadata_events(group)
        .unwrap()
        .into_iter()
        .rev()
        .find(|e| e.kind == crate::eventlog::EventKind::ChannelMetadata)
        .unwrap();
    let payload = original.ciphertext.clone();
    owner
        .rename_channel(group, "before transfer")
        .await
        .unwrap();
    owner
        .transfer_channel_owner(group, &next.user_id())
        .await
        .unwrap();
    let fork = crate::eventlog::Event::new(
        &owner.identity,
        group,
        100,
        vec![original.id],
        original.lamport + 1,
        original.wall_clock + 1000,
        crate::eventlog::EventKind::ChannelMetadata,
        payload,
    );
    owner.log.lock().unwrap().append(fork).unwrap();
    copy(&owner, &next, group);
    assert_eq!(
        metadata(&owner, group).announcement.unwrap().revision,
        hex::encode(original.id.as_bytes())
    );
    assert!(
        owner
            .set_channel_announcement_if(group, Some("Unauthorized"), None, |commit| commit())
            .await
            .is_err()
    );
    assert_eq!(
        metadata(&next, group).announcement.unwrap().revision,
        hex::encode(original.id.as_bytes())
    );
    next.set_channel_announcement_if(group, Some("New owner"), Some(original.id), |commit| {
        commit()
    })
    .await
    .unwrap();
    copy(&next, &owner, group);
    assert_eq!(
        metadata(&owner, group)
            .announcement
            .unwrap()
            .content
            .as_deref(),
        Some("New owner")
    );
}

#[tokio::test]
async fn removed_member_cannot_receive_updates_until_reinvitation_is_accepted() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let owner = node(a.path());
    let member = node(b.path());
    let group = owner
        .create_channel("metadata", vec![member.identity.public()])
        .await
        .unwrap();
    owner
        .set_channel_announcement_if(group, Some("Before removal"), None, |commit| commit())
        .await
        .unwrap();
    copy(&owner, &member, group);
    let previous = owner
        .metadata_events(group)
        .unwrap()
        .into_iter()
        .rev()
        .find(|e| e.kind == crate::eventlog::EventKind::ChannelMetadata)
        .unwrap()
        .id;
    owner
        .remove_channel_member(group, &member.user_id())
        .await
        .unwrap();
    owner
        .set_channel_announcement_if(group, Some("After removal"), Some(previous), |commit| {
            commit()
        })
        .await
        .unwrap();
    copy(&owner, &member, group);
    assert!(member.channel_metadata(group).is_err());
    assert!(owner.retry_group_metadata().unwrap().is_empty());
    owner
        .add_channel_member(group, member.identity.public())
        .await
        .unwrap();
    copy(&owner, &member, group);
    assert!(member.channel_metadata(group).is_err());
    member.accept_channel_invite(group).await.unwrap();
    copy(&member, &owner, group);
    assert!(metadata(&member, group).announcement.is_none());
    release_metadata(&owner, group);
    copy(&owner, &member, group);
    assert_eq!(
        metadata(&member, group)
            .announcement
            .unwrap()
            .content
            .as_deref(),
        Some("After removal")
    );
}

fn node(path: &std::path::Path) -> Arc<Node> {
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (messages, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        messages,
        files,
        &path.join("events"),
        &path.join("sent"),
        "pw",
    )
    .unwrap()
}
fn copy(from: &Node, to: &Node, channel: crate::eventlog::ConversationId) {
    for event in from.metadata_events(channel).unwrap() {
        to.log.lock().unwrap().append(event).unwrap();
    }
    settle_channel(to, channel);
}

#[tokio::test]
async fn accepted_holder_releases_only_current_metadata_without_owner_or_history() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let c = tempfile::tempdir().unwrap();
    let owner = node(a.path());
    let holder = node(b.path());
    let newcomer = node(c.path());
    let group = owner
        .create_channel_invited("metadata", vec![holder.identity.public()])
        .await
        .unwrap();
    copy(&owner, &holder, group);
    holder.accept_channel_invite(group).await.unwrap();
    copy(&holder, &owner, group);
    owner
        .set_channel_history_policy_if(group, crate::channel::HistoryPolicy::Off, |commit| commit())
        .await
        .unwrap();
    owner
        .set_channel_announcement_if(group, Some("old"), None, |commit| commit())
        .await
        .unwrap();
    let first = metadata(&owner, group).announcement.unwrap();
    owner
        .set_channel_announcement_if(
            group,
            Some("current"),
            Some(crate::eventlog::EventId::new(
                hex::decode(first.revision).unwrap().try_into().unwrap(),
            )),
            |commit| commit(),
        )
        .await
        .unwrap();
    owner
        .add_channel_member(group, newcomer.identity.public())
        .await
        .unwrap();
    copy(&owner, &holder, group);
    copy(&holder, &newcomer, group);
    assert!(newcomer.channel_metadata(group).is_err());
    newcomer.accept_channel_invite(group).await.unwrap();
    let current_event = owner
        .metadata_events(group)
        .unwrap()
        .into_iter()
        .rev()
        .find(|event| event.kind == crate::eventlog::EventKind::ChannelMetadata)
        .unwrap()
        .id;
    let wrong_key = crate::dm::seal(
        &newcomer.identity,
        &newcomer.identity.public().x25519_pub,
        &[0; 32],
    )
    .unwrap();
    newcomer
        .append_event(
            group,
            crate::eventlog::EventKind::ChannelMetadata,
            encode(&Control::Release {
                publication: current_event,
                recipient: newcomer.identity.public(),
                sealed_key: wrong_key,
            })
            .unwrap(),
        )
        .unwrap();
    copy(&newcomer, &holder, group);
    assert!(metadata(&newcomer, group).announcement_pending);
    assert!(metadata(&newcomer, group).announcement.is_none());
    let examined = holder
        .log
        .lock()
        .unwrap()
        .fingerprint_and_len(&group)
        .unwrap();
    release_metadata(&holder, group);
    assert!(
        holder
            .group_metadata_cache
            .lock()
            .unwrap()
            .reconciled
            .iter()
            .all(|(id, _)| *id != group),
        "a bounded release turn must not mark its unexamined remaining recipients complete"
    );
    assert_ne!(
        holder
            .log
            .lock()
            .unwrap()
            .fingerprint_and_len(&group)
            .unwrap(),
        examined
    );
    copy(&holder, &newcomer, group);
    let current = metadata(&newcomer, group);
    assert!(!current.announcement_pending);
    assert_eq!(
        current.announcement.unwrap().content.as_deref(),
        Some("current")
    );
    assert_eq!(
        current.announcement_history.len(),
        1,
        "a fresh member cannot decrypt earlier announcement versions"
    );
    assert!(newcomer.channel_history(group, 100).unwrap().is_empty());
    assert!(
        holder.retry_group_metadata().unwrap().is_empty(),
        "release retries deduplicate"
    );
    assert!(
        holder
            .set_channel_announcement_if(group, Some("forged"), None, |commit| commit())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn nonowner_controls_are_ignored_and_publication_payload_is_encrypted() {
    let a = tempfile::tempdir().unwrap();
    let b = tempfile::tempdir().unwrap();
    let owner = node(a.path());
    let member = node(b.path());
    let group = owner
        .create_channel("metadata", vec![member.identity.public()])
        .await
        .unwrap();
    owner
        .set_channel_announcement_if(group, Some("private announcement"), None, |commit| commit())
        .await
        .unwrap();
    copy(&owner, &member, group);
    let events = owner.metadata_events(group).unwrap();
    assert!(!events.iter().any(|event| {
        event
            .ciphertext
            .windows(20)
            .any(|bytes| bytes == b"private announcement")
    }));
    let keys = super::super::channel::SealedPayload::seal(
        &member.identity,
        &[member.identity.public()],
        &[0; 32],
    )
    .unwrap();
    member
        .append_event(
            group,
            crate::eventlog::EventKind::ChannelMetadata,
            encode(&Control::Publish {
                field: Field::Announcement,
                keys,
                ciphertext: vec![0; 64],
            })
            .unwrap(),
        )
        .unwrap();
    copy(&member, &owner, group);
    assert_eq!(
        metadata(&owner, group)
            .announcement
            .unwrap()
            .content
            .as_deref(),
        Some("private announcement")
    );
    let before = owner.metadata_events(group).unwrap().len();
    assert!(
        owner
            .set_channel_announcement_if(group, Some("   "), None, |commit| commit())
            .await
            .is_err()
    );
    assert!(
        owner
            .set_channel_announcement_if(group, Some(&"x".repeat(4097)), None, |commit| commit())
            .await
            .is_err()
    );
    assert_eq!(owner.metadata_events(group).unwrap().len(), before);
}

#[tokio::test]
async fn owner_metadata_revisions_preserve_other_fields_and_clear_tombstones() {
    let dir = tempfile::tempdir().unwrap();
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (messages, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        messages,
        files,
        &dir.path().join("events"),
        &dir.path().join("sent"),
        "pw",
    )
    .unwrap();
    let channel = node.create_channel("metadata", vec![]).await.unwrap();
    node.set_channel_announcement_if(channel, Some("Welcome\nMembers"), None, |commit| commit())
        .await
        .unwrap();
    let first = metadata(&node, channel).announcement.unwrap();
    assert_eq!(first.content.as_deref(), Some("Welcome\nMembers"));
    assert!(
        node.set_channel_announcement_if(channel, Some("stale edit"), None, |commit| commit())
            .await
            .is_err()
    );
    let revision =
        crate::eventlog::EventId::new(hex::decode(&first.revision).unwrap().try_into().unwrap());
    node.set_channel_announcement_if(channel, None, Some(revision), |commit| commit())
        .await
        .unwrap();
    let cleared = metadata(&node, channel);
    assert!(cleared.announcement.unwrap().content.is_none());
    assert_eq!(cleared.announcement_history.len(), 2);
    assert!(cleared.cover.is_none());
}

#[tokio::test]
async fn selected_group_metadata_does_not_read_retained_message_bodies() {
    for count in [128_u64, 512] {
        let dir = tempfile::tempdir().unwrap();
        let owner = node(dir.path());
        let group = owner
            .create_channel("bounded metadata", vec![])
            .await
            .unwrap();
        owner
            .set_channel_announcement_if(group, Some("current"), None, |commit| commit())
            .await
            .unwrap();
        for _ in 0..count {
            owner
                .append_event(group, crate::eventlog::EventKind::Message, vec![7; 2048])
                .unwrap();
        }
        owner.process_channel(group);
        owner.group_metadata_cache.lock().unwrap().values.clear();
        let before = owner
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        let value = metadata(&owner, group);
        assert_eq!(
            value.announcement.unwrap().content.as_deref(),
            Some("current")
        );
        let after = owner
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        assert!(
            after.1 - before.1 <= 64,
            "metadata request decoded {} bodies with {count} retained messages",
            after.1 - before.1
        );
        let _ = owner.channel_metadata(group).unwrap();
        assert_eq!(
            owner
                .log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            after,
            "unchanged metadata must reuse its selected value"
        );
    }
}

#[tokio::test]
async fn metadata_history_window_counts_valid_but_undecryptable_publications() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(dir.path());
    let group = owner
        .create_channel("metadata window", vec![])
        .await
        .unwrap();
    owner
        .set_channel_announcement_if(group, Some("retained"), None, |commit| commit())
        .await
        .unwrap();
    let original = owner
        .metadata_events(group)
        .unwrap()
        .into_iter()
        .find(|e| e.kind == crate::eventlog::EventKind::ChannelMetadata)
        .unwrap();
    for n in 0..20 {
        let mut payload = original.ciphertext.clone();
        if n == 19 {
            let Some(Control::Publish { field, keys, .. }) = super::decode(&original) else {
                panic!("publication fixture");
            };
            payload = encode(&Control::Publish {
                field,
                keys,
                ciphertext: vec![1; 64],
            })
            .unwrap();
        }
        owner
            .append_event(group, crate::eventlog::EventKind::ChannelMetadata, payload)
            .unwrap();
    }
    owner.process_channel(group);
    let result = metadata(&owner, group);
    assert!(result.announcement_pending);
    assert!(result.announcement.is_none());
    assert_eq!(
        result.announcement_history.len(),
        19,
        "the latest20 valid publications are selected before attempting decryption"
    );
    assert!(
        result
            .announcement_history
            .iter()
            .all(|value| value.revision != hex::encode(original.id.as_bytes()))
    );
}

#[tokio::test]
async fn metadata_control_cursor_resumes_after_restart_without_replaying_its_prefix() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(dir.path());
    let group = owner
        .create_channel("metadata cursor", vec![])
        .await
        .unwrap();
    owner
        .set_channel_announcement_if(group, Some("bounded"), None, |commit| commit())
        .await
        .unwrap();
    let payload = owner
        .metadata_events(group)
        .unwrap()
        .into_iter()
        .find(|e| e.kind == crate::eventlog::EventKind::ChannelMetadata)
        .unwrap()
        .ciphertext;
    for _ in 0..130 {
        owner
            .append_event(
                group,
                crate::eventlog::EventKind::ChannelMetadata,
                payload.clone(),
            )
            .unwrap();
    }
    owner.process_channel(group);
    let first = owner.channel_metadata(group).unwrap();
    assert!(first.query_pending);
    assert!(first.announcement.is_none());
    assert!(!first.announcement_pending);
    let mut cursor = -1;
    for _ in 0..(130 + 3) * 64 {
        let value = owner.channel_metadata(group).unwrap();
        assert!(
            value.query_pending,
            "fixture must retain an unexamined suffix"
        );
        assert!(value.announcement.is_none());
        cursor = owner
            .storage
            .database()
            .read(|c| {
                c.query_row(
                    "SELECT after_rank FROM group_metadata_index WHERE channel=?1",
                    [group.as_bytes().as_slice()],
                    |r| r.get::<_, i64>(0),
                )
                .map_err(Into::into)
            })
            .unwrap();
        if cursor >= 63 {
            break;
        }
    }
    assert!(
        cursor >= 63,
        "bounded authorization must retain its completed prefix"
    );
    let secret = owner.identity.secret_bytes();
    owner.close_profile().unwrap();
    let (dm, _) = tokio::sync::mpsc::unbounded_channel();
    let (messages, _) = tokio::sync::mpsc::unbounded_channel();
    let (files, _) = tokio::sync::mpsc::unbounded_channel();
    let owner = Node::open(
        DeviceIdentity::from_secret_bytes(secret.0, secret.1),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        messages,
        files,
        &dir.path().join("events"),
        &dir.path().join("sent"),
        "pw",
    )
    .unwrap();
    let reopened = owner
        .storage
        .database()
        .read(|c| {
            c.query_row(
                "SELECT after_rank FROM group_metadata_index WHERE channel=?1",
                [group.as_bytes().as_slice()],
                |r| r.get::<_, i64>(0),
            )
            .map_err(Into::into)
        })
        .unwrap();
    assert_eq!(reopened, cursor);
    let mut result = None;
    for _ in 0..(130 + 3) * 64 {
        let value = owner.channel_metadata(group).unwrap();
        if !value.query_pending {
            result = Some(value);
            break;
        }
    }
    let result = result.expect("resumed metadata query must finish");
    assert_eq!(
        result.announcement.unwrap().content.as_deref(),
        Some("bounded")
    );
    assert_eq!(result.announcement_history.len(), 20);
    owner
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .begin_retirement(&group)
        .unwrap();
    let before = owner
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .body_read_counts();
    let mut removed = 0;
    for _ in 0..(130 + 3) * 3 {
        let count = owner
            .storage
            .database()
            .transaction(|c| super::retire_index_on(c, group, 1))
            .unwrap();
        assert!(
            count <= 1,
            "metadata retirement exceeded the caller's row allowance"
        );
        if count == 0 {
            break;
        }
        removed += count;
    }
    assert!(removed > 0);
    assert_eq!(
        owner
            .storage
            .database()
            .transaction(|c| super::retire_index_on(c, group, 1))
            .unwrap(),
        0
    );
    assert_eq!(
        owner
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts(),
        before
    );
}

#[tokio::test]
async fn selected_metadata_authority_preparation_yields_before_history_expands() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(dir.path());
    let group = owner
        .create_account_channel_invited_if("bounded metadata authority", vec![], |commit| commit())
        .await
        .unwrap();
    let count = 601usize;
    for _ in 0..count {
        owner
            .append_event(group, crate::eventlog::EventKind::Message, vec![7; 4096])
            .unwrap();
    }
    let settle = || {
        for turn in 0..(count + 32) * 16 {
            if matches!(owner.channel_checkpoints.source_binding(group), Ok(Some(_))) {
                return;
            }
            let log = owner.log.lock().unwrap();
            owner
                .channel_checkpoints
                .project_step(log.repository().unwrap(), &owner.identity, group)
                .unwrap();
            assert!(
                turn + 1 < (count + 32) * 16,
                "canonical metadata fixture did not settle"
            );
        }
    };
    settle();
    owner
        .set_channel_announcement_if(group, Some("bounded current value"), None, |commit| {
            commit()
        })
        .await
        .unwrap();
    settle();
    owner.group_metadata_cache.lock().unwrap().values.clear();
    let facts = || {
        owner
            .storage
            .database()
            .read(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM channel_event_authority", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap()
    };
    let bodies = || {
        owner
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts()
            .0
    };
    for turn in 0..(count + 32) * 24 {
        let before = facts();
        let body_before = bodies();
        let value = owner.channel_metadata(group).unwrap();
        let installed = facts() - before;
        assert!(
            installed <= 64,
            "one metadata query installed {installed} historical authority facts"
        );
        assert!(
            bodies() - body_before <= 64,
            "one metadata query exceeded its body budget"
        );
        if !value.query_pending {
            assert_eq!(
                value.announcement.unwrap().content.as_deref(),
                Some("bounded current value")
            );
            let before = bodies();
            assert!(!owner.channel_metadata(group).unwrap().query_pending);
            assert_eq!(
                bodies(),
                before,
                "unchanged metadata query hydrated history"
            );
            return;
        }
        assert!(
            value.cover.is_none()
                && value.announcement.is_none()
                && value.announcement_history.is_empty()
        );
        if turn == 0 {
            let after: i64 = owner
                .storage
                .database()
                .read(|c| {
                    Ok(c.query_row(
                        "SELECT after_rank FROM group_metadata_index WHERE channel=?1",
                        [group.as_bytes().as_slice()],
                        |r| r.get(0),
                    )?)
                })
                .unwrap();
            assert_eq!(
                after, -1,
                "unfinished authorization advanced the durable cursor"
            );
            // An ordinary concurrent tail invalidates this exact-source query.
            // Retrying must keep the selected control eligible and never expose
            // partially authorized metadata.
            owner
                .append_event(group, crate::eventlog::EventKind::Message, vec![9; 4096])
                .unwrap();
            settle();
        }
        assert!(
            turn + 1 < (count + 32) * 24,
            "finite metadata authority query did not finish"
        );
    }
}

#[tokio::test]
async fn metadata_source_retirement_uses_remaining_query_budget() {
    let dir = tempfile::tempdir().unwrap();
    let owner = node(dir.path());
    let group = owner
        .create_channel("metadata retirement budget", vec![])
        .await
        .unwrap();
    owner
        .set_channel_announcement_if(group, Some("kept"), None, |commit| commit())
        .await
        .unwrap();
    assert_eq!(
        metadata(&owner, group)
            .announcement
            .unwrap()
            .content
            .as_deref(),
        Some("kept")
    );
    owner
        .rename_channel(group, "new authority stamp")
        .await
        .unwrap();
    for _ in 0..128 {
        if matches!(owner.channel_checkpoints.source_binding(group), Ok(Some(_))) {
            break;
        }
        let log = owner.log.lock().unwrap();
        owner
            .channel_checkpoints
            .project_step(log.repository().unwrap(), &owner.identity, group)
            .unwrap();
    }
    owner.storage.database().transaction(|c| {
        c.execute_batch("CREATE TEMP TABLE metadata_retirement_meter(count INTEGER NOT NULL); INSERT INTO metadata_retirement_meter VALUES(0);
        CREATE TEMP TRIGGER metadata_retirement_publication AFTER DELETE ON group_metadata_publications BEGIN UPDATE metadata_retirement_meter SET count=count+1; END;
        CREATE TEMP TRIGGER metadata_retirement_owner AFTER DELETE ON group_metadata_index BEGIN UPDATE metadata_retirement_meter SET count=count+1; END;
        CREATE TEMP TRIGGER metadata_retirement_new AFTER INSERT ON group_metadata_index BEGIN UPDATE metadata_retirement_meter SET count=count+1; END;")?;
        Ok(())
    }).unwrap();
    let changes = || {
        owner
            .storage
            .database()
            .read(|c| {
                Ok(
                    c.query_row("SELECT count FROM metadata_retirement_meter", [], |r| {
                        r.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap()
    };
    let mut ready = false;
    for _ in 0..8 {
        let before = changes();
        let mut budget = super::indexed::Budget::default();
        budget.limit_rows_for_test(1);
        let log = owner.log.lock().unwrap();
        ready = matches!(
            super::indexed::Index::open(&owner, log.repository().unwrap(), group, &mut budget)
                .unwrap(),
            super::indexed::Progress::Ready(_)
        );
        assert!(
            changes() - before <= 1,
            "index replacement exceeded its remaining row allowance"
        );
        if ready {
            break;
        }
    }
    assert!(ready);
    assert_eq!(
        changes(),
        3,
        "publication, old owner, and new owner must each be charged"
    );
    assert_eq!(
        metadata(&owner, group)
            .announcement
            .unwrap()
            .content
            .as_deref(),
        Some("kept")
    );
}
