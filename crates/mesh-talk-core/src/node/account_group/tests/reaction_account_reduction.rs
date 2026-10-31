//! Channel account reduction preserves original signed physical reaction effects.
use super::*;
use crate::node::{channel::SealedPayload, reaction::ReactionPayload};

struct Fixture {
    _directory: tempfile::TempDir,
    observer: Arc<Node>,
    first: DeviceIdentity,
    second: DeviceIdentity,
    group: ConversationId,
    target: EventId,
}

fn fixture() -> Fixture {
    let directory = tempfile::tempdir().unwrap();
    let observer = node(&directory.path().join("observer"), &Account::generate());
    let first = DeviceIdentity::generate();
    let second = DeviceIdentity::generate();
    let group = new_channel_id();
    let genesis = Event::new(
        &observer.identity,
        group,
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Account reaction reduction".into(),
            members: vec![observer.identity.public(), first.public(), second.public()],
            epoch: 0,
            owner: observer.user_id(),
        }
        .encode(),
    );
    install(&observer, &genesis);
    configure(&observer, &directory.path().join("observer"), 9);
    Fixture {
        _directory: directory,
        observer,
        first,
        second,
        group,
        target: EventId::new([0x71; 32]),
    }
}

fn bind(f: &Fixture, identity: &DeviceIdentity, account: &Account) {
    f.observer
        .privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .proofs
        .record(&Announce::new_with_account(
            identity,
            account,
            "Same display name",
            9,
        ))
        .unwrap();
}

fn append(f: &Fixture, identity: &DeviceIdentity, wall: u64, remove: bool) -> EventId {
    let payload = ReactionPayload {
        target: f.target,
        emoji: "👍".into(),
        remove,
    };
    let sealed = SealedPayload::seal(
        identity,
        &f.observer.channel_members(f.group),
        &payload.encode(),
    )
    .unwrap();
    let mut log = f.observer.log.lock().unwrap();
    let (parents, lamport) = log.prepare(&f.group).unwrap();
    let author = crate::eventlog::Author::from_ed25519(identity.public().ed25519_pub);
    let seq = log.next_sequence(&f.group, &author).unwrap();
    let event = Event::new(
        identity,
        f.group,
        seq,
        parents,
        lamport,
        wall,
        EventKind::React,
        sealed.encode(),
    );
    let id = event.id;
    log.append_durable(event).unwrap();
    id
}

fn assert_originals(f: &Fixture, originals: &[(EventId, &DeviceIdentity)]) {
    let log = f.observer.log.lock().unwrap();
    for (id, identity) in originals {
        let event = log.get(id).unwrap().unwrap();
        assert_eq!(event.author.ed25519_pub(), &identity.public().ed25519_pub);
        assert!(event.verify_integrity() && event.verify_signature());
    }
    drop(log);
    assert_selected_summary(f);
}

fn assert_selected_summary(f: &Fixture) {
    let expected = f
        .observer
        .reactions(f.group)
        .unwrap()
        .into_iter()
        .map(|view| crate::node::ReactionSummary {
            target: view.target,
            emoji: view.emoji,
            count: view.who.len() as u64,
            from_me: view
                .who
                .iter()
                .any(|id| id == &f.observer.account_id() || id == &f.observer.user_id()),
        })
        .collect::<Vec<_>>();
    let mut cursor = None;
    for _ in 0..512 {
        let page = f
            .observer
            .reaction_summaries(f.group, &[f.target], cursor.as_deref())
            .unwrap();
        if !page.pending {
            assert_eq!(
                page.items, expected,
                "selected summaries preserve the existing authenticated account reduction"
            );
            assert!(page.next.is_none());
            return;
        }
        assert!(page.items.is_empty());
        cursor = page.next;
        assert!(cursor.is_some());
    }
    panic!("selected channel reaction projection did not settle");
}

#[test]
fn two_certified_physical_adds_reduce_to_one_account_without_rewriting_events() {
    let f = fixture();
    let account = Account::generate();
    bind(&f, &f.first, &account);
    bind(&f, &f.second, &account);
    let first = append(&f, &f.first, 10, false);
    let second = append(&f, &f.second, 11, false);
    let views = f.observer.reactions(f.group).unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(
        views[0].who,
        vec![account.account_id()],
        "two certified devices represent one reacting account"
    );
    assert_originals(&f, &[(first, &f.first), (second, &f.second)]);
}

