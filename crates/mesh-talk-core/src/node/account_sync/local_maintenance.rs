//! One bounded local-storage turn, independent of account network scheduling.
use super::*;
use crate::node::account_group::operations::AccountGroupDataWorkTurn;

#[derive(Default)]
pub(super) struct LocalMaintenance {
    custody: Option<(EventId, String)>,
    custody_input: Option<CustodyInput>,
    custody_selected: Option<CustodyInput>,
    custody_active: bool,
    custody_repair_page: bool,
    custody_repair_cursor: Option<(EventId, String)>,
    custody_repair_at: Option<std::time::Instant>,
    custody_retry: Option<(Option<CustodyInput>, std::time::Instant)>,
    inventory: Option<ConversationId>,
    inventory_cycle_pending: bool,
    profile: Option<String>,
    profile_projection: Option<AccountDocumentKey>,
    personal: Option<DocumentConversation>,
    personal_retry_at: Option<std::time::Instant>,
    access: Option<ConversationId>,
    operation_data: Option<super::super::AccountGroupOperationId>,
    publication_input: Option<PublicationInput>,
    publication_selected: Option<PublicationInput>,
    publication_repair_at: Option<std::time::Instant>,
    publication_retry: Option<(Option<PublicationInput>, std::time::Instant)>,
    publication_failed: bool,
}

