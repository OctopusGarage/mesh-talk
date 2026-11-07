//! One bounded local-storage turn, independent of account network scheduling.
use super::*;
use crate::node::account_group::operations::AccountGroupDataWorkTurn;

#[derive(Default)]
pub(super) struct LocalMaintenance {
    custody: Option<(EventId, String)>,
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
    }

    pub(super) fn run(
        mut self,
        node: Arc<Node>,
        fanout_cursor: u64,
        select_data: bool,
    ) -> LocalMaintenanceTurn {
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
        node.migrate_channel_custody(&mut self.custody);
        let mut more_inventory = false;
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
            more_inventory =
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
                    more_inventory = false;
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
