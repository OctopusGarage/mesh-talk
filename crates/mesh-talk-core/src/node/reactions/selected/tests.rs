use super::*;
use crate::{
    discovery::Announce,
    identity::{account::Account, device::DeviceIdentity},
};

#[test]
fn selected_reaction_request_bounds_cold_work_and_reuses_warm_projection() {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "peer", 9);
    let node = crate::node::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    )
    .0;
    let conversation = dm_conversation_id(&node.identity.public(), &peer.public());
    let target = EventId::new([241; 32]);
    for n in 0u64..256 {
        let payload = ReactionPayload {
            target: if n == 255 {
                target
            } else {
                EventId::new([n as u8; 32])
            },
            emoji: "x".repeat(8192),
            remove: false,
        };
        let event = Event::new(
            &peer,
            conversation,
            n + 1,
            vec![],
            n + 1,
            n,
            EventKind::React,
            crate::dm::seal(&peer, &node.identity.public().x25519_pub, &payload.encode()).unwrap(),
        );
        node.log.lock().unwrap().append_durable(event).unwrap();
    }
    let reads = || {
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts()
            .0
    };
    let before = reads();
    let mut page = node
        .reaction_summaries(conversation, &[target], None)
        .unwrap();
    assert!(
        reads() - before <= 64,
        "one selected request must not decrypt the full history"
    );
    assert!(page.pending && page.items.is_empty() && page.next.is_some());
    for _ in 0..128 {
        if !page.pending {
            break;
        }
        let before = reads();
        page = node
            .reaction_summaries(conversation, &[target], page.next.as_deref())
            .unwrap();
        assert!(
            reads() - before <= 64,
            "each continuation has its own finite body budget"
        );
    }
    assert!(
        !page.pending,
        "finite selected history must reach a complete summary"
    );
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].count, 1);
    assert!(!page.items[0].from_me);
    let before = reads();
    let warm = node
        .reaction_summaries(conversation, &[target], None)
        .unwrap();
    assert_eq!(warm, page);
    assert_eq!(
        reads(),
        before,
        "unchanged selected reads reuse the decoded projection"
    );
    let ordinary = Event::new(
        &peer,
        conversation,
        257,
        vec![],
        257,
        257,
        EventKind::Message,
        vec![7; 4096],
    );
    node.log.lock().unwrap().append_durable(ordinary).unwrap();
    assert_eq!(
        node.reaction_summaries(conversation, &[target], None)
            .unwrap(),
        page
    );
    assert_eq!(
        reads(),
        before,
        "ordinary message tails preserve the ready reaction projection"
    );
    store::take_metadata_reads();
    let other = complete(&node, conversation, EventId::new([0; 32]));
    assert_eq!(other.items.len(), 1);
    assert_eq!(
        reads(),
        before,
        "retargeting reuses authenticated decoded facts"
    );
    assert!(
        store::take_metadata_reads() <= 2,
        "exact target seeks skip unrelated decoded history"
    );
}
fn selected_fixture() -> (
    tempfile::TempDir,
    std::sync::Arc<Node>,
    DeviceIdentity,
    ConversationId,
) {
    let directory = tempfile::tempdir().unwrap();
    let peer = DeviceIdentity::generate();
    let proof = Announce::new_with_account(&peer, &Account::generate(), "peer", 9);
    let node = crate::node::test_support::node_with_files(
        directory.path(),
        DeviceIdentity::generate(),
        Account::generate(),
        &proof,
    )
    .0;
    let conversation = dm_conversation_id(&node.identity.public(), &peer.public());
    (directory, node, peer, conversation)
}
fn complete(node: &Node, conversation: ConversationId, target: EventId) -> ReactionSummaryPage {
    let mut next = None;
    for _ in 0..512 {
        let page = node
            .reaction_summaries(conversation, &[target], next.as_deref())
            .unwrap();
        if !page.pending {
            return page;
        }
        assert!(page.items.is_empty());
        next = page.next;
        assert!(next.is_some());
    }
    panic!("bounded preparation did not settle");
}
#[test]
fn selected_reaction_continuations_bind_targets_and_source_and_reject_invalid_budgets() {
    let (_directory, node, peer, conversation) = selected_fixture();
    let target = EventId::new([13; 32]);
    let payload = ReactionPayload {
        target,
        emoji: "ok".into(),
        remove: false,
    };
    let event = Event::new(
        &peer,
        conversation,
        1,
        vec![],
        1,
        1,
        EventKind::React,
        crate::dm::seal(&peer, &node.identity.public().x25519_pub, &payload.encode()).unwrap(),
    );
    node.log.lock().unwrap().append_durable(event).unwrap();
    assert!(matches!(
        node.reaction_summaries(conversation, &[], None),
        Err(NodeError::Log(LogError::InvalidPageBudget))
    ));
    assert!(matches!(
        node.reaction_summaries(conversation, &vec![target; 65], None),
        Err(NodeError::Log(LogError::InvalidPageBudget))
    ));
    let first = node
        .reaction_summaries(conversation, &[target], None)
        .unwrap();
    assert!(first.pending);
    assert!(matches!(
        node.reaction_summaries(
            conversation,
            &[EventId::new([14; 32])],
            first.next.as_deref()
        ),
        Err(NodeError::Log(LogError::StaleCursor))
    ));
    let page = complete(&node, conversation, target);
    assert_eq!(page.items[0].count, 1);
    // A saved output cursor cannot be attached to a later source revision.
    let store = store::Store::open(
        node.storage.clone(),
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .receiving_owner()
            .unwrap(),
    )
    .unwrap();
    let scope = Scope {
        conversation,
        peer: None,
        channel: false,
    };
    let snapshot = node.reaction_snapshot(&store, &scope, &[target]).unwrap();
    let (state, _) = store.begin(snapshot.query, &[target], None).unwrap();
    let cursor = store::Store::cursor(state.id, 0);
    let remove = ReactionPayload {
        remove: true,
        ..payload
    };
    node.log
        .lock()
        .unwrap()
        .append_durable(Event::new(
            &peer,
            conversation,
            2,
            vec![],
            2,
            2,
            EventKind::React,
            crate::dm::seal(&peer, &node.identity.public().x25519_pub, &remove.encode()).unwrap(),
        ))
        .unwrap();
    assert!(matches!(
        node.reaction_summaries(conversation, &[target], Some(&cursor)),
        Err(NodeError::Log(LogError::StaleCursor))
    ));
    assert!(complete(&node, conversation, target).items.is_empty());
}
#[test]
fn selected_reaction_reduction_is_atomic_counted_and_remove_wins_ties() {
    let (_directory, node, _peer, _conversation) = selected_fixture();
    let store = store::Store::open(
        node.storage.clone(),
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .receiving_owner()
            .unwrap(),
    )
    .unwrap();
    let target = EventId::new([7; 32]);
    let tag = [9; 32];
    let own = [node.user_id(), node.account_id()];
    let (state, _) = store.begin(tag, &[target], None).unwrap();
    node.storage.database().transaction(|c|{c.execute_batch("CREATE TRIGGER reject_reaction_summary BEFORE INSERT ON reaction_query_summary BEGIN SELECT RAISE(ABORT,'injected summary failure'); END;")?;Ok(())}).unwrap();
    let mine = store::Fact {
        target,
        emoji: "yes".into(),
        principal: own[1].clone(),
        wall: 8,
        removed: false,
    };
    assert!(store.advance(&state, 0, 1, 0, None, &[mine], &own).is_err());
    let (unchanged, _) = store.begin(tag, &[target], None).unwrap();
    assert_eq!(unchanged.after, -1);
    node.storage
        .database()
        .transaction(|c| {
            assert_eq!(
                c.query_row("SELECT count(*) FROM reaction_query_latest", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            c.execute_batch("DROP TRIGGER reject_reaction_summary")?;
            Ok(())
        })
        .unwrap();
    let mut state = unchanged;
    for batch in 0..8 {
        let facts = (0..64)
            .map(|n| store::Fact {
                target,
                emoji: "yes".into(),
                principal: format!("peer{}", batch * 64 + n),
                wall: 8,
                removed: false,
            })
            .collect::<Vec<_>>();
        store
            .advance(&state, 0, batch, 0, None, &facts, &own)
            .unwrap();
        state = store.begin(tag, &[target], None).unwrap().0;
    }
    let facts = [
        store::Fact {
            target,
            emoji: "yes".into(),
            principal: own[1].clone(),
            wall: 8,
            removed: false,
        },
        store::Fact {
            target,
            emoji: "yes".into(),
            principal: "peer0".into(),
            wall: 8,
            removed: true,
        },
        store::Fact {
            target,
            emoji: "yes".into(),
            principal: "peer0".into(),
            wall: 8,
            removed: false,
        },
    ];
    store.advance(&state, 5, 0, 0, None, &facts, &own).unwrap();
    let state = store.begin(tag, &[target], None).unwrap().0;
    let page = store.summaries(&state, 0).unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.items[0].count, 512);
    assert!(page.items[0].from_me);
    assert!(!page.pending);
    assert!(page.next.is_none());
}
#[test]
fn selected_reaction_summary_pages_seek_and_reject_oversize_before_output() {
    let (_directory, node, _peer, _conversation) = selected_fixture();
    let store = store::Store::open(
        node.storage.clone(),
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .receiving_owner()
            .unwrap(),
    )
    .unwrap();
    let target = EventId::new([7; 32]);
    let tag = [10; 32];
    let own = [node.user_id(), node.account_id()];
    let mut state = store.begin(tag, &[target], None).unwrap().0;
    for batch in 0..3 {
        let facts = (batch * 64..((batch + 1) * 64).min(129))
            .map(|n| store::Fact {
                target,
                emoji: format!("e{n:03}"),
                principal: own[0].clone(),
                wall: 1,
                removed: false,
            })
            .collect::<Vec<_>>();
        store
            .advance(&state, 0, batch, 0, None, &facts, &own)
            .unwrap();
        state = store.begin(tag, &[target], None).unwrap().0;
    }
    store.advance(&state, 5, 0, 0, None, &[], &own).unwrap();
    state = store.begin(tag, &[target], None).unwrap().0;
    let first = store.summaries(&state, 0).unwrap();
    assert_eq!(first.items.len(), 128);
    assert_eq!(first.items[0].emoji, "e000");
    let (_, after) = store.begin(tag, &[target], first.next.as_deref()).unwrap();
    let tail = store.summaries(&state, after).unwrap();
    assert_eq!(tail.items.len(), 1);
    assert_eq!(tail.items[0].emoji, "e128");
    assert!(tail.next.is_none());
    let huge = store::Fact {
        target,
        emoji: "x".repeat(200_000),
        principal: own[0].clone(),
        wall: 1,
        removed: false,
    };
    let tag = [11; 32];
    let state = store.begin(tag, &[target], None).unwrap().0;
    store.advance(&state, 5, 0, 0, None, &[huge], &own).unwrap();
    let state = store.begin(tag, &[target], None).unwrap().0;
    assert!(matches!(
        store.summaries(&state, 0),
        Err(LogError::PageBudgetTooSmall { .. })
    ));
    let ordinal = node
        .storage
        .database()
        .read(|c| {
            Ok(c.query_row(
                "SELECT ordinal FROM reaction_query_summary WHERE query=?1",
                [state.id.as_slice()],
                |r| r.get::<_, i64>(0),
            )?)
        })
        .unwrap();
    assert!(
        matches!(
            store.summaries(&state, ordinal),
            Err(LogError::PageBudgetTooSmall { .. })
        ),
        "a fabricated cursor cannot hydrate an oversize key"
    );
}

#[tokio::test]
async fn selected_reaction_causal_preparation_yields_before_historical_facts_expand() {
    let (directory, node, _peer, _) = selected_fixture();
    let channel = node
        .create_account_channel_invited_if("bounded causal reactions", vec![], |commit| commit())
        .await
        .unwrap();
    let count = 601usize;
    for _ in 0..count {
        node.append_event(channel, EventKind::Message, vec![19; 4096])
            .unwrap();
    }
    let target = EventId::new([237; 32]);
    let payload = ReactionPayload {
        target,
        emoji: "bounded".into(),
        remove: false,
    };
    let sealed = crate::node::channel::SealedPayload::seal(
        &node.identity,
        &[node.identity.public()],
        &payload.encode(),
    )
    .unwrap();
    node.append_event(channel, EventKind::React, sealed.encode())
        .unwrap();
    // Prepare the canonical source before measuring the selected causal query.
    // This isolates its work from ordinary channel discovery and admission.
    for turn in 0..(count + 32) * 16 {
        if matches!(
            node.channel_checkpoints.source_binding(channel),
            Ok(Some(_))
        ) {
            break;
        }
        let log = node.log.lock().unwrap();
        node.channel_checkpoints
            .project_step(log.repository().unwrap(), &node.identity, channel)
            .unwrap();
        assert!(
            turn + 1 < (count + 32) * 16,
            "canonical fixture did not settle"
        );
    }
    assert!(
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .raw_kind_revisions(channel)
            .unwrap()
            .unwrap()[EventKind::ChannelAccountAuthority as usize]
            >= 0
    );
    let facts = || {
        node.storage
            .database()
            .read(|c| {
                Ok(
                    c.query_row("SELECT COUNT(*) FROM channel_event_authority", [], |row| {
                        row.get::<_, i64>(0)
                    })?,
                )
            })
            .unwrap()
    };
    let bodies = || {
        node.log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts()
            .0
    };
    let targets = [target, EventId::new([236; 32])];
    let mut cursors = [None, None];
    let mut completed = [None, None];
    // Multiple displayed target chunks must share causal preparation rather
    // than cancelling it whenever the next target-scoped page requests a turn.
    for turn in 0..(count + 32) * 24 {
        let slot = turn % 2;
        if completed[slot].is_some() {
            continue;
        }
        let before = facts();
        let body_before = bodies();
        let page = node
            .reaction_summaries(channel, &[targets[slot]], cursors[slot].as_deref())
            .unwrap();
        let installed = facts() - before;
        assert!(
            installed <= 64,
            "one selected request installed {installed} historical authority facts"
        );
        assert!(
            bodies() - body_before <= 64,
            "one selected request exceeded its body budget"
        );
        if !page.pending {
            completed[slot] = Some(page);
            if completed.iter().all(Option::is_some) {
                break;
            }
            continue;
        }
        assert!(page.items.is_empty());
        assert!(page.next.is_some());
        cursors[slot] = page.next;
    }
    assert!(
        completed[1]
            .as_ref()
            .expect("other selected target must settle")
            .items
            .is_empty()
    );
    let complete = completed[0]
        .take()
        .expect("finite selected causal preparation must finish");
    assert_eq!(complete.items.len(), 1);
    assert_eq!(complete.items[0].count, 1);
    assert!(complete.items[0].from_me);
    assert_eq!(complete.items[0].emoji, "bounded");
    let before = bodies();
    assert_eq!(
        node.reaction_summaries(channel, &[target], None).unwrap(),
        complete
    );
    assert_eq!(
        bodies(),
        before,
        "warm selected reactions do not revisit historical bodies"
    );
    drop(node);
    drop(directory);
}