// This is a caller scheduling stamp, never a substitute for document integrity
// or authorization at use. Each owner guard is released before reading the next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct PublicationInput([u8; 32]);
impl PublicationInput {
    fn capture(node: &Node) -> Result<Self, crate::eventlog::LogError> {
        node.group_projection_revision().map(Self)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CustodyInput {
    projection: PublicationInput,
    journal: u64,
}
impl CustodyInput {
    fn capture(node: &Node) -> Result<Self, crate::eventlog::LogError> {
        let projection = PublicationInput::capture(node)?;
        let journal = node
            .channel_custody
            .lock()
            .expect("custody mutex")
            .revision();
        Ok(Self {
            projection,
            journal,
        })
    }
}

const PUBLICATION_REPAIR_INTERVAL: Duration = Duration::from_secs(5 * 60);
const PUBLICATION_FAILURE_RETRY: Duration = Duration::from_secs(5);

pub(super) struct LocalMaintenanceTurn {
    pub state: LocalMaintenance,
    pub more_inventory: bool,
    pub fanout_after: u64,
    pub fanout: Vec<(u64, ConversationId)>,
    pub affected: Vec<(ConversationId, u64)>,
    pub data: Option<AccountGroupDataWorkTurn>,
}

impl LocalMaintenance {
    pub(super) fn publication_deadline(&self) -> Option<std::time::Instant> {
        self.publication_retry
            .map(|(_, at)| at)
            .or(self.publication_repair_at)
            .into_iter()
            .chain(
                self.custody_retry
                    .map(|(_, at)| at)
                    .or(self.custody_repair_at),
            )
            .min()
    }

    fn maintain_custody(&mut self, node: &Node) -> (bool, bool) {
        let input = CustodyInput::capture(node).ok();
        let now = std::time::Instant::now();
        let retry_due = self
            .custody_retry
            .is_none_or(|(previous, at)| previous != input || at <= now);
        let dirty = self.custody_active
            || input.is_none()
            || self.custody_input != input
            || self.custody_repair_at.is_none_or(|at| at <= now);
        if !retry_due || !dirty {
            return (false, false);
        }
        if input.is_none() {
            self.custody_repair_at = None;
            self.custody_retry = Some((None, now + PUBLICATION_FAILURE_RETRY));
            return (false, false);
        }
        if !self.custody_active {
            self.custody_selected = input;
            self.custody_repair_page = self.custody_input == input;
            self.custody_active = true;
        }
        self.custody_retry = None;
        self.custody_repair_at = None;
        let result = if self.custody_repair_page {
            node.migrate_channel_custody(&mut self.custody_repair_cursor)
        } else {
            node.migrate_channel_custody(&mut self.custody)
        };
        match result {
            Ok(_) if self.custody_repair_page => {
                // Integrity repair rotates one page per sparse deadline; a
                // timer alone must never drain the entire historical journal.
                self.custody_selected = None;
                self.custody_active = false;
                self.custody_repair_at =
                    Some(std::time::Instant::now() + PUBLICATION_REPAIR_INTERVAL);
                (false, true)
            }
            Ok(false) => (true, false),
            Ok(true) => {
                self.custody_input = self.custody_selected.take();
                self.custody_active = false;
                self.custody_repair_cursor = None;
                self.custody_repair_at =
                    Some(std::time::Instant::now() + PUBLICATION_REPAIR_INTERVAL);
                (false, true)
            }
            Err(error) => {
                log::debug!("custody migration awaits recovery: {error}");
                self.custody_retry =
                    Some((input, std::time::Instant::now() + PUBLICATION_FAILURE_RETRY));
                (false, false)
            }
        }
    }

    pub(super) fn run(
        mut self,
        node: Arc<Node>,
        fanout_cursor: u64,
        select_data: bool,
    ) -> LocalMaintenanceTurn {
        #[cfg(test)]
        node.trace_account_client(&node.identity.public(), || {
            "account_local_work_begin".into()
        });
        // Capture BEFORE all work, including operation effects that can publish.
        // A commit during this turn then remains dirty even if it completes here.
        let publication_input = PublicationInput::capture(&node).ok();
        let now = std::time::Instant::now();
        let retry_due = self
            .publication_retry
            .is_none_or(|(input, at)| input != publication_input || at <= now);
        let publish = retry_due
            && (self.publication_selected.is_some()
                || publication_input.is_none()
                || self.publication_input != publication_input
                || self.publication_repair_at.is_none_or(|at| at <= now));
        if self
            .personal_retry_at
            .is_none_or(|at| at <= std::time::Instant::now())
            && let Some(conversation) = node.personal_cleanup_next(self.personal.as_ref())
        {
            self.personal = Some(conversation.clone());
            self.personal_retry_at = node
                .retry_personal_cleanup_for(&conversation)
                .is_err()
                .then(|| std::time::Instant::now() + Duration::from_secs(5));
        }
        let affected = node.maintain_account_group_operation_effects();
        let fanout = node.account_group_fanout_page(fanout_cursor, fanout::CAPACITY);
        let affected = affected
            .into_iter()
            .filter_map(|group| {
                node.account_group_fanout_sequence(group)
                    .map(|sequence| (group, sequence))
            })
            .collect();
        let (custody_more, custody_complete) = self.maintain_custody(&node);
        let mut more_inventory = custody_more;
        let mut publication_complete = false;
        if publish {
            self.publication_failed |= publication_input.is_none();
            if self.publication_selected.is_none() {
                self.publication_selected = publication_input;
            }
            self.publication_retry = None;
            // An expired repair deadline cannot keep the parent timer ready
            // while a cold bounded selector or failed cycle is being drained.
            self.publication_repair_at = None;
            let access = node.publish_account_group_access(&mut self.access);
            let inventory = node.publish_group_inventory(&mut self.inventory);
            self.inventory_cycle_pending |= access.is_pending() || inventory.is_pending();
            self.publication_failed |= access.is_failed() || inventory.is_failed();
            more_inventory |=
                self.access.is_some() || self.inventory.is_some() || self.inventory_cycle_pending;
            if self.access.is_none() && self.inventory.is_none() {
                if self.publication_failed {
                    // Keep dirty work without turning an I/O failure into an
                    // immediately repeated local task. New inputs bypass backoff.
                    self.publication_retry = Some((
                        publication_input,
                        std::time::Instant::now() + PUBLICATION_FAILURE_RETRY,
                    ));
                    self.publication_selected = None;
                    self.publication_failed = false;
                    more_inventory = custody_more;
                } else if !self.inventory_cycle_pending {
                    self.publication_input = self.publication_selected.take();
                    publication_complete = true;
                    self.publication_repair_at =
                        Some(std::time::Instant::now() + PUBLICATION_REPAIR_INTERVAL);
                }
                // An unfinished earlier page earns another fair cycle, not an
                // unbounded retry of the first hot group.
                self.inventory_cycle_pending = false;
            }
        }
        if let Err(error) = node.publish_public_profile_documents_page(&mut self.profile) {
            log::debug!("contact profile publication deferred: {error}");
        }
        {
            let _commit = node.account_sync_commit.lock().expect("sync commit mutex");
            if let Err(error) =
                node.project_public_profile_documents_page(&mut self.profile_projection)
            {
                log::debug!("contact profile projection deferred: {error}");
            }
        }
        let data = select_data.then(|| {
            let turn = node.account_group_data_work(self.operation_data);
            self.operation_data = turn.after;
            if turn.more {
                node.account_sync_notify.notify_one();
            }
            turn
        });
        if custody_complete {
            more_inventory |= CustodyInput::capture(&node).ok() != self.custody_input;
        }
        if publication_complete {
            // Only the selected stamp was acknowledged. Observe newer inputs
            // solely to retain one follow-up, including independent work above.
            more_inventory |= PublicationInput::capture(&node).ok() != self.publication_input;
        }
        LocalMaintenanceTurn {
            state: self,
            more_inventory,
            fanout_after: fanout_cursor,
            fanout,
            affected,
            data,
        }
    }
}

/// A completed read-only selection reserves only currently available slots.
/// Skipped jobs remain durable and must not pin the rotating scan's next page.
pub(super) fn reserve_data<T>(
    work: impl IntoIterator<Item = T>,
    available: usize,
    active: &mut HashSet<String>,
    retry: &HashMap<String, std::time::Instant>,
    now: std::time::Instant,
    worker_key: impl Fn(&T) -> String,
) -> Vec<T> {
    let mut reserved = Vec::new();
    for job in work {
        if reserved.len() == available {
            break;
        }
        let key = worker_key(&job);
        if !active.contains(&key) && retry.get(&key).is_none_or(|at| *at <= now) {
            active.insert(key);
            reserved.push(job);
        }
    }
    reserved
}

/// The parent may have returned overflow ownership to an earlier durable row
/// while this scan ran. Observing its bounded snapshot cannot erase that rewind.
pub(super) fn observe_fanout_turn(
    queue: &mut fanout::FanoutQueue,
    cursor: &mut u64,
    started_after: u64,
    page: Vec<(u64, ConversationId)>,
    affected: Vec<(ConversationId, u64)>,
) {
    let advance = *cursor == started_after;
    for (sequence, group) in page {
        if !queue.observe(group, sequence) {
            break;
        }
        if advance {
            *cursor = sequence;
        }
    }
    for (group, sequence) in affected {
        if !queue.observe(group, sequence) {
            *cursor = (*cursor).min(sequence.saturating_sub(1));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_local_repair_actual_owners_wait_for_cancelled_profile_worker() {
        shared_local_repair_actual_owners(false).await;
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn shared_local_repair_actual_owners_cancel_queued_account_on_retirement() {
        shared_local_repair_actual_owners(true).await;
    }

    async fn shared_local_repair_actual_owners(retire_waiter: bool) {
        use futures_util::FutureExt;
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        let (_directory, node, peer) = super::super::selection_tests::sibling_nodes();
        let (entered, held) = tokio::sync::oneshot::channel();
        let entered = Mutex::new(Some(entered));
        let (release, released) = std::sync::mpsc::channel::<()>();
        let released = Mutex::new(released);
        let (scheduled, queued) = tokio::sync::oneshot::channel();
        let scheduled = Mutex::new(Some(scheduled));
        let (account_entered, account_ready) = tokio::sync::mpsc::unbounded_channel();
        let (dirty, dirty_seen) = tokio::sync::oneshot::channel();
        let dirty = Mutex::new(Some(dirty));
        let turns = Arc::new(AtomicUsize::new(0));
        let observed = turns.clone();
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new(move |_, phase| {
            if phase == "profile_local_work_begin" {
                if let Some(entered) = entered.lock().unwrap().take() {
                    let _ = entered.send(());
                    let result = released
                        .lock()
                        .unwrap()
                        .recv_timeout(Duration::from_secs(15));
                    assert!(
                        result.is_ok()
                            || matches!(
                                result,
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected)
                            ),
                        "actual profile worker release must stay bounded"
                    );
                }
            } else if phase == "account_local_work_scheduled" {
                if let Some(scheduled) = scheduled.lock().unwrap().take() {
                    let _ = scheduled.send(());
                }
            } else if phase == "account_local_work_begin" {
                observed.fetch_add(1, Ordering::SeqCst);
                let _ = account_entered.send(());
            } else if phase == "account_local_work_dirty"
                && let Some(dirty) = dirty.lock().unwrap().take()
            {
                let _ = dirty.send(());
            }
        }));
        let profile = tokio::spawn(node.clone().run_profile_compaction_loop());
        let mut account = None;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let serving = tokio::spawn(node.clone().run_inbound_connections_for_test(listener));
        let mut account_ready = account_ready;
        let outcome = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(Duration::from_secs(3), held).await
                .expect("actual profile work must start").unwrap();
            account = Some(tokio::spawn(node.clone().run_account_history_loop_with_tick(None)));
            tokio::time::timeout(Duration::from_secs(3), queued).await
                .expect("account parent must schedule its real local child").unwrap();
            node.account_sync_notify.notify_one();
            tokio::time::timeout(Duration::from_secs(3), dirty_seen).await
                .expect("account parent must retain dirty demand while its child waits").unwrap();
            // A live foreground Noise and paged exchange must remain independent
            // of the held local repair; no worker helper stands in for network work.
            tokio::time::timeout(Duration::from_secs(3), async {
                let mut channel = peer.privacy_sync_dial(address, &node.identity.public()).await.unwrap();
                let store = peer.sync_store(&node.identity.public());
                let conversation = super::super::super::conversation::dm_conversation_id(
                    &peer.identity.public(), &node.identity.public());
                super::super::super::session::request_round_paged(&mut channel, &store, conversation)
                    .await.unwrap();
            }).await.expect("foreground exchange must progress while local repair is held");
            let before_cancel = turns.load(Ordering::SeqCst);
            profile.abort();
            for _ in 0..32 { tokio::task::yield_now().await; }
            let after_cancel = turns.load(Ordering::SeqCst);
            if retire_waiter {
                node.runtime_work.close();
                account.as_ref().unwrap().abort();
            }
            drop(release);
            if !retire_waiter {
                tokio::time::timeout(Duration::from_secs(3), async {
                    for _ in 0..2 { account_ready.recv().await.unwrap(); }
                }).await.expect("queued account work and retained dirty followup must run after actual profile worker retires");
            }
            (before_cancel, after_cancel)
        }).catch_unwind().await;
        // Channel closure releases the held worker even if setup/assertion fails.
        // Always release real blocking work before joining/draining its owners.
        profile.abort();
        let _ = profile.await;
        if let Some(account) = account {
            account.abort();
            let _ = account.await;
        }
        serving.abort();
        let _ = serving.await;
        node.runtime_work.close();
        tokio::time::timeout(Duration::from_secs(5), node.runtime_work.drain())
            .await
            .expect("actual local and inbound work must retire");
        *node.account_client_test_hook.lock().unwrap() = None;
        let (before_cancel, after_cancel) = match outcome {
            Ok(counts) => counts,
            Err(panic) => std::panic::resume_unwind(panic),
        };
        assert_eq!(
            before_cancel, 0,
            "account scan must queue behind actual held profile work"
        );
        assert_eq!(
            after_cancel, 0,
            "observer cancellation must not release actual worker capacity"
        );
        if retire_waiter {
            assert_eq!(
                turns.load(Ordering::SeqCst),
                0,
                "retired queued owner cannot start work"
            );
        }
    }

    #[tokio::test]
    async fn unchanged_roster_heartbeat_does_not_admit_local_maintenance() {
        use crate::discovery::{Announce, Roster};
        use crate::identity::{account::Account, device::DeviceIdentity};
        use std::sync::{
            Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::sync::mpsc;

        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let node = Node::open_with_account(
            DeviceIdentity::generate(),
            Account::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            files,
            &dir.path().join("events"),
            &dir.path().join("sent"),
            "pw",
        )
        .unwrap();
        let peer = DeviceIdentity::generate();
        let announce = Announce::new(&peer, "Unrelated online peer", 21212);
        let address = "127.0.0.1".parse().unwrap();
        node.roster
            .lock()
            .unwrap()
            .update(&announce, address, &node.user_id());
        node.roster.lock().unwrap().update(
            &announce,
            "127.0.0.2".parse().unwrap(),
            &node.user_id(),
        );
        let turns = Arc::new(AtomicUsize::new(0));
        let idle = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let turns = turns.clone();
            let idle = idle.clone();
            let observed = observed.clone();
            move |_, phase| {
                if phase.starts_with("operation_owner phase=legacy_publication_end ") {
                    turns.fetch_add(1, Ordering::AcqRel);
                }
                if let Some(state) = phase.strip_prefix("account_owner_local_state idle=") {
                    idle.store(state == "true", Ordering::Release);
                }
                if phase == "account_owner_roster_observed" {
                    // A previous idle marker cannot certify the cycle which
                    // has only just consumed this new roster observation.
                    idle.store(false, Ordering::Release);
                    observed.fetch_add(1, Ordering::AcqRel);
                }
            }
        }));
        async fn drive_to_idle(
            owner: &mut std::pin::Pin<Box<impl std::future::Future<Output = ()>>>,
            idle: &std::sync::atomic::AtomicBool,
            observed: &AtomicUsize,
            minimum: usize,
        ) {
            tokio::time::timeout(
                Duration::from_secs(30),
                futures_util::future::poll_fn(|cx| {
                    assert!(owner.as_mut().poll(cx).is_pending());
                    if idle.load(Ordering::Acquire) && observed.load(Ordering::Acquire) >= minimum {
                        std::task::Poll::Ready(())
                    } else {
                        std::task::Poll::Pending
                    }
                }),
            )
            .await
            .unwrap();
        }
        // Observe the actual parent's idle state: retirement of blocking work
        // alone does not consume its JoinSet result or a retained startup wake.
        let mut owner = Box::pin(node.clone().run_account_history_loop_with_tick(None));
        drive_to_idle(&mut owner, &idle, &observed, 0).await;
        let before = turns.load(Ordering::Acquire);
        assert!(before > 0);
        let next_observation = observed.load(Ordering::Acquire) + 1;
        node.roster
            .lock()
            .unwrap()
            .update(&announce, address, &node.user_id());
        drive_to_idle(&mut owner, &idle, &observed, next_observation).await;
        assert_eq!(
            turns.load(Ordering::Acquire),
            before,
            "fresh-to-fresh discovery must not restart local publication/operation maintenance"
        );

