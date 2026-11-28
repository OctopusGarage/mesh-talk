//! User-requested recent/older history; independent from full repair scheduling.
use super::*;
use crate::eventlog::ConversationId;
use crate::eventlog::sync::network::RecentStatus;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
#[derive(Serialize, Deserialize)]
struct Continuation {
    scope: ConversationId,
    peer: String,
    cursor: Vec<u8>,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RemoteHistoryStatus {
    Page,
    Preparing,
    Blocked,
    Unsupported,
    Reset,
    Offline,
}
#[derive(Serialize)]
pub struct RemoteHistoryPage {
    pub status: RemoteHistoryStatus,
    pub applied: usize,
    /// Opaque source/policy/snapshot-bound older-page continuation. None on Page
    /// exhausts that source snapshot only, never proves global group equality.
    pub cursor: Option<String>,
}
impl Node {
    /// One source, one recent (default200) or explicitly requested older page.
    /// Preparation may yield. Callers retry through their existing foreground action;
    /// this creates no timer, background task, or automatic old-history drain.
    pub async fn fetch_channel_history_page(
        self: &Arc<Self>,
        channel: ConversationId,
        cursor: Option<&str>,
    ) -> Result<RemoteHistoryPage, NodeError> {
        let continuation: Option<Continuation> = cursor
            .map(|s| {
                use bincode::Options;
                if s.len() > 2048 {
                    return Err(NodeError::InvalidInput(
                        "invalid history continuation".into(),
                    ));
                }
                let bytes = hex::decode(s)
                    .map_err(|_| NodeError::InvalidInput("invalid history continuation".into()))?;
                bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(1024)
                    .reject_trailing_bytes()
                    .deserialize(&bytes)
                    .map_err(|_| NodeError::InvalidInput("invalid history continuation".into()))
            })
            .transpose()?;
        if continuation
            .as_ref()
            .is_some_and(|c| c.scope != channel || c.cursor.len() > 512)
        {
            return Err(NodeError::InvalidInput(
                "history continuation scope differs".into(),
            ));
        }
        let members = {
            let book = self.channels.lock().expect("channels lock");
            let Some(state) = book.state(&channel) else {
                return Err(NodeError::InvalidInput("unknown group".into()));
            };
            if !state.is_accepted_member(&self.user_id()) || state.is_dissolved() {
                return Err(NodeError::InvalidInput(
                    "group history access unavailable".into(),
                ));
            }
            state.members().to_vec()
        };
        let peer = members
            .iter()
            .filter(|m| **m != self.identity.public())
            .filter(|m| continuation.as_ref().is_none_or(|c| c.peer == m.user_id()))
            .find_map(|m| self.routing_peer(&m.user_id()).filter(|p| p.public == *m));
        let Some(peer) = peer else {
            return Ok(RemoteHistoryPage {
                status: RemoteHistoryStatus::Offline,
                applied: 0,
                cursor: cursor.map(str::to_owned),
            });
        };
        tokio::time::timeout(std::time::Duration::from_secs(15), async {
            let (mut connection, _) = self
                .privacy_sync_dial_with_reuse(peer.addr, &peer.public)
                .await
                .map_err(|e| NodeError::Session(super::session::SessionError::Transport(e)))?;
            let store = self.pull_sync_store(connection.peer_identity());
            let _reception = self
                .request_reception_scope_with_arrival(
                    &store,
                    &connection,
                    super::reception_effects::ArrivalKind::CatchUp,
                )
                .ok_or_else(|| NodeError::InvalidInput("history runtime retired".into()))?;
            // At most four preparation/page exchanges (eight RPCs). Stop on the
            // first actual page: never follow its older-history continuation.
            let mut response = None;
            for _ in 0..4 {
                let page = Box::pin(super::session::recent_history::request(
                    &mut connection,
                    &store,
                    channel,
                    continuation.as_ref().map(|c| c.cursor.as_slice()),
                ))
                .await;
                let preparing = matches!(&page,Ok(p) if p.status==RecentStatus::More);
                response = Some(page);
                if !preparing {
                    break;
                }
                tokio::task::yield_now().await;
            }
            let response = response.expect("nonempty bounded history turn");
            self.dispatch_received_sync_work(&store);
            let response = response.map_err(NodeError::Session)?;
            self.recycle_sync_channel(connection);
            if matches!(response.status, RecentStatus::More | RecentStatus::Blocked) {
                self.schedule_channel_recovery(channel);
            }
            let status = match response.status {
                RecentStatus::Ready => RemoteHistoryStatus::Page,
                RecentStatus::More => RemoteHistoryStatus::Preparing,
                RecentStatus::Blocked => RemoteHistoryStatus::Blocked,
                RecentStatus::Unsupported => RemoteHistoryStatus::Unsupported,
                RecentStatus::Reset => RemoteHistoryStatus::Reset,
            };
            let next = match response.status {
                RecentStatus::More => cursor.map(str::to_owned),
                _ => response
                    .next
                    .map(|cursor| {
                        bincode::serialize(&Continuation {
                            scope: channel,
                            peer: peer.public.user_id(),
                            cursor,
                        })
                        .map(hex::encode)
                    })
                    .transpose()
                    .map_err(|e| NodeError::InvalidInput(e.to_string()))?,
            };
            Ok::<_, NodeError>(RemoteHistoryPage {
                status,
                applied: response.report.applied,
                cursor: next,
            })
        })
        .await
        .map_err(|_| NodeError::InvalidInput("history page timed out; retry this page".into()))?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discovery::Announce;
    use crate::identity::{account::Account, device::DeviceIdentity};

    fn fixture() -> (tempfile::TempDir, Arc<Node>) {
        let directory = tempfile::tempdir().unwrap();
        let peer = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "unreachable source",
            9,
        );
        let (node, _, _) = crate::node::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &peer,
        );
        (directory, node)
    }

