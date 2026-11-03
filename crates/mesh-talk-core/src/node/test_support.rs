//! Shared account-aware loopback fixture. Receive policy keeps production defaults;
//! tests that need the legacy automatic-transfer behavior opt in explicitly.

use super::{Node, ReceivedDm, ReceivedFile};
use crate::discovery::{Announce, Roster};
use crate::identity::{account::Account, device::DeviceIdentity};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::{net::TcpListener, sync::mpsc};

pub(super) fn node_with_files(
    dir: &Path,
    identity: DeviceIdentity,
    account: Account,
    proof: &Announce,
) -> (
    Arc<Node>,
    mpsc::UnboundedReceiver<ReceivedDm>,
    mpsc::UnboundedReceiver<ReceivedFile>,
) {
    let roster = Arc::new(Mutex::new(Roster::default()));
    roster
        .lock()
        .unwrap()
        .update(proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &identity.user_id());
    let (tx, rx) = mpsc::unbounded_channel();
    let (ch, _) = mpsc::unbounded_channel();
    let (file, files) = mpsc::unbounded_channel();
    let node = Node::open_with_account(
        identity,
        account,
        roster,
        tx,
        ch,
        file,
        &dir.join("events.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap();
    (node, rx, files)
}

/// Explicit signed-append fixtures do not run the participant recovery loop.
/// Drive its existing bounded metadata turns before testing a ready consumer.
pub(super) fn settle_group_metadata(node: &Node, group: crate::eventlog::ConversationId) {
    use crate::eventlog::LogError;
    let count = node
        .log
        .lock()
        .unwrap()
        .repository()
        .unwrap()
        .metadata(&group)
        .unwrap()
        .map_or(0, |meta| meta.count);
    let turns = count.saturating_mul(64).saturating_add(1024);
    for _ in 0..turns {
        match node.refresh_channel_control_metadata(group) {
            Ok(()) | Err(LogError::StaleCursor) => {}
            Err(error) => panic!("fixture channel metadata failed: {error}"),
        }
        // A removed physical member has a fresh canonical projection but no
        // local presentation to install. That is completion, not pending work.
        match node.channel_checkpoints.source_binding(group) {
            Ok(Some(_)) => match node.channel_checkpoints.current_state(group) {
                Ok(None) => return,
                Ok(Some(_)) | Err(LogError::StaleCursor) => {}
                Err(error) => panic!("fixture current metadata failed: {error}"),
            },
            Ok(None) | Err(LogError::StaleCursor) => {}
            Err(error) => panic!("fixture canonical metadata failed: {error}"),
        }
        let installed = node.channel_checkpoints.install_prepared_presentation(
            &mut node.channels.lock().unwrap(),
            &node.user_id(),
            group,
        );
        match installed {
            Ok(true) => return,
            Ok(false) | Err(LogError::StaleCursor) => {}
            Err(error) => panic!("fixture channel presentation failed: {error}"),
        }
    }
    panic!("fixture channel metadata did not settle within {turns} bounded turns");
}

// Each simulated remote host owns its reactor and retirement independently.
pub(super) struct IndependentPeerRuntime {
    launch: Option<tokio::sync::oneshot::Sender<()>>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<()>>,
    handle: tokio::runtime::Handle,
}
impl IndependentPeerRuntime {
    pub(super) async fn start(node: Arc<Node>, listener: TcpListener) -> Self {
        let listener = listener.into_std().unwrap();
        let (shutdown, mut stopped) = tokio::sync::oneshot::channel();
        let (launch, launched) = tokio::sync::oneshot::channel();
        let (ready, ready_rx) = tokio::sync::oneshot::channel();
        let thread = std::thread::Builder::new()
            .name("account-action-peer".into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                runtime.block_on(async move {
                    let listener = TcpListener::from_std(listener).unwrap();
                    ready.send(tokio::runtime::Handle::current()).unwrap();
                    let launched = tokio::select! {
                        _ = &mut stopped => false,
                        result = launched => result.is_ok(),
                    };
                    let task = if launched {
                        let task = tokio::spawn(node.clone().run_accept_loop(listener));
                        let _ = stopped.await;
                        Some(task)
                    } else {
                        None
                    };
                    node.runtime_work.close();
                    if let Some(task) = task {
                        task.abort();
                        if let Err(error) = task.await {
                            assert!(
                                error.is_cancelled(),
                                "owned peer server failed before shutdown"
                            );
                        }
                    }
                    node.sessions.clear();
                    node.runtime_work.drain().await;
                });
            })
            .unwrap();
        Self {
            launch: Some(launch),
            shutdown: Some(shutdown),
            thread: Some(thread),
            handle: ready_rx.await.unwrap(),
        }
    }
    pub(super) fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.handle.clone()
    }
    pub(super) fn launch(&mut self) {
        self.launch.take().unwrap().send(()).unwrap();
    }
    pub(super) async fn stop(mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        let thread = self.thread.take().unwrap();
        let joined = tokio::task::spawn_blocking(move || thread.join())
            .await
            .unwrap();
        assert!(
            joined.is_ok(),
            "owned peer runtime must finish its shutdown and writer drain"
        );
    }
}
impl Drop for IndependentPeerRuntime {
    fn drop(&mut self) {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        if let Some(thread) = self.thread.take() {
            let joined = thread.join();
            if !std::thread::panicking() {
                assert!(joined.is_ok(), "owned peer runtime failed during cleanup");
            }
        }
    }
}