        for index in 0..4 {
            let next_observation = observed.load(Ordering::Acquire) + 1;
            let address = if index % 2 == 0 {
                "127.0.0.2"
            } else {
                "127.0.0.1"
            };
            node.roster.lock().unwrap().update(
                &announce,
                address.parse().unwrap(),
                &node.user_id(),
            );
            drive_to_idle(&mut owner, &idle, &observed, next_observation).await;
            assert_eq!(
                turns.load(Ordering::Acquire),
                before,
                "alternating already-known routes must not admit account maintenance"
            );
        }

        let moved = Announce::new(&peer, "Unrelated online peer", 21213);
        let next_observation = observed.load(Ordering::Acquire) + 1;
        node.roster
            .lock()
            .unwrap()
            .update(&moved, address, &node.user_id());
        drive_to_idle(&mut owner, &idle, &observed, next_observation).await;
        let moved_turns = turns.load(Ordering::Acquire);
        assert!(
            moved_turns > before,
            "an authenticated endpoint change must still admit local recovery"
        );
        let renamed = Announce::new(&peer, "Changed signed name", 21213);
        let next_observation = observed.load(Ordering::Acquire) + 1;
        node.roster
            .lock()
            .unwrap()
            .update(&renamed, address, &node.user_id());
        drive_to_idle(&mut owner, &idle, &observed, next_observation).await;
        assert!(
            turns.load(Ordering::Acquire) > moved_turns,
            "changed signed metadata must wake without new route retry credit"
        );