    #[tokio::test]
    async fn history_cursor_validation_precedes_group_lookup_and_network() {
        let (_directory, node) = fixture();
        let group = ConversationId::new([7; 32]);
        for cursor in [
            "x".to_owned(),
            "zz".to_owned(),
            "a".repeat(2050),
            "00".into(),
        ] {
            assert!(matches!(
                node.fetch_channel_history_page(group, Some(&cursor)).await,
                Err(NodeError::InvalidInput(message)) if message == "invalid history continuation"
            ));
        }
        for (scope, cursor) in [
            (ConversationId::new([8; 32]), vec![]),
            (group, vec![0; 513]),
        ] {
            let encoded = hex::encode(
                bincode::serialize(&Continuation {
                    scope,
                    peer: "source".into(),
                    cursor,
                })
                .unwrap(),
            );
            assert!(matches!(
                node.fetch_channel_history_page(group, Some(&encoded)).await,
                Err(NodeError::InvalidInput(message)) if message == "history continuation scope differs"
            ));
        }
        let mut trailing = bincode::serialize(&Continuation {
            scope: group,
            peer: "source".into(),
            cursor: vec![],
        })
        .unwrap();
        trailing.push(0);
        assert!(matches!(
            node.fetch_channel_history_page(group, Some(&hex::encode(trailing))).await,
            Err(NodeError::InvalidInput(message)) if message == "invalid history continuation"
        ));
    }

    #[tokio::test]
    async fn history_without_an_eligible_source_preserves_explicit_navigation() {
        let (_directory, node) = fixture();
        let unknown = ConversationId::new([9; 32]);
        assert!(matches!(
            node.fetch_channel_history_page(unknown, None).await,
            Err(NodeError::InvalidInput(message)) if message == "unknown group"
        ));
        let group = node.create_channel("local history", vec![]).await.unwrap();
        let first = node.fetch_channel_history_page(group, None).await.unwrap();
        assert_eq!(first.status, RemoteHistoryStatus::Offline);
        assert_eq!(first.applied, 0);
        assert!(first.cursor.is_none());
        let cursor = hex::encode(
            bincode::serialize(&Continuation {
                scope: group,
                peer: "unavailable source".into(),
                cursor: vec![1, 2, 3],
            })
            .unwrap(),
        );
        let older = node
            .fetch_channel_history_page(group, Some(&cursor))
            .await
            .unwrap();
        assert_eq!(older.status, RemoteHistoryStatus::Offline);
        assert_eq!(older.applied, 0);
        assert_eq!(older.cursor.as_deref(), Some(cursor.as_str()));
        node.dissolve_channel(group).await.unwrap();
        assert!(matches!(
            node.fetch_channel_history_page(group, None).await,
            Err(NodeError::InvalidInput(message)) if message == "group history access unavailable"
        ));
    }