#[test]
fn certified_sibling_remove_clears_another_devices_account_add() {
    let f = fixture();
    let account = Account::generate();
    bind(&f, &f.first, &account);
    bind(&f, &f.second, &account);
    let first = append(&f, &f.first, 10, false);
    let second = append(&f, &f.second, 11, true);
    assert!(
        f.observer.reactions(f.group).unwrap().is_empty(),
        "D-remove must clear A-add for the same certified account"
    );
    assert_originals(&f, &[(first, &f.first), (second, &f.second)]);
}

#[test]
fn certified_account_remove_wins_equal_clock_across_physical_authors() {
    let f = fixture();
    let account = Account::generate();
    bind(&f, &f.first, &account);
    bind(&f, &f.second, &account);
    let remove = append(&f, &f.second, 10, true);
    let add = append(&f, &f.first, 10, false);
    assert!(
        f.observer.reactions(f.group).unwrap().is_empty(),
        "the existing remove-wins tie rule applies across certified account devices"
    );
    assert_originals(&f, &[(add, &f.first), (remove, &f.second)]);
}

#[test]
fn unknown_physical_authors_remain_distinct_until_verified_bindings_repair_old_rows() {
    let f = fixture();
    let first = append(&f, &f.first, 10, false);
    let second = append(&f, &f.second, 11, false);
    let mut unknown = vec![f.first.user_id(), f.second.user_id()];
    unknown.sort();
    assert_eq!(f.observer.reactions(f.group).unwrap()[0].who, unknown);
    let account = Account::generate();
    bind(&f, &f.first, &account);
    bind(&f, &f.second, &account);
    assert_eq!(
        f.observer.reactions(f.group).unwrap()[0].who,
        vec![account.account_id()],
        "existing stored events must repair on read after exact verified certificates arrive"
    );
    assert_originals(&f, &[(first, &f.first), (second, &f.second)]);
}

#[test]
fn different_verified_accounts_with_the_same_name_remain_distinct() {
    let f = fixture();
    let first_account = Account::generate();
    let second_account = Account::generate();
    bind(&f, &f.first, &first_account);
    bind(&f, &f.second, &second_account);
    append(&f, &f.first, 10, false);
    append(&f, &f.second, 11, false);
    let mut accounts = vec![first_account.account_id(), second_account.account_id()];
    accounts.sort();
    assert_eq!(
        f.observer.reactions(f.group).unwrap()[0].who,
        accounts,
        "display names cannot combine unrelated authenticated principals"
    );
}

#[tokio::test]
async fn modern_reaction_keeps_its_causal_account_after_device_adoption() {
    use crate::node::channel_account_authority::{AccountAuthorityControl, AccountGrant};
    let f = fixture();
    let original_account = Account::generate();
    let adopted_account = Account::generate();
    bind(&f, &f.first, &original_account);
    bind(&f, &f.second, &original_account);
    let root = f
        .observer
        .log
        .lock()
        .unwrap()
        .events(&f.group)
        .unwrap()
        .into_iter()
        .find(|event| event.kind == EventKind::MembershipChange)
        .unwrap()
        .id;
    let own = f.observer.own_group_proof();
    let first = Announce::new_with_account(&f.first, &original_account, "original account", 9);
    let second = Announce::new_with_account(&f.second, &original_account, "original sibling", 9);
    // The actual legacy owner signs the supported account-authority cutover;
    // the projection must validate it before it can supply historical evidence.
    let control = AccountAuthorityControl {
        version: 1,
        previous: None,
        issuer: own.clone(),
        owner_account: f.observer.account.public().ed25519_pub,
        grants: vec![
            AccountGrant {
                incarnation: [1; 16],
                pending: false,
                original: own.clone(),
                membership_event: root,
                acceptance_event: None,
            },
            AccountGrant {
                incarnation: [2; 16],
                pending: false,
                original: first.clone(),
                membership_event: root,
                acceptance_event: None,
            },
        ],
        revoked: vec![],
        legacy_bindings: vec![own, first, second],
        account_signature: vec![],
    };
    let (_, authority) = f
        .observer
        .append_event_with_profile_parent(
            f.group,
            EventKind::ChannelAccountAuthority,
            control.signed_encode(f.group, &f.observer.account),
            None,
        )
        .unwrap();
    f.observer.process_channel(f.group);
    assert_eq!(
        f.observer
            .account_group_projection(f.group)
            .unwrap()
            .authority
            .current_event(),
        Some(authority)
    );
    let effect = append(&f, &f.first, 10, false);
    f.observer
        .privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .proofs
        .record_explicit_account(&Announce::new_with_account(
            &f.first,
            &adopted_account,
            "adopted device",
            9,
        ))
        .unwrap();
    assert_eq!(
        f.observer.account_id_for_device(&f.first.public()),
        Some(adopted_account.account_id()),
        "the fixture must really expose a different current device account"
    );
    let views = f.observer.reactions(f.group).unwrap();
    assert_eq!(
        views[0].who,
        vec![original_account.account_id()],
        "a reaction belongs to its authenticated causal account, not today's adopted binding"
    );
    assert_originals(&f, &[(effect, &f.first)]);
}

