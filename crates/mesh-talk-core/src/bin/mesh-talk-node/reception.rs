//! CLI output advances a fixed durable receipt cursor independently of autosave.
use mesh_talk_core::node::{
    HostReception, Node, NodeError, ParticipantWork, ReceptionConsumer, ReceptionCursor,
    ReceptionLimit,
};
use std::sync::Arc;

pub(super) async fn blocking<T: Send + 'static>(
    work: &ParticipantWork,
    operation: impl FnOnce() -> T + Send + 'static,
) -> Option<T> {
    let mut tasks = tokio::task::JoinSet::new();
    if !work.spawn(&mut tasks, operation) {
        return None;
    }
    tasks.join_next().await?.ok()
}
pub(super) async fn output(
    node: Arc<Node>,
    mut cursor: ReceptionCursor,
    emit: impl Fn(&str) + Send + Sync + 'static,
    work: ParticipantWork,
) {
    let mut changes = node.subscribe_ui_changes();
    let mut through = None;
    loop {
        let reader = node.clone();
        let Some(result) = blocking(&work, move || {
            reader.reception_page(cursor, through, ReceptionLimit::default())
        })
        .await
        else {
            return;
        };
        match result {
            Ok(page) => {
                for receipt in page.receipts {
                    match receipt {
                        HostReception::DirectMessage(dm) => emit(&format!(
                            "from {} ({}): {}",
                            dm.from,
                            dm.from_name,
                            String::from_utf8_lossy(&dm.text)
                        )),
                        HostReception::ChannelMessage(message) if !message.reception_only => {
                            emit(&format!(
                                "channel {} from {}: {}",
                                hex::encode(message.channel_id.as_bytes()),
                                message.from,
                                String::from_utf8_lossy(&message.text)
                            ))
                        }
                        _ => {}
                    }
                }
                let next = page.next;
                let writer = node.clone();
                let Some(ack) = blocking(&work, move || {
                    writer.acknowledge_reception(ReceptionConsumer::CliOutput, cursor, next)
                })
                .await
                else {
                    return;
                };
                if let Err(error) = ack {
                    emit(&format!(
                        "receipt output acknowledgement failed; will retry: {error}"
                    ));
                    retry(&mut changes).await;
                    continue;
                }
                cursor = next;
                through = (!page.complete).then_some(page.through);
                if page.complete {
                    retry(&mut changes).await;
                } else {
                    tokio::task::yield_now().await;
                }
            }
            Err(NodeError::Log(mesh_talk_core::eventlog::LogError::StaleCursor)) => {
                emit("receipt output account changed; restart the node");
                return;
            }
            Err(error) => {
                emit(&format!("receipt output unavailable; will retry: {error}"));
                retry(&mut changes).await;
            }
        }
    }
}
pub(super) async fn retry(changes: &mut tokio::sync::watch::Receiver<u64>) {
    tokio::select! {_ = changes.changed()=>{},_ = tokio::time::sleep(std::time::Duration::from_secs(1))=>{}}
    changes.borrow_and_update();
}
