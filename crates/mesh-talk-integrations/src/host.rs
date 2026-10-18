//! A lifecycle boundary for a separately owned bot profile.
use crate::{
    IntegrationError,
    service::{CommitAuthorization, HostFuture, HostOperation, IntegrationHost},
};
use mesh_talk_core::{
    eventlog::event::ConversationId,
    node::{MessageAcceptance, Node, NodeError, OutgoingMessageRequest},
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub struct HeadlessHost {
    node: Arc<Node>,
    account_id: String,
    active: Arc<Mutex<bool>>,
    operations: tokio::sync::Mutex<()>,
    deliveries: tokio::sync::Mutex<tokio::task::JoinSet<()>>,
}
fn retired() -> IntegrationError {
    IntegrationError::new("SESSION_RETIRED", "Bot profile is stopping")
}
impl HeadlessHost {
    pub fn new(node: Arc<Node>) -> Self {
        Self {
            account_id: node.account_id(),
            node,
            active: Arc::new(Mutex::new(true)),
            operations: tokio::sync::Mutex::new(()),
            deliveries: tokio::sync::Mutex::new(tokio::task::JoinSet::new()),
        }
    }
    /// Existing enrollment finishes before retirement; no new commit can follow.
    pub async fn retire(&self) {
        let _operation = self.operations.lock().await;
        if let Ok(mut active) = self.active.lock() {
            *active = false;
        }
        let mut deliveries = self.deliveries.lock().await;
        deliveries.abort_all();
        while deliveries.join_next().await.is_some() {}
    }
}
impl IntegrationHost for HeadlessHost {
    fn account_id(&self) -> &str {
        &self.account_id
    }
    fn send<'a>(
        &'a self,
        request: OutgoingMessageRequest,
        authorization: Arc<dyn CommitAuthorization>,
    ) -> HostFuture<'a, MessageAcceptance> {
        Box::pin(async move {
            let accepted = {
                let _operation = self.operations.lock().await;
                self.node
                    .enqueue_message_once_if(&request, |operation| {
                        let active = self
                            .active
                            .lock()
                            .map_err(|_| NodeError::Authorization("profile unavailable".into()))?;
                        if !*active {
                            return Err(NodeError::Authorization("profile retired".into()));
                        }
                        authorization.commit(operation)
                    })
                    .await?
            };
            // Network distribution never delays a durable acceptance receipt.
            let mut deliveries = self.deliveries.lock().await;
            while deliveries.try_join_next().is_some() {}
            let active = self.active.lock().map_err(|_| retired())?;
            if *active && deliveries.len() < 100 {
                let node = self.node.clone();
                let destination = request.destination.clone();
                let message_id = accepted.message_id;
                deliveries.spawn(async move {
                    node.flush_accepted_message(&destination, message_id).await;
                });
            }
            Ok(accepted)
        })
    }
    fn with_node<'a>(&'a self, operation: HostOperation<'a>) -> HostFuture<'a, Value> {
        Box::pin(async move {
            let _operation = self.operations.lock().await;
            let active = self.active.lock().map_err(|_| retired())?;
            if !*active {
                return Err(retired());
            }
            operation(&self.node)
        })
    }
    fn accept_invite<'a>(&'a self, channel: ConversationId) -> HostFuture<'a, Value> {
        Box::pin(async move {
            let _operation = self.operations.lock().await;
            if !*self.active.lock().map_err(|_| retired())? {
                return Err(retired());
            }
            self.node.accept_channel_invite(channel).await?;
            Ok(json!({"accepted":true,"channel_id":hex::encode(channel.as_bytes())}))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_talk_core::node::{NodeRuntime, RuntimeConfig, RuntimeEvents};
    #[tokio::test]
    async fn retired_host_rejects_sync_operations() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = NodeRuntime::start_configured(
            RuntimeConfig {
                base_dir: directory.path(),
                account_id: "bot",
                display_name: "Bot",
                password: "test",
                discovery_port: 0,
                post_office_enabled: false,
                candidate_source: None,
            },
            RuntimeEvents {
                on_dm: Box::new(|_| {}),
                on_channel: Box::new(|_| {}),
                on_file: Box::new(|_| {}),
                on_profile: Box::new(|_| {}),
                on_call_signal: Box::new(|_| {}),
            },
        )
        .await
        .unwrap();
        let host = HeadlessHost::new(runtime.handle());
        assert!(
            host.with_node(Box::new(|_| Ok(serde_json::json!({"ok":true}))))
                .await
                .is_ok()
        );
        let (delivery_sender, delivery_stopped) = tokio::sync::oneshot::channel::<()>();
        host.deliveries.lock().await.spawn(async move {
            std::future::pending::<()>().await;
            let _ = delivery_sender.send(());
        });
        host.retire().await;
        assert!(delivery_stopped.await.is_err());
        assert!(host.deliveries.lock().await.is_empty());
        assert_eq!(
            host.accept_invite(ConversationId::new([0; 32]))
                .await
                .unwrap_err()
                .code,
            "SESSION_RETIRED"
        );
        assert_eq!(
            host.with_node(Box::new(|_| panic!("retired host executed operation")))
                .await
                .unwrap_err()
                .code,
            "SESSION_RETIRED"
        );
        runtime.stop().await;
    }
}