#[test]
fn bound_private_reactions_keep_both_account_lifetimes_after_adoption_and_reopen() {
    let f = fixture();
    let old_account = Account::generate();
    let new_account = Account::generate();
    bind(&f, &f.first, &old_account);
    // Reopen the account-scoped source already published by Node, preserving
    // the same profile authority instead of swapping in a fresh legacy log.
    let path = f
        ._directory
        .path()
        .join("observer")
        .join("events.log")
        .with_extension(format!("{}-reactions.log", f.observer.account_id()));
    let reopen = || {
        crate::node::reaction_store::IndexedReactionStore::from_profile(
            f.observer.storage.clone(),
            &path,
            "pw",
            f.observer.account.public().ed25519_pub,
        )
        .unwrap()
    };
    *f.observer.my_dm_reactions.lock().unwrap() = reopen();
    let add = append(&f, &f.first, 10, false);
    let payload = ReactionPayload {
        target: f.target,
        emoji: "👍".into(),
        remove: false,
    };
    f.observer
        .my_dm_reactions
        .lock()
        .unwrap()
        .record_channel_bound(
            f.group,
            f.first.public(),
            add,
            10,
            payload,
            Announce::new_with_account(&f.first, &old_account, "old", 9)
                .account_cert
                .unwrap(),
        )
        .unwrap();
    f.observer
        .privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .proofs
        .record_explicit_account(&Announce::new_with_account(
            &f.first,
            &new_account,
            "new",
            9,
        ))
        .unwrap();
    let remove = append(&f, &f.first, 11, true);
    f.observer
        .my_dm_reactions
        .lock()
        .unwrap()
        .record_channel_bound(
            f.group,
            f.first.public(),
            remove,
            11,
            ReactionPayload {
                target: f.target,
                emoji: "👍".into(),
                remove: true,
            },
            Announce::new_with_account(&f.first, &new_account, "new", 9)
                .account_cert
                .unwrap(),
        )
        .unwrap();
    assert_eq!(
        f.observer.reactions(f.group).unwrap()[0].who,
        vec![old_account.account_id()],
        "the adopted account's remove cannot erase the old account's independent add"
    );
    *f.observer.my_dm_reactions.lock().unwrap() = reopen();
    let rows = f
        .observer
        .my_dm_reactions
        .lock()
        .unwrap()
        .channel_page(f.group, None, 3, 8 * 1024)
        .unwrap()
        .rows;
    assert_eq!(rows.len(), 2);
    assert!(
        rows.iter()
            .any(|(source, effect, _, _, root)| *source == f.first.public()
                && *effect == add
                && *root == Some(old_account.public().ed25519_pub))
    );
    assert!(
        rows.iter()
            .any(|(source, effect, _, _, root)| *source == f.first.public()
                && *effect == remove
                && *root == Some(new_account.public().ed25519_pub))
    );
    assert_eq!(
        f.observer.reactions(f.group).unwrap()[0].who,
        vec![old_account.account_id()]
    );
    assert_originals(&f, &[(add, &f.first), (remove, &f.first)]);
}