        // A gateway's signed listening port is distinct from the actual local
        // gateway endpoint. Conflicted history is excluded by the real roster,
        // leaving this valid signed change observable only in the live proof.
        let gateway_device = DeviceIdentity::generate();
        let first_account = Account::generate();
        let next_account = Account::generate();
        let first_proof = Announce::new_post_office_with_account(
            &gateway_device,
            &first_account,
            "Gateway peer",
            23232,
        );
        let live_proof = Announce::new_post_office_with_account(
            &gateway_device,
            &next_account,
            "Gateway peer",
            23232,
        );
        let changed_proof = Announce::new_post_office_with_account(
            &gateway_device,
            &next_account,
            "Gateway peer",
            23233,
        );
        assert!(first_proof.verify() && live_proof.verify() && changed_proof.verify());
        let next_observation = observed.load(Ordering::Acquire) + 1;
        {
            let mut roster = node.roster.lock().unwrap();
            assert!(
                roster
                    .update_gateway(&first_proof, address, 24242, &node.user_id())
                    .accepted()
            );
            assert!(
                roster
                    .update_gateway(&live_proof, address, 24242, &node.user_id())
                    .accepted()
            );
            assert!(
                roster
                    .historical_announcement(&gateway_device.public())
                    .is_none()
            );
            assert_eq!(
                roster.announcement(&gateway_device.public()),
                Some(&live_proof)
            );
        }
        drive_to_idle(&mut owner, &idle, &observed, next_observation).await;
        let gateway_turns = turns.load(Ordering::Acquire);
        let next_observation = observed.load(Ordering::Acquire) + 1;
        {
            let mut roster = node.roster.lock().unwrap();
            let before = roster.get(&gateway_device.user_id()).unwrap().clone();
            assert!(
                roster
                    .update_gateway(&changed_proof, address, 24242, &node.user_id())
                    .accepted()
            );
            let after = roster.get(&gateway_device.user_id()).unwrap();
            assert_eq!(before.public, after.public);
            assert_eq!(before.account_id, after.account_id);
            assert_eq!(before.name, after.name);
            assert_eq!(before.post_office, after.post_office);
            assert_eq!(before.addr, after.addr);
            assert_eq!(
                before
                    .routes
                    .iter()
                    .map(|route| route.addr)
                    .collect::<Vec<_>>(),
                after
                    .routes
                    .iter()
                    .map(|route| route.addr)
                    .collect::<Vec<_>>()
            );
            assert_eq!(before.presence_deadline(std::time::Instant::now()), None);
            assert_eq!(after.presence_deadline(std::time::Instant::now()), None);
            assert!(
                roster
                    .historical_announcement(&gateway_device.public())
                    .is_none()
            );
            assert_eq!(
                roster.announcement(&gateway_device.public()),
                Some(&changed_proof)
            );
        }
        drive_to_idle(&mut owner, &idle, &observed, next_observation).await;
        assert!(
            turns.load(Ordering::Acquire) > gateway_turns,
            "a changed live signed proof must wake even when history and projected routes cannot expose it"
        );
        drop(owner);
        node.runtime_work.close();
        tokio::time::timeout(Duration::from_secs(30), node.runtime_work.drain())
            .await
            .unwrap();
    }

    async fn settle_publication(node: &Arc<Node>, mut state: LocalMaintenance) -> LocalMaintenance {
        tokio::time::timeout(Duration::from_secs(30), async {
            let mut quiet = 0;
            loop {
                let turn = state.run(node.clone(), 0, false);
                state = turn.state;
                quiet = if turn.more_inventory { 0 } else { quiet + 1 };
                if quiet == 4 {
                    break state;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap()
    }

    async fn custody_fixture() -> (tempfile::TempDir, Arc<Node>, ConversationId, EventId) {
        let (directory, node, _sibling) = super::super::selection_tests::sibling_nodes();
        let channel = node
            .create_channel("custody producer", vec![])
            .await
            .unwrap();
        node.send_channel_message(channel, b"custody migration")
            .await
            .unwrap();
        let event = node
            .channel_history(channel, 20)
            .unwrap()
            .into_iter()
            .find(|row| row.text == b"custody migration")
            .unwrap()
            .id;
        node.channel_custody
            .lock()
            .unwrap()
            .confirm(event, &node.user_id())
            .unwrap();
        (directory, node, channel, event)
    }

    #[tokio::test]
    async fn unchanged_local_custody_migration_does_not_rebuild_certificates() {
        let (_directory, node, channel, event) = custody_fixture().await;
        let reads = node
            .log
            .lock()
            .unwrap()
            .repository()
            .unwrap()
            .body_read_counts();
        let mut cold_cursor = None;
        assert!(!node.migrate_channel_custody(&mut cold_cursor).unwrap());
        assert_eq!(
            node.log
                .lock()
                .unwrap()
                .repository()
                .unwrap()
                .body_read_counts(),
            reads,
            "cold required custody migration only needs authoritative scope metadata"
        );
        let mut state = settle_publication(&node, LocalMaintenance::default()).await;
        let before = node.custody_certificate_builds();
        assert!(
            before > 0,
            "fixture must run real custody certificate publication"
        );
        for _ in 0..8 {
            state = state.run(node.clone(), 0, false).state;
        }
        assert_eq!(
            node.custody_certificate_builds(),
            before,
            "unchanged local turns must not cycle the historical custody journal"
        );
        let revision = node.channel_custody.lock().unwrap().revision();
        node.channel_custody
            .lock()
            .unwrap()
            .confirm(event, &node.user_id())
            .unwrap();
        assert_eq!(
            node.channel_custody.lock().unwrap().revision(),
            revision,
            "no-op confirmations do not invalidate migration"
        );
        state = state.run(node.clone(), 0, false).state;
        assert_eq!(node.custody_certificate_builds(), before);
        node.channel_custody
            .lock()
            .unwrap()
            .revoke(event, &node.user_id())
            .unwrap();
        assert!(node.channel_custody.lock().unwrap().revision() > revision);
        state = settle_publication(&node, state).await;
        let key = AccountDocumentKey::ChannelCustody {
            channel: hex::encode(channel.as_bytes()),
            event: hex::encode(event.as_bytes()),
            holder: node.user_id(),
        };
        assert_eq!(
            state.custody_input.unwrap().journal,
            node.channel_custody.lock().unwrap().revision(),
            "changed journal must complete a fresh migration cycle"
        );
        assert!(
            !node.account_document(&key).unwrap().unwrap().meta.tombstone,
            "legacy migration must preserve the newer signed positive observation"
        );
        node.channel_custody
            .lock()
            .unwrap()
            .confirm(event, &node.user_id())
            .unwrap();
        let _state = settle_publication(&node, state).await;
        assert!(!node.account_document(&key).unwrap().unwrap().meta.tombstone);
        assert!(node.custody_certificate_builds() > before);
    }

    #[tokio::test]
    async fn sparse_custody_repair_visits_only_one_page_and_retains_failed_cursor() {
        let (_directory, node, channel, _event) = custody_fixture().await;
        for _ in 0..64 {
            node.append_event(channel, crate::eventlog::EventKind::Message, vec![0; 64])
                .unwrap();
        }
        let author = crate::eventlog::Author::from_ed25519(node.identity.public().ed25519_pub);
        let keys = node
            .log
            .lock()
            .unwrap()
            .message_keys_after(channel, author, None, 128)
            .unwrap();
        assert_eq!(
            keys.len(),
            65,
            "the fixture must contain more than two real journal pages"
        );
        for (_, event) in keys {
            node.channel_custody
                .lock()
                .unwrap()
                .confirm(event, &node.user_id())
                .unwrap();
        }
        node.await_channel_metadata(channel).await.unwrap();
        let mut state = settle_publication(&node, LocalMaintenance::default()).await;
        let before = node.custody_certificate_builds();
        state.custody_repair_at = Some(std::time::Instant::now());
        let turn = state.run(node.clone(), 0, false);
        assert!(
            !turn.more_inventory,
            "an integrity deadline must not start a full historical drain"
        );
        state = turn.state;
        let after_page = node.custody_certificate_builds();
        assert!(after_page > before && after_page - before <= 32);
        let cursor = state.custody_repair_cursor.clone();
        assert!(cursor.is_some());
        for _ in 0..8 {
            state = state.run(node.clone(), 0, false).state;
        }
        assert_eq!(node.custody_certificate_builds(), after_page);
        let next = node
            .channel_custody
            .lock()
            .unwrap()
            .observations_after(cursor.as_ref())
            .unwrap()[0]
            .0
            .0;
        node.log.lock().unwrap().fail_read_for(Some(next));
        state.custody_repair_at = Some(std::time::Instant::now());
        state = state.run(node.clone(), 0, false).state;
        assert_eq!(
            state.custody_repair_cursor, cursor,
            "failed scope read must retain the repair page"
        );
        assert!(state.custody_retry.is_some());
        state = state.run(node.clone(), 0, false).state;
        assert_eq!(node.custody_certificate_builds(), after_page);
        node.log.lock().unwrap().fail_read_for(None);
        state.custody_retry.as_mut().unwrap().1 = std::time::Instant::now();
        let turn = state.run(node.clone(), 0, false);
        assert!(!turn.more_inventory);
        assert_ne!(turn.state.custody_repair_cursor, cursor);
        assert!(node.custody_certificate_builds() - after_page <= 32);
    }

    #[tokio::test]
    async fn failed_custody_migration_keeps_the_unpublished_cursor() {
        let (_directory, node, _channel, _event) = custody_fixture().await;
        node.account_documents
            .lock()
            .unwrap()
            .fail_index_for_test(true);
        let mut after = None;
        assert!(node.migrate_channel_custody(&mut after).is_err());
        assert!(
            after.is_none(),
            "failed publication must not advance the durable observation cursor"
        );
        let turn = LocalMaintenance::default().run(node.clone(), 0, false);
        assert!(turn.state.custody_retry.is_some());
        let certificates = node.custody_certificate_builds();
        let mut state = turn.state.run(node.clone(), 0, false).state;
        assert_eq!(
            node.custody_certificate_builds(),
            certificates,
            "ordinary local turns must respect failed migration backoff"
        );
        node.account_documents
            .lock()
            .unwrap()
            .fail_index_for_test(false);
        state.custody_retry.as_mut().unwrap().1 = std::time::Instant::now();
        let state = settle_publication(&node, state).await;
        assert!(state.custody_retry.is_none());
        assert!(state.custody_input.is_some());
    }

    #[tokio::test]
    async fn unchanged_local_publication_reads_no_document_bodies_after_settle() {
        let (_directory, source, _receiver) = super::super::selection_tests::sibling_nodes();
        for index in 0..3 {
            source
                .create_channel(&format!("Quiet publication {index}"), vec![])
                .await
                .unwrap();
        }
        let state = settle_publication(&source, LocalMaintenance::default()).await;
        let before = source.account_documents.lock().unwrap().body_read_count();
        assert!(
            before > 0,
            "fixture must exercise real document body hydration"
        );
        let turn = state.run(source.clone(), 0, false);
        assert!(!turn.more_inventory);
        assert_eq!(
            source.account_documents.lock().unwrap().body_read_count(),
            before,
            "an unchanged routine local turn must select before reading document blobs"
        );
    }

    #[tokio::test]
    async fn local_publication_tracks_profiles_proofs_documents_policy_and_newer_dirty_work() {
        use crate::node::account_documents::GroupInventoryReference;
        let (_directory, source, sibling) = super::super::selection_tests::sibling_nodes();
        let group = source
            .create_channel("Publication inputs", vec![])
            .await
            .unwrap();
        let mut state = settle_publication(&source, LocalMaintenance::default()).await;
        let key = AccountDocumentKey::GroupInventory {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        source
            .set_profile_display_name("New inventory label")
            .unwrap();
        let before = source.account_documents.lock().unwrap().body_read_count();
        state = settle_publication(&source, state).await;
        assert!(source.account_documents.lock().unwrap().body_read_count() > before);
        let document = source.account_document(&key).unwrap().unwrap();
        let reference: GroupInventoryReference =
            serde_json::from_slice(document.payload()).unwrap();
        assert!(
            reference
                .members
                .iter()
                .any(|member| member.name.as_deref() == Some("New inventory label"))
        );

        let proof = sibling.signed_announce("Sibling proof", 21212);
        source.roster.lock().unwrap().update(
            &proof,
            "127.0.0.1".parse().unwrap(),
            &source.user_id(),
        );
        let before = source.account_documents.lock().unwrap().body_read_count();
        state = settle_publication(&source, state).await;
        assert!(
            source.account_documents.lock().unwrap().body_read_count() > before,
            "a changed effective signed proof must invalidate publication"
        );
        // Identical signed heartbeats change freshness only.
        source.roster.lock().unwrap().update(
            &proof,
            "127.0.0.1".parse().unwrap(),
            &source.user_id(),
        );
        let before = source.account_documents.lock().unwrap().body_read_count();
        state = state.run(source.clone(), 0, false).state;
        assert_eq!(
            source.account_documents.lock().unwrap().body_read_count(),
            before
        );

        source
            .set_device_hostname(Some("Changed document input"))
            .unwrap();
        let before = source.account_documents.lock().unwrap().body_read_count();
        state = settle_publication(&source, state).await;
        assert!(source.account_documents.lock().unwrap().body_read_count() > before);
        source
            .set_device_sync_category(SyncCategory::History, false)
            .unwrap();
        let before = source.account_documents.lock().unwrap().body_read_count();
        state = settle_publication(&source, state).await;
        assert!(source.account_documents.lock().unwrap().body_read_count() > before);

        // Hold the actual inventory profile read after input selection. A newer
        // durable commit inside the turn must retain a follow-up without a tick.
        source
            .set_device_hostname(Some("Start a selected turn"))
            .unwrap();
        *source.profile_read_hook.lock().unwrap() = Some(Box::new({
            let source = source.clone();
            move || {
                source
                    .set_device_hostname(Some("Committed during selected turn"))
                    .unwrap();
            }
        }));
        let turn = state.run(source.clone(), 0, false);
        assert!(
            turn.more_inventory,
            "newer committed work must retain one local follow-up"
        );
        assert_ne!(
            turn.state.publication_input,
            PublicationInput::capture(&source).ok(),
            "only the selected-before-work stamp may be acknowledged"
        );
        state = settle_publication(&source, turn.state).await;
        assert_eq!(
            state.publication_input,
            PublicationInput::capture(&source).ok()
        );

        source
            .rename_channel(group, "Changed public group")
            .await
            .unwrap();
        state = settle_publication(&source, state).await;
        let document = source.account_document(&key).unwrap().unwrap();
        let reference: GroupInventoryReference =
            serde_json::from_slice(document.payload()).unwrap();
        assert_eq!(reference.name, "Changed public group");
        // Sparse repair is a caller boundary: direct reads still authenticate
        // durable bytes immediately, while a due pass re-enters the publishers.
        state.publication_repair_at = Some(std::time::Instant::now());
        let before = source.account_documents.lock().unwrap().body_read_count();
        let turn = state.run(source.clone(), 0, false);
        assert!(source.account_documents.lock().unwrap().body_read_count() > before);
        assert!(turn.state.publication_deadline().unwrap() > std::time::Instant::now());
    }

    #[tokio::test]
    async fn failed_local_publication_retains_dirty_work_without_hot_retry() {
        let (_directory, source, _sibling) = super::super::selection_tests::sibling_nodes();
        let group = source
            .create_channel("Failed publication", vec![])
            .await
            .unwrap();
        let mut state = settle_publication(&source, LocalMaintenance::default()).await;
        source
            .account_documents
            .lock()
            .unwrap()
            .fail_index_for_test(true);
        source.set_profile_display_name("Retry this label").unwrap();
        state = settle_publication(&source, state).await;
        assert!(state.publication_retry.is_some());
        assert_ne!(
            state.publication_input,
            PublicationInput::capture(&source).ok()
        );
        let before = source.account_documents.lock().unwrap().body_read_count();
        let turn = state.run(source.clone(), 0, false);
        assert!(!turn.more_inventory);
        assert_eq!(
            source.account_documents.lock().unwrap().body_read_count(),
            before,
            "a failed publication must retain demand without a hot retry"
        );
        state = turn.state;
        source
            .account_documents
            .lock()
            .unwrap()
            .fail_index_for_test(false);
        state.publication_retry.as_mut().unwrap().1 = std::time::Instant::now();
        state = settle_publication(&source, state).await;
        assert!(state.publication_retry.is_none());
        let key = AccountDocumentKey::GroupInventory {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        let document = source.account_document(&key).unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(document.payload()).unwrap();
        assert!(
            value["members"]
                .as_array()
                .unwrap()
                .iter()
                .any(|member| member["name"] == "Retry this label")
        );
    }

    #[tokio::test]
    async fn local_publication_retains_selected_input_across_pages_and_newer_mutation() {
        let (_directory, source, _sibling) = super::super::selection_tests::sibling_nodes();
        let mut groups = Vec::new();
        for index in 0..17 {
            groups.push(
                source
                    .create_channel(&format!("Selected page {index}"), vec![])
                    .await
                    .unwrap(),
            );
        }
        let state = settle_publication(&source, LocalMaintenance::default()).await;
        source
            .set_device_hostname(Some("Start a bounded publication cycle"))
            .unwrap();
        let selected = PublicationInput::capture(&source).ok();
        let first = state.run(source.clone(), 0, false);
        assert!(first.more_inventory);
        assert!(first.state.inventory.is_some());
        assert_eq!(first.state.publication_selected, selected);
        source
            .set_profile_display_name("Committed between publication pages")
            .unwrap();
        let second = first.state.run(source.clone(), 0, false);
        assert!(second.state.inventory.is_none());
        assert_eq!(
            second.state.publication_input, selected,
            "the last bounded page may acknowledge only the original selected stamp"
        );
        assert!(
            second.more_inventory,
            "the newer generation retains one full-cycle follow-up"
        );
        assert_ne!(
            second.state.publication_input,
            PublicationInput::capture(&source).ok()
        );
        let state = settle_publication(&source, second.state).await;
        assert_eq!(
            state.publication_input,
            PublicationInput::capture(&source).ok()
        );
        for group in groups {
            let key = AccountDocumentKey::GroupInventory {
                channel: hex::encode(group.as_bytes()),
                source_device: source.user_id(),
            };
            let document = source.account_document(&key).unwrap().unwrap();
            let value: serde_json::Value = serde_json::from_slice(document.payload()).unwrap();
            assert!(
                value["members"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .any(|member| member["name"] == "Committed between publication pages")
            );
        }
    }

    #[tokio::test]
    async fn sparse_local_publication_repairs_missing_blob_and_retains_unreadable_source_floor() {
        let (directory, source, _sibling) = super::super::selection_tests::sibling_nodes();
        let group = source
            .create_channel("Sparse publication repair", vec![])
            .await
            .unwrap();
        let mut state = settle_publication(&source, LocalMaintenance::default()).await;
        let key = AccountDocumentKey::GroupInventory {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        let original = source.account_document(&key).unwrap().unwrap();
        let blob = directory
            .path()
            .join("source")
            .join(format!("account-document-blobs-{}", source.account_id()))
            .join("media")
            .join(format!("{}.bin", hex::encode(original.meta.content_hash)));
        assert!(
            blob.is_file(),
            "fixture must select the real encrypted inventory blob"
        );
        std::fs::remove_file(&blob).unwrap();
        assert!(
            source.account_document(&key).is_err(),
            "direct reads still fail immediately"
        );
        let before = source.account_documents.lock().unwrap().body_read_count();
        state = state.run(source.clone(), 0, false).state;
        assert_eq!(
            source.account_documents.lock().unwrap().body_read_count(),
            before,
            "external damage alone does not turn quiet ticks into body reads"
        );
        state.publication_repair_at = Some(std::time::Instant::now());
        state = settle_publication(&source, state).await;
        let restored = source.account_document(&key).unwrap().unwrap();
        assert_eq!(restored.meta, original.meta);
        assert_eq!(restored.payload(), original.payload());

        let encrypted = std::fs::read(&blob).unwrap();
        let mut corrupt = encrypted.clone();
        *corrupt.last_mut().unwrap() ^= 0x80;
        std::fs::write(&blob, &corrupt).unwrap();
        assert_eq!(
            std::fs::metadata(&blob).unwrap().len(),
            encrypted.len() as u64
        );
        assert!(source.account_document(&key).is_err());
        // A newer canonical input prevents reconstructing the exact unreadable
        // prior head. Retain its signed floor rather than guessing a replacement.
        source
            .set_profile_display_name("After exact floor restoration")
            .unwrap();
        state.publication_repair_at = Some(std::time::Instant::now());
        state = settle_publication(&source, state).await;
        assert!(state.publication_retry.is_some());
        assert!(source.account_document(&key).is_err());
        assert_eq!(source.account_document_meta(&key).unwrap(), original.meta);
        assert_eq!(std::fs::read(&blob).unwrap(), corrupt);
        std::fs::write(&blob, &encrypted).unwrap();
        assert_eq!(
            source.account_document(&key).unwrap().unwrap().meta,
            original.meta
        );
        state.publication_retry.as_mut().unwrap().1 = std::time::Instant::now();
        state = settle_publication(&source, state).await;
        assert!(state.publication_retry.is_none());
        let document = source.account_document(&key).unwrap().unwrap();
        let value: serde_json::Value = serde_json::from_slice(document.payload()).unwrap();
        assert!(
            value["members"]
                .as_array()
                .unwrap()
                .iter()
                .any(|member| member["name"] == "After exact floor restoration")
        );
    }

    #[tokio::test]
    async fn actual_failed_publication_retries_at_owned_deadline_without_periodic_tick() {
        use futures_util::FutureExt;
        let (directory, source, _sibling) = super::super::selection_tests::sibling_nodes();
        let group = source
            .create_channel("Owned publication retry", vec![])
            .await
            .unwrap();
        let _settled = settle_publication(&source, LocalMaintenance::default()).await;
        let key = AccountDocumentKey::GroupInventory {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        let previous = source.account_document_meta(&key).unwrap();
        source
            .rename_channel(group, "Inventory write requires retry")
            .await
            .unwrap();
        // Qualify access and directory readiness before injecting an inventory
        // write failure, independently of cold creation/presentation recovery.
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let access = source.publish_account_group_access(&mut None);
                let directory = {
                    let log = source.log.lock().unwrap();
                    source.directory_observation(&log, group)
                };
                let directory_ready = match directory {
                    Ok(_) => true,
                    Err(crate::eventlog::LogError::StaleCursor) => false,
                    Err(crate::eventlog::LogError::Io(error))
                        if error.kind() == std::io::ErrorKind::WouldBlock =>
                    {
                        false
                    }
                    Err(error) => panic!("directory retry fixture preparation failed: {error}"),
                };
                if !access.is_pending() && !access.is_failed() && directory_ready {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(source.account_document_meta(&key).unwrap(), previous);
        source
            .account_documents
            .lock()
            .unwrap()
            .fail_index_for_test(true);
        let failed_at = Arc::new(std::sync::Mutex::new(None));
        let phases = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
        *source.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let source = source.clone();
            let failed_at = failed_at.clone();
            let phases = phases.clone();
            move |_, phase| {
                let mut trace = phases.lock().unwrap();
                if trace.len() == 64 {
                    trace.pop_front();
                }
                trace.push_back(phase.clone());
                drop(trace);
                if phase == "inventory_publication_deferred outcome=error" {
                    failed_at
                        .lock()
                        .unwrap()
                        .get_or_insert_with(std::time::Instant::now);
                    // Restore the actual failed storage owner without a semantic
                    // mutation or wake: its retained deadline must own the retry.
                    source
                        .account_documents
                        .lock()
                        .unwrap()
                        .fail_index_for_test(false);
                }
            }
        }));
        while source
            .account_sync_notify
            .notified()
            .now_or_never()
            .is_some()
        {}
        let mut owner = Box::pin(source.clone().run_account_history_loop_with_tick(None));
        let outcome = std::panic::AssertUnwindSafe(async {
            let result = tokio::time::timeout(
                Duration::from_secs(30),
                futures_util::future::poll_fn(|cx| {
                    assert!(owner.as_mut().poll(cx).is_pending());
                    if source
                        .account_document_meta(&key)
                        .is_some_and(|meta| meta != previous)
                    {
                        std::task::Poll::Ready(())
                    } else {
                        std::task::Poll::Pending
                    }
                }),
            )
            .await;
            assert!(
                result.is_ok(),
                "failed local publication must retain its own retry wake; phases={:?}",
                phases.lock().unwrap()
            );
            let failed_at = failed_at
                .lock()
                .unwrap()
                .expect("actual index failure must be observed");
            assert!(
                failed_at.elapsed() >= PUBLICATION_FAILURE_RETRY,
                "an error must not become a hot immediate local continuation"
            );
        })
        .catch_unwind()
        .await;
        drop(owner);
        *source.account_client_test_hook.lock().unwrap() = None;
        source
            .account_documents
            .lock()
            .unwrap()
            .fail_index_for_test(false);
        source.runtime_work.close();
        let retired = tokio::time::timeout(Duration::from_secs(5), source.runtime_work.drain())
            .await
            .is_ok();
        if !retired {
            let _ = directory.keep();
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
        assert!(retired, "publication owner must retire before its store");
    }

    #[tokio::test]
    async fn cold_inventory_directory_work_does_not_request_network_refresh_before_publication() {
        use futures_util::FutureExt;
        let (_directory, source, _receiver) = super::super::selection_tests::sibling_nodes();
        let group = source
            .create_channel("Local directory continuation", vec![])
            .await
            .unwrap();
        let key = AccountDocumentKey::GroupInventory {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        assert!(source.account_document_meta(&key).is_none());
        // Creation is a real durable change. Consume its wake before isolating
        // the independent, unfinished local directory selector.
        while source
            .account_sync_notify
            .notified()
            .now_or_never()
            .is_some()
        {}
        assert!(matches!(
            source.try_publish_group_inventory_groups(&[group]),
            Err(NodeError::Log(crate::eventlog::LogError::Io(error)))
                if error.kind() == std::io::ErrorKind::WouldBlock
        ));
        assert!(source.account_document_meta(&key).is_none());
        assert!(
            source
                .account_sync_notify
                .notified()
                .now_or_never()
                .is_none(),
            "unfinished local directory work must not refresh remote document lanes"
        );
    }

    #[tokio::test]
    async fn local_inventory_full_page_retries_pending_groups_without_pinning_later_groups() {
        let (_directory, source, _receiver) = super::super::selection_tests::sibling_nodes();
        let mut groups = Vec::new();
        for index in 0..16 {
            groups.push(
                source
                    .create_channel(&format!("Inventory page {index}"), vec![])
                    .await
                    .unwrap(),
            );
        }
        groups.sort_by_key(|group| *group.as_bytes());
        let key = |group: ConversationId| AccountDocumentKey::GroupInventory {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        let last = *groups.last().unwrap();
        // Only the last real group's selector is ready. Its publication must
        // not wait for the first group's independent, cold selector.
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let result = {
                    let log = source.log.lock().unwrap();
                    source.directory_observation(&log, last)
                };
                match result {
                    Ok(_) => break,
                    Err(crate::eventlog::LogError::StaleCursor) => {}
                    Err(crate::eventlog::LogError::Io(error))
                        if error.kind() == std::io::ErrorKind::WouldBlock => {}
                    Err(error) => panic!("real directory preparation failed: {error}"),
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(source.account_document_meta(&key(last)).is_none());
        let first = LocalMaintenance::default().run(source.clone(), 0, false);
        assert!(source.account_document_meta(&key(groups[0])).is_none());
        assert!(
            source.account_document_meta(&key(last)).is_some(),
            "a pending first group must not block the later ready group"
        );
        assert!(first.more_inventory);
        assert_eq!(first.state.inventory, Some(last));
        // Exactly one full page ends on an empty page. That page must retain
        // the earlier groups' unfinished work for another fair cycle.
        let empty = first.state.run(source.clone(), 0, false);
        assert!(empty.state.inventory.is_none());
        assert!(empty.more_inventory);
        let mut state = empty.state;
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let turn = state.run(source.clone(), 0, false);
                state = turn.state;
                if !turn.more_inventory {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        for group in groups {
            let document = source.account_document(&key(group)).unwrap().unwrap();
            assert_eq!(document.meta.writer_device, source.user_id());
            assert_eq!(document.meta.key, key(group));
        }
    }

    #[tokio::test]
    async fn actual_local_inventory_continuations_publish_without_periodic_or_peer_refresh() {
        use futures_util::FutureExt;
        let (directory, source, _receiver) = super::super::selection_tests::sibling_nodes();
        let group = source
            .create_channel("Owned inventory continuation", vec![])
            .await
            .unwrap();
        let key = AccountDocumentKey::GroupInventory {
            channel: hex::encode(group.as_bytes()),
            source_device: source.user_id(),
        };
        while source
            .account_sync_notify
            .notified()
            .now_or_never()
            .is_some()
        {}
        // No linked device has a route and there is no periodic repair tick.
        // Only the real parent owns and consumes the local continuation.
        let mut owner = Box::pin(source.clone().run_account_history_loop_with_tick(None));
        let outcome = std::panic::AssertUnwindSafe(async {
            tokio::time::timeout(
                Duration::from_secs(30),
                futures_util::future::poll_fn(|cx| {
                    assert!(owner.as_mut().poll(cx).is_pending());
                    if source.account_document_meta(&key).is_some() {
                        std::task::Poll::Ready(())
                    } else {
                        std::task::Poll::Pending
                    }
                }),
            )
            .await
            .expect("owned local turns must finish the real signed inventory");
            let document = source.account_document(&key).unwrap().unwrap();
            assert_eq!(document.meta.key, key);
            assert_eq!(document.meta.writer_device, source.user_id());
        })
        .catch_unwind()
        .await;
        drop(owner);
        source.runtime_work.close();
        let retired = tokio::time::timeout(Duration::from_secs(5), source.runtime_work.drain())
            .await
            .is_ok();
        if !retired {
            let _ = directory.keep();
        }
        if let Err(panic) = outcome {
            std::panic::resume_unwind(panic);
        }
        assert!(
            retired,
            "local inventory owner must retire before its store"
        );
    }

    #[tokio::test]
    async fn actual_local_turn_coalesces_inflight_wakes_without_completion_driven_work() {
        use crate::discovery::Roster;
        use crate::identity::{account::Account, device::DeviceIdentity};
        use std::sync::{
            Condvar, Mutex,
            atomic::{AtomicUsize, Ordering},
        };
        use tokio::sync::{Notify, mpsc};
        struct Release(Arc<(Mutex<bool>, Condvar)>);
        impl Drop for Release {
            fn drop(&mut self) {
                *self.0.0.lock().unwrap() = true;
                self.0.1.notify_all();
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let (dm, _) = mpsc::unbounded_channel();
        let (channel, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let node = Node::open_with_account(
            DeviceIdentity::generate(),
            Account::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            files,
            &dir.path().join("events"),
            &dir.path().join("sent"),
            "pw",
        )
        .unwrap();
        let gate = Arc::new((Mutex::new(false), Condvar::new()));
        let _release = Release(gate.clone());
        let entered = Arc::new(Notify::new());
        let turns = Arc::new(AtomicUsize::new(0));
        let completions = Arc::new(AtomicUsize::new(0));
        *node.account_client_test_hook.lock().unwrap() = Some(Arc::new({
            let gate = gate.clone();
            let entered = entered.clone();
            let turns = turns.clone();
            let completions = completions.clone();
            move |_, phase| {
                if phase == "operation_owner phase=local_turn_completed" {
                    completions.fetch_add(1, Ordering::AcqRel);
                }
                if phase.starts_with("operation_owner phase=legacy_publication_end ")
                    && turns.fetch_add(1, Ordering::AcqRel) == 0
                {
                    let mut released = gate.0.lock().unwrap();
                    entered.notify_one();
                    while !*released {
                        released = gate.1.wait(released).unwrap();
                    }
                }
            }
        }));
        // No periodic tick or peers can manufacture another ordinary turn.
        // Manual polling distinguishes actual worker completion from parent
        // consumption without sleeps or a separate coordination hook.
        let mut owner = Box::pin(node.clone().run_account_history_loop_with_tick(None));
        assert!(futures_util::poll!(owner.as_mut()).is_pending());
        tokio::time::timeout(Duration::from_secs(30), entered.notified())
            .await
            .unwrap();
        node.account_sync_notify.notify_one();
        node.account_sync_notify.notify_one();
        assert!(futures_util::poll!(owner.as_mut()).is_pending());
        *gate.0.lock().unwrap() = true;
        gate.1.notify_all();
        // Retirement is not JoinSet result delivery. Drive the actual owner
        // with the current waker until it consumes both completed turns, then
        // inspect admission after the rest of that parent poll has run.
        tokio::time::timeout(
            Duration::from_secs(30),
            futures_util::future::poll_fn(|cx| {
                assert!(owner.as_mut().poll(cx).is_pending());
                if completions.load(Ordering::Acquire) >= 2 {
                    std::task::Poll::Ready(())
                } else {
                    std::task::Poll::Pending
                }
            }),
        )
        .await
        .unwrap();
        // If completion wrongly starts a third queued closure, its actual
        // admission must retire before this counter is accepted.
        tokio::time::timeout(Duration::from_secs(30), node.runtime_work.drain())
            .await
            .unwrap();
        assert_eq!(
            turns.load(Ordering::Acquire),
            2,
            "inflight wakes retain one turn; local completion alone retains none"
        );
        drop(owner);
        node.runtime_work.close();
        tokio::time::timeout(Duration::from_secs(30), node.runtime_work.drain())
            .await
            .unwrap();
        *node.account_client_test_hook.lock().unwrap() = None;
    }

    #[tokio::test]
    async fn capacity_change_at_selection_handoff_revisits_durable_work_on_ordinary_turn() {
        let (selected, received) = tokio::sync::oneshot::channel();
        let durable = vec!["peer-a".to_owned()];
        selected.send(durable.clone()).unwrap();
        let mut active = HashSet::new();
        let now = std::time::Instant::now();
        let retry = HashMap::new();
        assert!(
            reserve_data(
                received.await.unwrap(),
                0,
                &mut active,
                &retry,
                now,
                Clone::clone
            )
            .is_empty()
        );
        // The result's skipped selection is not a queue or destructive read.
        // A real capacity-release wake admits the next ordinary durable scan.
        assert_eq!(
            reserve_data(durable, 1, &mut active, &retry, now, Clone::clone),
            ["peer-a"]
        );
    }

    #[test]
    fn one_peer_batch_cannot_pin_later_peer_or_bypass_current_retry() {
        let now = std::time::Instant::now();
        let retry_at = now + RETRY_INTERVAL;
        let mut active = HashSet::new();
        let retry = HashMap::from([("peer-a".to_owned(), retry_at)]);
        let first = vec!["peer-a".to_owned(); 128];
        assert!(reserve_data(first, 2, &mut active, &retry, now, Clone::clone).is_empty());
        // Keep the source's advanced cursor: the next bounded page can use the
        // free slot rather than waiting for all of A's prior files to finish.
        assert_eq!(
            reserve_data(
                ["peer-b".to_owned()],
                2,
                &mut active,
                &retry,
                now,
                Clone::clone
            ),
            ["peer-b"]
        );
        assert!(
            reserve_data(
                ["peer-b".to_owned()],
                1,
                &mut active,
                &retry,
                now,
                Clone::clone
            )
            .is_empty()
        );
        assert_eq!(
            reserve_data(
                ["peer-a".to_owned()],
                1,
                &mut active,
                &retry,
                retry_at,
                Clone::clone
            ),
            ["peer-a"]
        );
    }

    #[test]
    fn stale_fanout_handoff_preserves_concurrent_overflow_rewind() {
        let mut queue = fanout::FanoutQueue::default();
        for sequence in 1..=fanout::CAPACITY {
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&(sequence as u64).to_be_bytes());
            assert!(queue.observe(ConversationId::new(bytes), sequence as u64));
        }
        let started_after = 500;
        // Overflow returned an earlier revision to the durable source, then an
        // existing fanout freed one queue slot before the scan result arrived.
        let mut cursor = 12;
        assert!(queue.start_next().is_some());
        observe_fanout_turn(
            &mut queue,
            &mut cursor,
            started_after,
            vec![(501, ConversationId::new([250; 32]))],
            vec![],
        );
        assert_eq!(
            cursor, 12,
            "a stale completed scan cannot skip the returned durable prefix"
        );
    }
}
