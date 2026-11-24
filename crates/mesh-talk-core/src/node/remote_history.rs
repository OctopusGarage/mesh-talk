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