#[test]
fn exact_unbound_effect_can_gain_verified_proof_but_cannot_change_its_bound_account() {
    let f = fixture();
    let account = Account::generate();
    let other_account = Account::generate();
    let effect = append(&f, &f.first, 10, false);
    let payload = ReactionPayload {
        target: f.target,
        emoji: "👍".into(),
        remove: false,
    };
    let mut store = f.observer.my_dm_reactions.lock().unwrap();
    store
        .record_channel(f.group, f.first.public(), effect, 10, payload.clone())
        .unwrap();
    store
        .record_channel_bound(
            f.group,
            f.first.public(),
            effect,
            10,
            payload.clone(),
            account.certify(&f.first.public().ed25519_pub),
        )
        .unwrap();
    assert!(
        store
            .record_channel_bound(
                f.group,
                f.first.public(),
                effect,
                10,
                payload.clone(),
                other_account.certify(&f.first.public().ed25519_pub)
            )
            .is_err(),
        "a later device adoption cannot rebind an existing immutable effect"
    );
    assert!(
        store
            .record_channel_bound(
                f.group,
                f.first.public(),
                effect,
                10,
                payload,
                account.certify(&f.second.public().ed25519_pub)
            )
            .is_err(),
        "another physical device's certificate is not source proof"
    );
    drop(store);
    assert_eq!(
        f.observer.reactions(f.group).unwrap()[0].who,
        vec![account.account_id()],
        "old and upgraded copies of the same effect must reduce once under the verified binding"
    );
}

#[test]
fn legacy_long_history_reactions_do_not_replay_causal_membership_per_effect() {
    let f = fixture();
    let account = Account::generate();
    bind(&f, &f.first, &account);
    for wall in 10..138 {
        append(&f, &f.first, wall, false);
    }
    crate::node::reactions::take_reaction_projection_work_for_test();
    let views = f.observer.reactions(f.group).unwrap();
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].who, vec![account.account_id()]);
    let (ancestry_visits, projections) =
        crate::node::reactions::take_reaction_projection_work_for_test();
    assert_eq!(
        (ancestry_visits, projections),
        (0, 0),
        "a legacy group has no causal account authority to replay, regardless of reaction history length"
    );
}

fn legacy_explicit_rehome_keeps_old_unbound_effect_conservative(reopen: bool) {
    let f = fixture();
    let previous = Account::generate();
    let current = Account::generate();
    bind(&f, &f.first, &previous);
    let effect = append(&f, &f.first, 10, false);
    assert_eq!(
        f.observer.reactions(f.group).unwrap()[0].who,
        vec![previous.account_id()]
    );
    f.observer
        .privacy
        .state
        .write()
        .unwrap()
        .as_mut()
        .unwrap()
        .proofs
        .record_explicit_account(&Announce::new_with_account(
            &f.first,
            &current,
            "accepted rehome",
            9,
        ))
        .unwrap();
    if reopen {
        let reloaded = crate::node::peer_directory::PeerDirectory::open(
            &f._directory.path().join("observer").join("peer-proofs"),
            "pw",
        )
        .unwrap();
        f.observer
            .privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs = reloaded;
    }
    assert_eq!(
        f.observer.account_id_for_device(&f.first.public()),
        Some(current.account_id()),
        "current identity/admission must still expose the explicitly accepted account"
    );
    assert_eq!(
        f.observer.reactions(f.group).unwrap()[0].who,
        vec![f.first.user_id()],
        "an old legacy effect without immutable account evidence must not follow the device's accepted rehome, including after reopen"
    );
    assert_originals(&f, &[(effect, &f.first)]);
}

#[test]
fn legacy_unbound_reaction_does_not_follow_an_explicit_account_rehome() {
    legacy_explicit_rehome_keeps_old_unbound_effect_conservative(false);
}

#[test]
fn legacy_unbound_reaction_rehome_ambiguity_survives_directory_reopen() {
    legacy_explicit_rehome_keeps_old_unbound_effect_conservative(true);
}
