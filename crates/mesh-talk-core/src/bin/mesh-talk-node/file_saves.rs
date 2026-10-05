//! Bounded CLI autosaves: a durable file card can precede its immutable chunks.
use mesh_talk_core::{
    eventlog::ConversationId,
    node::{FileProgress, Node, ReceivedFile},
};
use std::{collections::VecDeque, path::PathBuf, sync::Arc, time::Duration};
use tokio::{sync::mpsc, task::JoinSet};

const MAX_FILES: usize = 256;

enum SaveWork {
    Readiness(Vec<(ConversationId, Option<FileProgress>)>),
    Saved(
        ReceivedFile,
        Result<PathBuf, mesh_talk_core::node::NodeError>,
    ),
}

#[derive(Default)]
struct PendingSaves {
    pending: VecDeque<(ReceivedFile, bool)>,
    recent: VecDeque<ConversationId>,
}

impl PendingSaves {
    fn enqueue(&mut self, file: ReceivedFile) -> Result<bool, ()> {
        if self.recent.contains(&file.file_conv)
            || self
                .pending
                .iter()
                .any(|(existing, _)| existing.file_conv == file.file_conv)
        {
            return Ok(false);
        }
        if self.pending.len() >= MAX_FILES {
            return Err(());
        }
        self.pending.push_back((file, false));
        Ok(true)
    }
    fn take_ready(
        &mut self,
        mut progress: impl FnMut(ConversationId) -> Option<FileProgress>,
    ) -> Option<ReceivedFile> {
        for _ in 0..self.pending.len() {
            let (file, saving) = self.pending.pop_front()?;
            if !saving {
                let Some(state) = progress(file.file_conv) else {
                    continue;
                };
                if state.done == state.total {
                    self.pending.push_back((file.clone(), true));
                    return Some(file);
                }
            }
            self.pending.push_back((file, saving));
        }
        None
    }
    fn finish(&mut self, file: ConversationId) {
        self.pending.retain(|(entry, _)| entry.file_conv != file);
        if self.recent.len() == MAX_FILES {
            self.recent.pop_front();
        }
        self.recent.push_back(file);
    }
}