    #[tokio::test]
    async fn foreground_history_uses_one_source_and_requires_explicit_older_navigation() {
        let directories = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let source_identity = DeviceIdentity::generate();
        let target_identity = DeviceIdentity::generate();
        let source_account = Account::generate();
        let target_account = Account::generate();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let source_proof = Announce::new_with_account(
            &source_identity,
            &source_account,
            "history source",
            listener.local_addr().unwrap().port(),
        );
        let target_proof =
            Announce::new_with_account(&target_identity, &target_account, "reader", 9);
        let (source, _, _) = crate::node::test_support::node_with_files(
            directories[0].path(),
            source_identity,
            source_account,
            &target_proof,
        );
        let (target, _, _) = crate::node::test_support::node_with_files(
            directories[1].path(),
            target_identity,
            target_account,
            &source_proof,
        );
        source.cached_peer_snapshot_async().await.unwrap();
        target.cached_peer_snapshot_async().await.unwrap();
        let group = source
            .create_channel("bounded foreground history", vec![target.identity.public()])
            .await
            .unwrap();
        source.await_channel_metadata(group).await.unwrap();
        for event in source.log.lock().unwrap().events(&group).unwrap() {
            target.log.lock().unwrap().append_durable(event).unwrap();
        }
        crate::node::test_support::settle_group_metadata(&target, group);
        // Signed opaque bodies exercise the history transport independently of
        // application plaintext decoding, as in the owning session regression.
        let mut messages = Vec::new();
        for _ in 0..201 {
            let (_, id) = source
                .append_event_with_profile_parent(
                    group,
                    crate::eventlog::EventKind::Message,
                    vec![1],
                    None,
                )
                .unwrap();
            messages.push(id);
        }
        let mut host = crate::node::test_support::IndependentPeerRuntime::start_multithreaded_with(
            source.clone(),
            listener,
            |node, listener| node.run_inbound_connections_for_test(listener),
        )
        .await;
        host.launch();
        async fn page(
            node: &Arc<Node>,
            group: ConversationId,
            cursor: Option<&str>,
        ) -> RemoteHistoryPage {
            for _ in 0..128 {
                let page = node
                    .fetch_channel_history_page(group, cursor)
                    .await
                    .unwrap();
                if page.status != RemoteHistoryStatus::Preparing {
                    return page;
                }
                assert_eq!(page.applied, 0);
                assert_eq!(page.cursor.as_deref(), cursor);
                tokio::task::yield_now().await;
            }
            panic!("finite history preparation must finish within bounded foreground retries");
        }
        let first = page(&target, group, None).await;
        assert_eq!(first.status, RemoteHistoryStatus::Blocked);
        assert_eq!(
            first.applied, 0,
            "missing old ancestry is an explicit gap, never a false committed page"
        );
        let cursor = first
            .cursor
            .expect("201 records require explicit older navigation");
        let held = messages
            .iter()
            .filter(|id| target.log.lock().unwrap().get(id).unwrap().is_some())
            .count();
        assert!(
            held <= 200,
            "one foreground action must not drain group history"
        );
        let token: Continuation = bincode::deserialize(&hex::decode(&cursor).unwrap()).unwrap();
        assert_eq!(token.scope, group);
        assert_eq!(token.peer, source.user_id());
        let older = page(&target, group, Some(&cursor)).await;
        assert_eq!(older.status, RemoteHistoryStatus::Page);
        assert!(older.applied <= 200);
        assert!(
            older.cursor.is_none(),
            "the older action exhausts only its pinned source snapshot"
        );
        assert!(
            messages
                .iter()
                .any(|id| target.log.lock().unwrap().get(id).unwrap().is_some())
        );
        host.stop().await;
    }
}
