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
    let cover_version = owner
        .channel_metadata(group)
        .unwrap()
        .cover
        .unwrap()
        .revision;
    owner
        .set_channel_announcement_if(group, Some("Welcome"), None, |commit| commit())
        .await
        .unwrap();
    assert_eq!(
        owner
            .channel_metadata(group)
            .unwrap()
            .cover
            .as_ref()
            .unwrap()
            .revision,
        cover_version
    );
    let announcement = owner
        .channel_metadata(group)
        .unwrap()
        .announcement
        .unwrap()
        .revision;
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
    let before = owner.channel_metadata(group).unwrap();
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
    assert_eq!(reopened.channel_metadata(group).unwrap(), before);
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
    }
    assert!(
        owner
            .account_group_projection(group)
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
        owner
            .channel_metadata(group)
            .unwrap()
            .announcement
            .unwrap()
            .revision,
        hex::encode(original.id.as_bytes())
    );
    assert!(
        owner
            .set_channel_announcement_if(group, Some("Unauthorized"), None, |commit| commit())
            .await
            .is_err()
    );
    next.set_channel_announcement_if(group, Some("New owner"), Some(original.id), |commit| {
        commit()
    })
    .await
    .unwrap();
    copy(&next, &owner, group);
    assert_eq!(
        owner
            .channel_metadata(group)
            .unwrap()
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
    assert!(
        member
            .channel_metadata(group)
            .unwrap()
            .announcement
            .is_none()
    );
    owner.retry_group_metadata().unwrap();
    copy(&owner, &member, group);
    assert_eq!(
        member
            .channel_metadata(group)
            .unwrap()
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
    for event in from.metadata_events(channel) {
        to.log.lock().unwrap().append(event).unwrap();
    }
    to.process_channel(channel);
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
    let first = owner.channel_metadata(group).unwrap().announcement.unwrap();
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
    assert!(
        newcomer
            .channel_metadata(group)
            .unwrap()
            .announcement_pending
    );
    assert!(
        newcomer
            .channel_metadata(group)
            .unwrap()
            .announcement
            .is_none()
    );
    let examined = holder.log.lock().unwrap().fingerprint_and_len(&group);
    assert_eq!(holder.retry_group_metadata().unwrap(), vec![group]);
    assert!(
        holder
            .group_metadata_cache
            .lock()
            .unwrap()
            .reconciled
            .contains(&(group, examined)),
        "retry must cache only examined sources, never stamp unexamined appended controls as complete"
    );
    assert_ne!(
        holder.log.lock().unwrap().fingerprint_and_len(&group),
        examined
    );
    copy(&holder, &newcomer, group);
    let current = newcomer.channel_metadata(group).unwrap();
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
    assert!(newcomer.channel_history(group, 100).is_empty());
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
    let events = owner.metadata_events(group);
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
        owner
            .channel_metadata(group)
            .unwrap()
            .announcement
            .unwrap()
            .content
            .as_deref(),
        Some("private announcement")
    );
    let before = owner.metadata_events(group).len();
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
    assert_eq!(owner.metadata_events(group).len(), before);
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
    let first = node
        .channel_metadata(channel)
        .unwrap()
        .announcement
        .unwrap();
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
    let cleared = node.channel_metadata(channel).unwrap();
    assert!(cleared.announcement.unwrap().content.is_none());
    assert_eq!(cleared.announcement_history.len(), 2);
    assert!(cleared.cover.is_none());
}