pub(super) async fn run(
    node: Arc<Node>,
    directory: PathBuf,
    mut incoming: mpsc::UnboundedReceiver<ReceivedFile>,
    emit: impl Fn(&str) + Send + Sync + 'static,
) {
    let mut queue = PendingSaves::default();
    let mut saves = JoinSet::new();
    let mut tick = tokio::time::interval(Duration::from_millis(100));
    loop {
        tokio::select! {
            file = incoming.recv() => {
                let Some(file) = file else { break; };
                if queue.enqueue(file.clone()).is_err() {
                    emit(&format!("file from {}: {} ({} bytes) save failed: pending save capacity exhausted", file.from, file.name, file.size));
                }
            }
            _ = tick.tick() => {
                if saves.is_empty() && !queue.pending.is_empty() {
                    let candidates: Vec<_> = queue.pending.iter().filter(|(_, saving)| !saving).map(|(file, _)| file.file_conv).collect();
                    let checking_node = node.clone();
                    saves.spawn_blocking(move || {
                        let mut ready = false;
                        let states = candidates.into_iter().map(|file| {
                            let state = checking_node.file_progress(file).map(|mut progress| {
                                if !ready && checking_node.file_ready_to_save(file) { progress.done = progress.total; ready = true; }
                                progress
                            });
                            (file, state)
                        }).collect();
                        SaveWork::Readiness(states)
                    });
                }
            }
            saved = saves.join_next(), if !saves.is_empty() => {
                match saved {
                    Some(Ok(SaveWork::Readiness(states))) => {
                        if let Some(file) = queue.take_ready(|file| states.iter().find(|(id, _)| *id == file).map(|(_, state)| *state).unwrap_or(Some(FileProgress { done: 0, total: 1 }))) {
                            let saving_node = node.clone(); let destination = directory.clone();
                            saves.spawn_blocking(move || {
                                let result = saving_node.save_file_into_dir(file.file_conv, &destination);
                                SaveWork::Saved(file, result)
                            });
                        }
                    }
                    Some(Ok(SaveWork::Saved(file, result))) => {
                        queue.finish(file.file_conv);
                        if node.file_progress(file.file_conv).is_none() { continue; }
                        match result {
                            Ok(path) => emit(&format!("file from {}: {} ({} bytes) saved {}", file.from, file.name, file.size, path.display())),
                            Err(error) => emit(&format!("file from {}: {} ({} bytes) save failed: {error}", file.from, file.name, file.size)),
                        }
                    }
                    Some(Err(_)) => {
                        // A panicking save task must not keep its slot or spin retries.
                        if let Some(file) = queue.pending.iter().find(|(_, saving)| *saving).map(|(file, _)| file.clone()) {
                            queue.finish(file.file_conv);
                            emit(&format!("file from {}: {} ({} bytes) save failed: save worker failed", file.from, file.name, file.size));
                        } else {
                            for (file, _) in queue.pending.drain(..) {
                                emit(&format!("file from {}: {} ({} bytes) save failed: readiness worker failed", file.from, file.name, file.size));
                            }
                            break;
                        }
                    }
                    None => {}
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn file(n: u8) -> ReceivedFile {
        ReceivedFile {
            conv: ConversationId::new([1; 32]),
            from: "peer".into(),
            name: "fixture.txt".into(),
            size: 1,
            mime: "text/plain".into(),
            file_conv: ConversationId::new([n; 32]),
            media: false,
        }
    }
    fn complete(_: ConversationId) -> Option<FileProgress> {
        Some(FileProgress { done: 1, total: 1 })
    }

    #[tokio::test]
    async fn late_cli_channel_media_callback_exports_verified_managed_copy_after_normal_prune() {
        use mesh_talk_core::{
            discovery::{Announce, Roster},
            identity::{account::Account, device::DeviceIdentity},
        };
        use std::{
            net::{IpAddr, Ipv4Addr},
            sync::Mutex,
        };
        use tokio::net::TcpListener;
        fn open(
            dir: &std::path::Path,
            identity: DeviceIdentity,
            account: Account,
            peer: &Announce,
        ) -> (Arc<Node>, mpsc::UnboundedReceiver<ReceivedFile>) {
            let mut roster = Roster::default();
            roster.update(peer, IpAddr::V4(Ipv4Addr::LOCALHOST), &identity.user_id());
            let (dm, _) = mpsc::unbounded_channel();
            let (channel, _) = mpsc::unbounded_channel();
            let (files, incoming) = mpsc::unbounded_channel();
            (
                Node::open_with_account(
                    identity,
                    account,
                    Arc::new(Mutex::new(roster)),
                    dm,
                    channel,
                    files,
                    &dir.join("events.log"),
                    &dir.join("sent.log"),
                    "pw",
                )
                .unwrap(),
                incoming,
            )
        }
        let ad = tempfile::tempdir().unwrap();
        let bd = tempfile::tempdir().unwrap();
        let alice = DeviceIdentity::generate();
        let aa = Account::generate();
        let bob = DeviceIdentity::generate();
        let ba = Account::generate();
        let bpublic = bob.public();
        let al = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let bl = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ap = Announce::new_with_account(&alice, &aa, "Alice", al.local_addr().unwrap().port());
        let bp = Announce::new_with_account(&bob, &ba, "Bob", bl.local_addr().unwrap().port());
        let (a, _) = open(ad.path(), alice, aa, &bp);
        let (b, incoming) = open(bd.path(), bob, ba, &ap);
        let at = tokio::spawn(a.clone().run_accept_loop(al));
        let bt = tokio::spawn(b.clone().run_accept_loop(bl));
        let channel = a.create_channel("media", vec![bpublic]).await.unwrap();
        let bytes = vec![7; 2048];
        let source = ad.path().join("channel.png");
        std::fs::write(&source, &bytes).unwrap();
        let file = a
            .send_file_channel(channel, &source, mesh_talk_core::file::FileKind::Media)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                b.pull_pending_files().await;
                if b.read_media(file).as_ref() == Some(&bytes)
                    && b.file_progress(file).is_some_and(|p| p.done == 0)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap();
        let destination = bd.path().join("downloads");
        std::fs::create_dir(&destination).unwrap();
        let (output, mut printed) = mpsc::unbounded_channel();
        let runner = tokio::spawn(run(b.clone(), destination.clone(), incoming, move |line| {
            let _ = output.send(line.to_owned());
        }));
        let success = tokio::time::timeout(Duration::from_secs(3), printed.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(
            success.contains(": channel.png (2048 bytes) saved "),
            "{success}"
        );
        assert_eq!(
            std::fs::read(destination.join("channel.png")).unwrap(),
            bytes
        );
        assert_eq!(b.file_progress(file).unwrap().done, 0);
        assert!(
            tokio::time::timeout(Duration::from_millis(250), printed.recv())
                .await
                .is_err()
        );
        runner.abort();
        at.abort();
        bt.abort();
        let _ = runner.await;
        let _ = at.await;
        let _ = bt.await;
    }

    #[test]
    fn incomplete_card_waits_then_complete_card_saves_once_and_deduplicates() {
        let mut queue = PendingSaves::default();
        assert_eq!(queue.enqueue(file(2)), Ok(true));
        assert!(queue
            .take_ready(|_| Some(FileProgress { done: 0, total: 1 }))
            .is_none());
        assert_eq!(queue.enqueue(file(2)), Ok(false));
        assert_eq!(
            queue.take_ready(complete).unwrap().file_conv,
            file(2).file_conv
        );
        queue.finish(file(2).file_conv);
        assert_eq!(queue.enqueue(file(2)), Ok(false));
        assert!(queue.take_ready(complete).is_none());
    }

    #[test]
    fn incomplete_first_file_does_not_block_other_ready_files_or_retry_terminal_failure() {
        let mut queue = PendingSaves::default();
        queue.enqueue(file(2)).unwrap();
        queue.enqueue(file(3)).unwrap();
        let ready = queue
            .take_ready(|conv| {
                Some(FileProgress {
                    done: u32::from(conv == file(3).file_conv),
                    total: 1,
                })
            })
            .unwrap();
        assert_eq!(ready.file_conv, file(3).file_conv);
        queue.finish(ready.file_conv); // A genuine save/verification/I/O error is terminal too.
        assert_eq!(queue.enqueue(file(3)), Ok(false));
        assert_eq!(
            queue.take_ready(complete).unwrap().file_conv,
            file(2).file_conv
        );
        assert!(queue.take_ready(complete).is_none());
    }

    #[test]
    fn bounded_entries_refuse_overflow_and_deleted_or_unknown_cards_release_slots() {
        let mut queue = PendingSaves::default();
        for n in 0..MAX_FILES {
            let mut entry = file(0);
            let mut id = [0; 32];
            id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            entry.file_conv = ConversationId::new(id);
            queue.enqueue(entry).unwrap();
        }
        assert_eq!(queue.enqueue(file(9)), Err(()));
        assert!(queue.take_ready(|_| None).is_none());
        assert_eq!(queue.enqueue(file(9)), Ok(true));
    }

    #[test]
    fn recent_terminal_cache_eviction_never_evicts_waiting_work_or_blocks_new_files() {
        let mut queue = PendingSaves::default();
        queue.enqueue(file(2)).unwrap();
        for n in 0..MAX_FILES + 1 {
            let mut entry = file(0);
            let mut id = [0; 32];
            id[..8].copy_from_slice(&(n as u64).to_le_bytes());
            entry.file_conv = ConversationId::new(id);
            queue.enqueue(entry.clone()).unwrap();
            let ready = queue
                .take_ready(|conv| {
                    if conv == file(2).file_conv {
                        Some(FileProgress { done: 0, total: 1 })
                    } else {
                        complete(conv)
                    }
                })
                .unwrap();
            queue.finish(ready.file_conv);
        }
        assert_eq!(queue.pending.len(), 1);
        assert_eq!(queue.recent.len(), MAX_FILES);
        assert_eq!(
            queue.take_ready(complete).unwrap().file_conv,
            file(2).file_conv
        );
    }

    #[tokio::test]
    async fn actual_save_io_failure_is_reported_once_without_retrying_or_duplicate_callbacks() {
        use mesh_talk_core::{
            discovery::{Announce, Roster},
            identity::{account::Account, device::DeviceIdentity},
        };
        use std::{
            net::{IpAddr, Ipv4Addr},
            sync::Mutex,
        };
        let dir = tempfile::tempdir().unwrap();
        let identity = DeviceIdentity::generate();
        let account = Account::generate();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "Peer", 9);
        let mut roster = Roster::default();
        roster.update(&proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &identity.user_id());
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let node = Node::open_with_account(
            identity,
            account,
            Arc::new(Mutex::new(roster)),
            dm,
            channel,
            files,
            &dir.path().join("events.log"),
            &dir.path().join("sent.log"),
            "pw",
        )
        .unwrap();
        let source = dir.path().join("fixture.txt");
        std::fs::write(&source, b"fixture").unwrap();
        let (_, conv) = node
            .enqueue_file_dm(
                &peer.user_id(),
                &source,
                mesh_talk_core::file::FileKind::File,
            )
            .await
            .unwrap();
        let mut entry = file(2);
        entry.file_conv = conv;
        let blocked = dir.path().join("not-a-directory");
        std::fs::write(&blocked, []).unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let (output, mut printed) = mpsc::unbounded_channel();
        let runner = tokio::spawn(run(node, blocked, rx, move |line| {
            let _ = output.send(line.to_owned());
        }));
        tx.send(entry.clone()).unwrap();
        let failure = tokio::time::timeout(Duration::from_secs(2), printed.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(failure.starts_with("file from peer: fixture.txt (1 bytes) save failed:"));
        tx.send(entry).unwrap();
        assert!(
            tokio::time::timeout(Duration::from_millis(250), printed.recv())
                .await
                .is_err()
        );
        runner.abort();
        let _ = runner.await;
    }
}
