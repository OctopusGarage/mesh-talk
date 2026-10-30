//! Bounded direct and channel history catch-up between certified account devices.

use super::account_preferences::AccountPreference;
use super::account_sync_policy::{DeviceSyncPolicy, SyncCategory};
use super::account_sync_wire::{
    AccountHistoryFrame, AccountSyncVersion, HistoryCursor, MAX_CHUNK, MAX_RECORD,
};
use super::conversation::account_conversation_id;
use super::dm_envelope::DmEnvelope;
use super::node::NodeError;
use super::pairing::BackfillRecord;
use super::session::SessionError;
use super::*;
use crate::discovery::{Announce, roster::PeerRecord};
use crate::eventlog::event::{ConversationId, Event, EventId};
use crate::identity::device::PublicIdentity;
use crate::transport::SecureChannel;
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpStream;

#[cfg(test)]
use super::account_sync_wire::VERSION;

/// A selected plaintext page or staged file retains its original expiry context.
enum PrivateContent<'a> {
    Record(&'a HistoryRecord),
    Attachment(&'a SelectedAttachment),
}

/// The original authorized manifest behind one staged stream. A later alias
/// of the same file conversation cannot replace this record or its lifetime.
struct SelectedAttachment {
    file: ConversationId,
    row: super::received_log::ReceivedEntry,
    manifest: crate::file::AnyManifest,
    scope: Option<archive::PrivateChannelScope>,
}

mod archive;
mod fanout;
mod favorites;
mod wake;
pub(in crate::node) use wake::AccountWake;
mod page;
pub(in crate::node) use archive::ArchiveCache;
#[cfg(test)]
mod personal_replay_tests;
#[cfg(test)]
mod selection_tests;
use page::{ChannelRecord, FileCardRecord, HistoryPageAssembler, HistoryRecord};
#[cfg(test)]
use page::{PageBuilder, PendingRecord};

const RECORDS_PER_PAGE: usize = 16;
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const RETRY_INTERVAL: Duration = Duration::from_secs(10);
const ATTACHMENT_AVAILABILITY_TTL: Duration = Duration::from_secs(60);
const MAX_UNAVAILABLE_ATTACHMENTS: usize = 256;

/// Mandatory document admission respects its retained page retry deadline.
/// Optional workers also own history/favorites and keep their independent scheduling.
pub(in crate::node) fn take_ready_mandatory_peer<K>(
    queue: &mut std::collections::VecDeque<(K, PeerRecord)>,
    retry_at: impl Fn(&K) -> Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<(K, PeerRecord)> {
    let ready = queue
        .iter()
        .position(|(id, _)| retry_at(id).is_none_or(|at| at <= now))?;
    queue.remove(ready)
}

pub(in crate::node) fn mandatory_retry_deadline<K>(
    queue: &std::collections::VecDeque<(K, PeerRecord)>,
    retry_at: impl Fn(&K) -> Option<std::time::Instant>,
) -> Option<std::time::Instant> {
    queue.iter().filter_map(|(id, _)| retry_at(id)).min()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncPhase {
    Offline,
    CatchingUp,
    UpToDate,
    WaitingForFiles,
    Error,
    Disabled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct DeviceSyncStatus {
    pub device_id: String,
    pub phase: SyncPhase,
    pub pending_attachments: usize,
    pub legacy_file_metadata: bool,
    pub document_phase: SyncPhase,
    pub document_error: Option<String>,
    pub document_requires_upgrade: bool,
    pub error: Option<String>,
}

#[derive(Default)]
struct PeerCursor {
    history_retry_at: Option<std::time::Instant>,
    favorite_next: Option<String>,
    favorite_digest: [u8; 32],
    favorite_sweep_digest: [u8; 32],
    favorites_supported: bool,
    favorite_error: Option<String>,
    favorite_retry_at: Option<std::time::Instant>,
    next: Option<HistoryCursor>,
    known_digest: [u8; 32],
    sweep_digest: [u8; 32],
    next_file: usize,
}

fn initial_sync_status(device_id: String) -> DeviceSyncStatus {
    DeviceSyncStatus {
        device_id,
        phase: SyncPhase::CatchingUp,
        pending_attachments: 0,
        legacy_file_metadata: false,
        document_phase: SyncPhase::CatchingUp,
        document_error: None,
        document_requires_upgrade: false,
        error: None,
    }
}

type DocumentStatus = (SyncPhase, Option<String>, bool);

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum DocumentLane {
    Control,
    Directory,
    Optional,
}
impl DocumentLane {
    const ALL: [Self; 3] = [Self::Control, Self::Directory, Self::Optional];
    fn index(self) -> usize {
        match self {
            Self::Control => 0,
            Self::Directory => 1,
            Self::Optional => 2,
        }
    }
    fn cursor(self) -> super::account_documents::sync::DocumentSyncCursor {
        match self {
            Self::Control => super::account_documents::sync::DocumentSyncCursor::control(),
            Self::Directory => super::account_documents::sync::DocumentSyncCursor::directory(),
            Self::Optional => super::account_documents::sync::DocumentSyncCursor::optional(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
struct DocumentPeer {
    device_id: String,
    ed25519_pub: [u8; 32],
    x25519_pub: [u8; 32],
    lane: DocumentLane,
}
impl DocumentPeer {
    fn new(public: &PublicIdentity, lane: DocumentLane) -> Self {
        Self {
            device_id: public.user_id(),
            ed25519_pub: public.ed25519_pub,
            x25519_pub: public.x25519_pub,
            lane,
        }
    }
    fn public(&self) -> PublicIdentity {
        PublicIdentity {
            ed25519_pub: self.ed25519_pub,
            x25519_pub: self.x25519_pub,
        }
    }
    fn identity(&self) -> ([u8; 32], [u8; 32]) {
        (self.ed25519_pub, self.x25519_pub)
    }
}

// Hints order ordinary first turns but confer no authority or reservation. One
// owned-preemption replacement and its resumed checkpoint are protected, so a
// newer sender can interrupt a natural hint turn once without a cancellation
// storm or starving the interrupted comparison. Local explicit sources win.
fn control_turn_protected(resumed: bool, replacement: bool) -> bool {
    resumed || replacement
}

fn control_wake_preempts(
    active: wake::WakeIdentity,
    protected: bool,
    waiting: Option<wake::WakeIdentity>,
) -> bool {
    !protected && waiting.is_some_and(|peer| peer != active)
}

fn control_queue_priority(
    selected: bool,
    foreground: bool,
    resumed: bool,
    hint: Option<usize>,
) -> (u8, usize) {
    if selected {
        (0, 0)
    } else if foreground {
        (1, 0)
    } else if resumed {
        (2, 0)
    } else if let Some(position) = hint {
        (3, position)
    } else {
        (4, 0)
    }
}

// Each certified Control peer can temporarily own two metadata checkpoints,
// but still has only one admitted page. The parked checkpoint never owns bytes.
type ControlDemand = std::collections::BTreeSet<super::AccountGroupOperationId>;

/// Scheduling evidence travels with the protocol checkpoint through queues,
/// task rollback and paired page swaps. It never authorizes a public effect.
#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct ControlComparisonStart {
    requests: ControlDemand,
    wake: Option<wake::ControlWakeHint>,
}
impl ControlComparisonStart {
    fn predates(&self, requests: &ControlDemand, wake: Option<&wake::ControlWakeHint>) -> bool {
        !requests.is_subset(&self.requests)
            || wake.is_some_and(|wake| self.wake.as_ref() != Some(wake))
    }
}

#[derive(Clone)]
#[cfg_attr(test, derive(Debug, PartialEq, Eq))]
struct DocumentCheckpoint {
    cursor: super::account_documents::sync::DocumentSyncCursor,
    control_start: Option<ControlComparisonStart>,
}
impl From<super::account_documents::sync::DocumentSyncCursor> for DocumentCheckpoint {
    fn from(cursor: super::account_documents::sync::DocumentSyncCursor) -> Self {
        Self {
            cursor,
            control_start: None,
        }
    }
}
impl std::ops::Deref for DocumentCheckpoint {
    type Target = super::account_documents::sync::DocumentSyncCursor;
    fn deref(&self) -> &Self::Target {
        &self.cursor
    }
}
impl std::ops::DerefMut for DocumentCheckpoint {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.cursor
    }
}
impl DocumentCheckpoint {
    fn begin_refresh(&mut self) -> bool {
        if !self.cursor.begin_refresh() {
            return false;
        }
        self.control_start = None;
        true
    }
    fn fresh_control_comparison(&self) -> Self {
        self.cursor.fresh_control_comparison().into()
    }
    fn refresh_timed_out_control_for_requests(
        &mut self,
        peer: &PublicIdentity,
        requests: Option<&ControlDemand>,
        wake: Option<&wake::ControlWakeHint>,
    ) -> bool {
        let Some(wake) = wake.filter(|wake| wake.public == *peer) else {
            return false;
        };
        if requests.is_none_or(|requests| requests.is_empty())
            || !self.cursor.refresh_timed_out_control(wake.received_at)
        {
            return false;
        }
        self.control_start = None;
        true
    }
    fn predates_control_demand(
        &self,
        requests: &ControlDemand,
        wake: Option<&wake::ControlWakeHint>,
    ) -> bool {
        self.control_start
            .as_ref()
            .is_some_and(|start| start.predates(requests, wake))
    }
    fn start_control_comparison(
        &mut self,
        requests: &ControlDemand,
        wake: Option<&wake::ControlWakeHint>,
    ) {
        if self.control_start.is_none() {
            self.control_start = Some(ControlComparisonStart {
                requests: requests.clone(),
                wake: wake.cloned(),
            });
        }
    }
}

/// Cross-peer scheduling credits belong only to the initial replacement and
/// exact interrupted resume. A later same-peer refresh retains its checkpoints,
/// but does not renew either credit ahead of another authenticated sender.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ControlPairCredit {
    Prefix,
    Resume,
    Repaid,
}

struct ControlComparisonPair {
    credit: ControlPairCredit,
    parked: DocumentCheckpoint,
    foreground: bool,
    followup: bool,
    local_requests: ControlDemand,
    wake: Option<wake::ControlWakeHint>,
}
impl ControlComparisonPair {
    fn new(
        background: DocumentCheckpoint,
        local_requests: ControlDemand,
        wake: Option<wake::ControlWakeHint>,
    ) -> Self {
        Self {
            credit: ControlPairCredit::Prefix,
            parked: background,
            foreground: true,
            followup: false,
            local_requests,
            wake,
        }
    }
    fn prefix_turn_owed(&self) -> bool {
        self.credit == ControlPairCredit::Prefix && self.foreground
    }
    fn resume_turn_owed(&self) -> bool {
        self.credit == ControlPairCredit::Resume && !self.foreground
    }
    fn current_turn_owed(&self) -> bool {
        self.prefix_turn_owed() || self.resume_turn_owed()
    }
    fn observe(&mut self, local_requests: ControlDemand, wake: Option<&wake::ControlWakeHint>) {
        if !local_requests.is_subset(&self.local_requests) {
            self.followup = true;
        }
        self.local_requests = local_requests;
        if let Some(wake) = wake
            && self.wake.as_ref() != Some(wake)
        {
            self.wake = Some(wake.clone());
            self.followup = true;
        }
    }
    fn deadline(&self, current: Option<&DocumentCheckpoint>) -> Option<std::time::Instant> {
        self.parked
            .control_retry_deadline()
            .into_iter()
            .chain(current.and_then(|state| state.control_retry_deadline()))
            .max()
    }
    fn finish_page(&mut self, current: &mut DocumentCheckpoint) -> bool {
        // A rejected admission or failed read did not pay for a page turn.
        // Keep this owner and its checkpoint until a validated Complete.
        if !current.control_page_completed() {
            return false;
        }
        if self.prefix_turn_owed() {
            self.credit = if self.parked.control_comparison_complete() {
                ControlPairCredit::Repaid
            } else {
                ControlPairCredit::Resume
            };
        } else if self.resume_turn_owed() {
            self.credit = ControlPairCredit::Repaid;
        }
        let foreground = if self.foreground {
            &mut *current
        } else {
            &mut self.parked
        };
        if self.followup && foreground.control_comparison_complete() {
            let refreshed = foreground.begin_refresh();
            debug_assert!(refreshed);
            self.followup = false;
        }
        let current_complete = current.control_comparison_complete();
        let parked_complete = self.parked.control_comparison_complete();
        if self.followup
            && ((self.foreground && !current_complete && parked_complete)
                || (!self.foreground && current_complete && !parked_complete))
        {
            // A new publication need not wait for a stale foreground suffix
            // when the other comparison has already completed. Reuse only that
            // completed slot; preserve the unfinished checkpoint as background.
            if self.foreground {
                std::mem::swap(current, &mut self.parked);
            }
            let refreshed = current.begin_refresh();
            debug_assert!(refreshed);
            self.foreground = true;
            self.followup = false;
            // The fresh prefix runs next. Its validated page then repays the
            // exact parked continuation through ordinary page alternation.
            return false;
        }
        if current.control_comparison_complete() && self.parked.control_comparison_complete() {
            // Retain the foreground's independently verified comparison, never
            // combine after keys, digest sets or support booleans.
            let settled = current.up_to_date && self.parked.up_to_date;
            if !self.foreground {
                std::mem::swap(current, &mut self.parked);
            }
            current.up_to_date = settled;
            return true;
        }
        if !self.parked.control_comparison_complete() {
            std::mem::swap(current, &mut self.parked);
            self.foreground = !self.foreground;
        }
        false
    }
    fn status(&self, current: &DocumentCheckpoint, policy: DeviceSyncPolicy) -> DocumentStatus {
        let current = current.status(policy);
        let parked = self.parked.status(policy);
        if current.0 == SyncPhase::Error {
            return (current.0, current.1, current.2 || parked.2);
        }
        if parked.0 == SyncPhase::Error {
            return (parked.0, parked.1, current.2 || parked.2);
        }
        if current.0 == SyncPhase::Disabled {
            return current;
        }
        if parked.0 == SyncPhase::Disabled {
            return parked;
        }
        (SyncPhase::CatchingUp, None, current.2 || parked.2)
    }
}

fn document_sync_status(
    statuses: &HashMap<DocumentPeer, DocumentStatus>,
    public: &PublicIdentity,
) -> DocumentStatus {
    let lanes = DocumentLane::ALL.map(|lane| statuses.get(&DocumentPeer::new(public, lane)));
    let mandatory = combined_document_status(lanes[0], lanes[1]);
    combined_document_status(lanes[2], Some(&mandatory))
}

fn combined_document_status(
    optional: Option<&DocumentStatus>,
    mandatory: Option<&DocumentStatus>,
) -> DocumentStatus {
    let pending = (SyncPhase::CatchingUp, None, false);
    let optional = optional.unwrap_or(&pending);
    let mandatory = mandatory.unwrap_or(&pending);
    let phases = [optional.0, mandatory.0];
    let phase = if phases.contains(&SyncPhase::Error) {
        SyncPhase::Error
    } else if phases.contains(&SyncPhase::Disabled) {
        SyncPhase::Disabled
    } else if phases.iter().all(|phase| *phase == SyncPhase::UpToDate) {
        SyncPhase::UpToDate
    } else {
        SyncPhase::CatchingUp
    };
    let error = if mandatory.0 == SyncPhase::Error {
        mandatory.1.clone()
    } else if optional.0 == SyncPhase::Error {
        optional.1.clone()
    } else {
        mandatory.1.clone().or_else(|| optional.1.clone())
    };
    (phase, error, optional.2 || mandatory.2)
}

#[derive(Debug, PartialEq, Eq)]
enum AttachmentOutcome {
    Complete,
    Unavailable,
    SourceDisabled,
}

fn optional_attachment_closed(error: &NodeError) -> bool {
    matches!(error, NodeError::Session(SessionError::Transport(error)) if super::session_pool::retryable_transport_error(error))
}

pub(in crate::node) fn local_sync_capacity_busy(error: &NodeError) -> bool {
    matches!(error, NodeError::Session(SessionError::Transport(error))
        if super::session_pool::local_capacity_busy(error))
}

impl PeerCursor {
    /// Continue forward through a changing archive. A changed sweep gets one
    /// fresh sweep afterward to collect new IDs behind the exclusive cursor.
    fn complete_page(&mut self, next: Option<HistoryCursor>, digest: [u8; 32]) -> bool {
        match next {
            Some(next) => {
                if self.next.is_none() {
                    self.sweep_digest = digest;
                }
                self.next = Some(next);
                self.known_digest = [0; 32];
                false
            }
            None => {
                let complete = self.next.is_none() || self.sweep_digest == digest;
                self.next = None;
                self.known_digest = if complete { digest } else { [0; 32] };
                complete
            }
        }
    }
}

#[derive(Serialize, Deserialize)]
struct FileCardPayload {
    manifest: Vec<u8>,
    proof: Event,
}

#[derive(Serialize)]
struct FileCardPayloadRef<'a> {
    manifest: &'a [u8],
    proof: &'a Event,
}

pub(in crate::node) fn attachment_deadline(size: u64) -> Duration {
    Duration::from_secs(30).saturating_add(Duration::from_secs(size.div_ceil(16 * 1024)))
}

fn transport(error: crate::transport::TransportError) -> NodeError {
    NodeError::Session(SessionError::Transport(error))
}

async fn send_frame(
    channel: &mut SecureChannel<TcpStream>,
    version: AccountSyncVersion,
    frame: &AccountHistoryFrame,
) -> Result<(), NodeError> {
    let bytes = frame
        .encode_version(version)
        .ok_or_else(|| NodeError::Channel("invalid account-history frame".into()))?;
    channel.send(&bytes).await.map_err(transport)
}

impl Node {
    /// Keep disk/scan work out of the shared accept/delivery task. Retirement
    /// holds its permit inside the writer even if the awaiting future is cancelled.
    pub(in crate::node) async fn account_blocking<T: Send + 'static>(
        self: &Arc<Self>,
        work: impl FnOnce(Arc<Self>) -> Result<T, NodeError> + Send + 'static,
    ) -> Result<T, NodeError> {
        let permit = self
            .runtime_work
            .admit()
            .ok_or_else(|| NodeError::Channel("participant runtime is stopping".into()))?;
        let node = self.clone();
        tokio::task::spawn_blocking(move || {
            let _permit = permit;
            work(node)
        })
        .await
        .map_err(|error| NodeError::Channel(format!("account sync worker: {error}")))?
    }
    pub(in crate::node) async fn send_account_frame(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        revision: u64,
        frame: &AccountHistoryFrame,
    ) -> Result<(), NodeError> {
        #[cfg(test)]
        if matches!(
            frame,
            AccountHistoryFrame::DocumentHeader { .. }
                | AccountHistoryFrame::DocumentsComplete { .. }
        ) {
            let hook = self.account_session_test_hook.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(channel.peer_identity().clone(), frame.clone()).await;
            }
        }
        if self
            .delivery_suspended
            .load(std::sync::atomic::Ordering::Acquire)
        {
            return Err(NodeError::Authorization("account source retired".into()));
        }
        if self.device_sync_policy_snapshot().1 != revision {
            return Err(NodeError::Authorization(
                "account sync policy changed".into(),
            ));
        }
        send_frame(channel, version, frame).await
    }

    async fn send_account_history_frame(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        revision: u64,
        controls: (u64, [u8; 32]),
        frame: &AccountHistoryFrame,
    ) -> Result<(), NodeError> {
        if !self.account_private_sibling_current(channel.peer_identity())
            || !self.account_allowed(channel.peer_identity(), channel.peer_announcement())
        {
            return Err(NodeError::Authorization(
                "account source binding changed".into(),
            ));
        }
        if self.private_dm_control_revision() != controls {
            return Err(NodeError::Authorization(
                "account recall state changed".into(),
            ));
        }
        self.send_account_frame(channel, version, revision, frame)
            .await
    }

    fn history_record_conversation(&self, record: &HistoryRecord) -> Option<DocumentConversation> {
        match record {
            HistoryRecord::Direct(row) => {
                let envelope = DmEnvelope::decode(&row.plaintext)?;
                let own = self.account_id();
                let peer = if envelope.route.sender_account == own {
                    envelope.route.recipient_account
                } else if envelope.route.recipient_account == own {
                    envelope.route.sender_account
                } else {
                    return None;
                };
                (account_conversation_id(&own, &peer).as_bytes() == &row.conv)
                    .then_some(DocumentConversation::Account(peer))
            }
            HistoryRecord::Channel(row) => {
                Some(DocumentConversation::Channel(hex::encode(row.conversation)))
            }
            HistoryRecord::FileCard(row) => Some(match &row.peer_account {
                Some(peer) => DocumentConversation::Account(peer.clone()),
                None => DocumentConversation::Channel(hex::encode(row.host)),
            }),
        }
    }

    fn personal_record_current(
        &self,
        record: &HistoryRecord,
        cut: Option<&super::account_documents::personal::PersonalHistoryCut>,
        source: Option<&PublicIdentity>,
    ) -> bool {
        let Some(conversation) = self.history_record_conversation(record) else {
            // Keep malformed records on the existing validation path.
            return true;
        };
        let channel_event = match record {
            HistoryRecord::Channel(row) => Some((
                ConversationId::new(row.conversation),
                EventId::new(row.event_id),
            )),
            HistoryRecord::FileCard(row) if row.peer_account.is_none() => {
                Some((ConversationId::new(row.host), EventId::new(row.event_id)))
            }
            _ => None,
        };
        if let Some((group, event)) = channel_event {
            let state = self.conversation_personal_state(&conversation);
            if state.floor.is_some()
                && self.account_group_access(group).is_ok_and(|access| {
                    matches!(
                        access.role,
                        AccountGroupRole::Member | AccountGroupRole::Owner
                    )
                })
            {
                return self.personal_channel_record_current(group, event);
            }
        }
        cut.map_or_else(
            || self.personal_history_source_allowed(&conversation, source),
            |cut| cut.allows(self, &conversation),
        )
    }

    fn private_record_current(&self, requester: &PublicIdentity, record: &HistoryRecord) -> bool {
        if !self.personal_record_current(record, None, Some(&self.identity.public())) {
            return false;
        }
        match record {
            HistoryRecord::Direct(row) => {
                DmEnvelope::decode(&row.plaintext).is_some_and(|envelope| {
                    !self.recipient_expired(
                        &envelope.body,
                        EventId::new(row.event_id),
                        row.wall_clock,
                    )
                })
            }
            HistoryRecord::Channel(row) => {
                self.account_private_channel_export_allowed(
                    ConversationId::new(row.conversation),
                    requester,
                ) && !self.recipient_expired(
                    &row.plaintext,
                    EventId::new(row.event_id),
                    row.wall_clock,
                )
            }
            HistoryRecord::FileCard(row) => {
                if row.peer_account.is_none()
                    && !self.account_private_channel_export_allowed(
                        ConversationId::new(row.host),
                        requester,
                    )
                {
                    return false;
                }
                let Ok(payload): Result<FileCardPayload, _> = bincode::DefaultOptions::new()
                    .with_fixint_encoding()
                    .with_limit(MAX_RECORD as u64)
                    .reject_trailing_bytes()
                    .deserialize(&row.payload)
                else {
                    return false;
                };
                !self.recipient_expired(
                    &payload.manifest,
                    EventId::new(row.event_id),
                    row.wall_clock,
                )
            }
        }
    }

    async fn send_account_content_frame(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        revision: u64,
        controls: (u64, [u8; 32]),
        content: PrivateContent<'_>,
        frame: &AccountHistoryFrame,
    ) -> Result<(), NodeError> {
        #[cfg(test)]
        {
            let hook = self.account_session_test_hook.lock().unwrap().clone();
            if let Some(hook) = hook {
                hook(channel.peer_identity().clone(), frame.clone()).await;
            }
        }
        let current = match content {
            PrivateContent::Record(record) => {
                self.private_record_current(channel.peer_identity(), record)
            }
            PrivateContent::Attachment(selected) => self
                .select_attachment_for_sibling(
                    selected.file,
                    channel.peer_identity(),
                    Some(selected),
                )
                .is_some(),
        };
        if !current {
            return Err(NodeError::Authorization(
                "account private content no longer available".into(),
            ));
        }
        self.send_account_history_frame(channel, version, revision, controls, frame)
            .await
    }

    /// Process-local revision of durable history projections for host polling.
    pub fn account_history_revision(&self) -> u64 {
        self.account_history_revision
            .load(std::sync::atomic::Ordering::Acquire)
    }

    pub(in crate::node) fn note_account_history_change(&self) {
        self.account_history_revision
            .fetch_add(1, std::sync::atomic::Ordering::Release);
        self.note_ui_change();
    }

    pub fn device_sync_status(&self) -> Vec<DeviceSyncStatus> {
        let policy = self.device_sync_policy();
        let current = self
            .account_sync_status
            .lock()
            .expect("account sync status mutex not poisoned");
        let mut statuses = self
            .linked_devices()
            .into_iter()
            .filter(|device| device.public.user_id() != self.user_id())
            .map(|device| {
                let id = device.public.user_id();
                let mut status = current.get(&id).cloned().unwrap_or(DeviceSyncStatus {
                    device_id: id,
                    phase: SyncPhase::Offline,
                    pending_attachments: 0,
                    legacy_file_metadata: false,
                    document_phase: SyncPhase::Offline,
                    document_error: None,
                    document_requires_upgrade: false,
                    error: None,
                });
                if !policy.history && !policy.attachments && !policy.appearance && !policy.contacts
                {
                    status.phase = SyncPhase::Disabled;
                    status.error = None;
                }
                status
            })
            .collect::<Vec<_>>();
        statuses.sort_by(|a, b| a.device_id.cmp(&b.device_id));
        statuses
    }
    pub fn account_preferences(&self, category: SyncCategory) -> Vec<AccountPreference> {
        self.account_preferences
            .lock()
            .expect("account preferences mutex not poisoned")
            .entries(category)
    }

    pub fn set_account_preference(
        &self,
        category: SyncCategory,
        key: String,
        value: Option<String>,
    ) -> Result<AccountPreference, NodeError> {
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(1, |duration| {
                duration.as_millis().min(u64::MAX as u128) as u64
            });
        let record = self
            .account_preferences
            .lock()
            .expect("account preferences mutex not poisoned")
            .set_local(category, key, value, self.user_id(), now_ms)
            .map_err(NodeError::Log)?;
        if self.device_sync_policy().enabled(category) {
            self.account_sync_notify.notify_one();
        }
        self.note_ui_change();
        self.note_account_local_change();
        Ok(record)
    }

    pub fn device_sync_policy(&self) -> DeviceSyncPolicy {
        self.device_sync_policy_snapshot().0
    }

    pub(in crate::node) fn certified_sibling(&self, public: &PublicIdentity) -> bool {
        self.linked_devices().iter().any(|device| {
            device.public == *public
                && device.certificate.verify()
                && device.certificate.account_id() == self.account_id()
                && device.certificate.device_ed25519_pub == public.ed25519_pub
        })
    }

    pub(in crate::node) fn device_sync_policy_snapshot(&self) -> (DeviceSyncPolicy, u64) {
        let store = self
            .account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned");
        (store.current(), store.revision())
    }

    pub fn set_device_sync_category(
        &self,
        category: SyncCategory,
        enabled: bool,
    ) -> Result<(), NodeError> {
        {
            let _commit = self
                .account_sync_commit
                .lock()
                .expect("account sync commit mutex not poisoned");
            self.account_sync_policy
                .lock()
                .expect("account sync policy mutex not poisoned")
                .set(category, enabled)
                .map_err(NodeError::Log)?;
        }
        self.account_sync_notify.notify_one();
        self.note_ui_change();
        self.note_account_local_change();
        if category == SyncCategory::History && enabled {
            let conversations = self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .conversations();
            for conversation in conversations {
                self.emit_new_messages(conversation);
            }
            self.delivery_notify.notify_one();
        }
        if category == SyncCategory::Profile && enabled {
            {
                let _commit = self.account_sync_commit.lock().expect("sync commit mutex");
                self.project_public_profile_documents()?;
            }
            let conversations = self
                .log
                .lock()
                .expect("log mutex not poisoned")
                .conversations();
            for conversation in conversations {
                self.process_profile_events(conversation);
            }
        }
        Ok(())
    }

    #[cfg(test)]
    fn export_sync_records(
        &self,
        requester: &PublicIdentity,
        after: Option<HistoryCursor>,
        include_history: bool,
        include_files: bool,
        version: AccountSyncVersion,
    ) -> (Vec<HistoryRecord>, Option<HistoryCursor>, [u8; 32]) {
        let (records, next, digest, _) = self
            .prepare_account_export(requester, after, include_history, include_files, version)
            .unwrap();
        (records, next, digest)
    }

    pub(in crate::node) async fn serve_account_history(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        account_id: &str,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
        wanted: DeviceSyncPolicy,
    ) -> bool {
        tokio::time::timeout(
            Duration::from_secs(20),
            self.serve_account_history_inner(
                channel,
                version,
                account_id,
                cursor,
                known_digest,
                wanted,
            ),
        )
        .await
        .unwrap_or(false)
    }

    async fn serve_account_history_inner(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        account_id: &str,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
        wanted: DeviceSyncPolicy,
    ) -> bool {
        if account_id != self.account_id() || !channel.io_admitted() {
            return false;
        }
        let peer = channel.peer_identity().clone();
        if !self.linked_devices().iter().any(|device| {
            device.public == peer
                && device.certificate.verify()
                && device.certificate.account_id() == account_id
                && device.certificate.device_ed25519_pub == peer.ed25519_pub
        }) {
            return false;
        }
        let (offered, revision) = self.device_sync_policy_snapshot();
        for device in self.linked_devices() {
            if self
                .send_account_frame(
                    channel,
                    version,
                    revision,
                    &AccountHistoryFrame::Device { device },
                )
                .await
                .is_err()
            {
                return false;
            }
        }
        let mut preferences = Vec::new();
        {
            let store = self
                .account_preferences
                .lock()
                .expect("account preferences mutex not poisoned");
            for category in [SyncCategory::Appearance, SyncCategory::Contacts] {
                if offered.enabled(category) && wanted.enabled(category) {
                    preferences.extend(store.entries(category));
                }
            }
        }
        preferences.sort_by(|a, b| {
            (matches!(a.category, SyncCategory::Contacts), a.key.as_str())
                .cmp(&(matches!(b.category, SyncCategory::Contacts), b.key.as_str()))
        });
        let (records, next_cursor, history_digest, controls) =
            if (offered.history && wanted.history) || (offered.attachments && wanted.attachments) {
                let result = self
                    .account_blocking(move |node| {
                        node.prepare_account_export(
                            &peer,
                            cursor,
                            offered.history && wanted.history,
                            version.includes_file_cards(
                                offered.history && wanted.history,
                                offered.attachments && wanted.attachments,
                            ),
                            version,
                        )
                    })
                    .await;
                let Ok(page) = result else {
                    return false;
                };
                page
            } else {
                (
                    Vec::new(),
                    None,
                    archive::empty_history_digest(),
                    self.private_dm_control_revision(),
                )
            };
        let mut digest_hasher = Sha256::new();
        digest_hasher.update(b"mesh-talk-account-sync-v2");
        digest_hasher.update(history_digest);
        for record in &preferences {
            digest_hasher.update([matches!(record.category, SyncCategory::Contacts) as u8]);
            digest_hasher.update((record.key.len() as u32).to_be_bytes());
            digest_hasher.update(record.key.as_bytes());
            digest_hasher.update(record.revision.to_be_bytes());
            digest_hasher.update(record.device_id.as_bytes());
            if let Some(value) = &record.value {
                digest_hasher.update([1]);
                digest_hasher.update((value.len() as u32).to_be_bytes());
                digest_hasher.update(value.as_bytes());
            } else {
                digest_hasher.update([0]);
            }
        }
        let digest: [u8; 32] = digest_hasher.finalize().into();
        if cursor.is_none() && known_digest == digest {
            return self
                .send_account_history_frame(
                    channel,
                    version,
                    revision,
                    controls,
                    &AccountHistoryFrame::Complete {
                        next_cursor: None,
                        digest,
                    },
                )
                .await
                .is_ok();
        }
        if cursor.is_none() {
            for record in preferences {
                if self
                    .send_account_history_frame(
                        channel,
                        version,
                        revision,
                        controls,
                        &AccountHistoryFrame::Preference { record },
                    )
                    .await
                    .is_err()
                {
                    return false;
                }
            }
        }
        let channel_hosts: HashSet<_> = records
            .iter()
            .filter_map(|record| match record {
                HistoryRecord::Channel(record) => Some(ConversationId::new(record.conversation)),
                HistoryRecord::FileCard(record) if record.peer_account.is_none() => {
                    Some(ConversationId::new(record.host))
                }
                _ => None,
            })
            .collect();
        for record in &records {
            let host = match record {
                HistoryRecord::Channel(record) => Some(ConversationId::new(record.conversation)),
                HistoryRecord::FileCard(record) if record.peer_account.is_none() => {
                    Some(ConversationId::new(record.host))
                }
                _ => None,
            };
            if host.is_some_and(|host| {
                !self.account_private_channel_export_allowed(host, channel.peer_identity())
            }) {
                return false;
            }
            let (header, plaintext) = match record {
                HistoryRecord::Direct(record) => (
                    AccountHistoryFrame::RecordHeader {
                        conversation: record.conv,
                        from: record.from.clone(),
                        wall_clock: record.wall_clock,
                        event_id: record.event_id,
                        total_len: record.plaintext.len() as u32,
                    },
                    &record.plaintext,
                ),
                HistoryRecord::Channel(record) => (
                    AccountHistoryFrame::ChannelHeader {
                        channel: record.conversation,
                        from: record.from.clone(),
                        wall_clock: record.wall_clock,
                        event_id: record.event_id,
                        total_len: record.plaintext.len() as u32,
                    },
                    &record.plaintext,
                ),
                HistoryRecord::FileCard(record) => (
                    AccountHistoryFrame::FileCardHeader {
                        host: record.host,
                        peer_account: record.peer_account.clone(),
                        from: record.from.clone(),
                        wall_clock: record.wall_clock,
                        event_id: record.event_id,
                        total_len: record.payload.len() as u32,
                    },
                    &record.payload,
                ),
            };
            if self
                .send_account_content_frame(
                    channel,
                    version,
                    revision,
                    controls,
                    PrivateContent::Record(record),
                    &header,
                )
                .await
                .is_err()
            {
                return false;
            }
            for chunk in plaintext.chunks(MAX_CHUNK) {
                if host.is_some_and(|host| {
                    !self.account_private_channel_export_allowed(host, channel.peer_identity())
                }) {
                    return false;
                }
                if self
                    .send_account_content_frame(
                        channel,
                        version,
                        revision,
                        controls,
                        PrivateContent::Record(record),
                        &AccountHistoryFrame::RecordChunk {
                            bytes: chunk.to_vec(),
                        },
                    )
                    .await
                    .is_err()
                {
                    return false;
                }
            }
        }
        if channel_hosts.iter().any(|host| {
            !self.account_private_channel_export_allowed(*host, channel.peer_identity())
        }) {
            return false;
        }
        self.send_account_history_frame(
            channel,
            version,
            revision,
            controls,
            &AccountHistoryFrame::Complete {
                next_cursor,
                digest,
            },
        )
        .await
        .is_ok()
    }

    #[cfg(test)]
    fn import_account_file_card(
        &self,
        record: &FileCardRecord,
        version: AccountSyncVersion,
    ) -> Result<(), NodeError> {
        self.import_account_file_card_from_sibling(record, version, None)
    }
    fn import_account_file_card_from_sibling(
        &self,
        record: &FileCardRecord,
        version: AccountSyncVersion,
        source: Option<&Announce>,
    ) -> Result<(), NodeError> {
        self.import_account_file_card_with_reference(record, version, source, None)
    }

    /// An explicit own-account send proves private authorship independently of
    /// later group membership. It never creates public membership or signatures.
    pub(in crate::node) fn import_explicit_account_group_file_card(
        &self,
        reference: &super::account_group::operations::journal::OperationReference,
        event: &Event,
        descriptor: &super::account_group::operations::file_preparation::GroupFileDescriptor,
        source: &Announce,
    ) -> Result<(), NodeError> {
        use super::account_group::operations::{file_preparation::GroupFileLogical, journal};
        let logical = GroupFileLogical::from_manifest(&descriptor.manifest)
            .ok_or_else(|| NodeError::Authorization("invalid original File manifest".into()))?;
        let logical_payload =
            super::account_group::operations::file_preparation::GroupFilePayload {
                logical,
                descriptor: None,
            };
        if !descriptor.valid_for(reference, &logical_payload.logical)
            || logical_payload.logical_operation().is_none_or(|body| {
                journal::operation_digest(reference.group, &body) != reference.digest
            })
            || event.conversation_id != reference.group
            || event.author.ed25519_pub() != &reference.executor.device.ed25519_pub
        {
            return Err(NodeError::Authorization(
                "invalid original File copy binding".into(),
            ));
        }
        let payload = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(&FileCardPayloadRef {
                manifest: &descriptor.manifest,
                proof: event,
            })
            .map_err(|_| NodeError::Channel("invalid original File copy encoding".into()))?;
        let record = FileCardRecord {
            host: *reference.group.as_bytes(),
            peer_account: None,
            from: reference.executor.device.user_id(),
            wall_clock: event.wall_clock,
            event_id: *event.id.as_bytes(),
            payload,
        };
        self.import_account_file_card_with_reference(
            &record,
            AccountSyncVersion::V2,
            Some(source),
            Some(reference),
        )
    }

    fn import_account_file_card_with_reference(
        &self,
        record: &FileCardRecord,
        version: AccountSyncVersion,
        source: Option<&Announce>,
        explicit: Option<&super::account_group::operations::journal::OperationReference>,
    ) -> Result<(), NodeError> {
        let host = ConversationId::new(record.host);
        let event_id = EventId::new(record.event_id);
        if let Some(other) = &record.peer_account {
            if (other == &self.account_id() && version != AccountSyncVersion::V2)
                || account_conversation_id(&self.account_id(), other) != host
            {
                return Err(NodeError::Authorization(
                    "invalid account file conversation".into(),
                ));
            }
        } else {
            let allowed = if let Some(reference) = explicit {
                reference.kind == super::AccountGroupOperationKind::File
                    && reference.group == host
                    && reference.valid(&self.account.public().ed25519_pub)
                    && source.is_some_and(|proof| {
                        proof.valid_presence()
                            && !proof.post_office
                            && proof.public() == reference.executor.device
                            && proof.account_id().as_deref() == Some(self.account_id().as_str())
                            && self.certified_sibling(&proof.public())
                    })
            } else {
                source.map_or_else(
                    || {
                        self.channels
                            .lock()
                            .expect("channels mutex not poisoned")
                            .state(&host)
                            .is_some_and(|state| {
                                !state.pending_invite() && state.is_accepted_member(&self.user_id())
                            })
                    },
                    |proof| self.account_private_channel_import_allowed(host, proof),
                )
            };
            if !allowed {
                return Err(NodeError::Authorization(
                    "file card requires accepted channel membership".into(),
                ));
            }
        }
        let payload: FileCardPayload = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_RECORD as u64)
            .reject_trailing_bytes()
            .deserialize(&record.payload)
            .map_err(|_| NodeError::Channel("invalid account file card".into()))?;
        let row = super::received_log::ReceivedEntry {
            conversation: host,
            event_id,
            from: record.from.clone(),
            wall_clock: record.wall_clock,
            plaintext: payload.manifest,
        };
        let personal = record.peer_account.as_deref() == Some(self.account_id().as_str());
        if personal
            && (!super::files::personal_file_proof_matches(&row, &payload.proof)
                || !self.linked_devices().iter().any(|device| {
                    device.public.ed25519_pub == *payload.proof.author.ed25519_pub()
                        && device.certificate.verify()
                        && device.certificate.account_id() == self.account_id()
                        && device.certificate.device_ed25519_pub == device.public.ed25519_pub
                }))
        {
            return Err(NodeError::Authorization(
                "invalid certified personal file proof".into(),
            ));
        }
        if !personal && super::files::is_personal_file_proof(&payload.proof) {
            return Err(NodeError::Authorization(
                "personal file proof has another host".into(),
            ));
        }
        if self.recipient_expired(&row.plaintext, row.event_id, row.wall_clock) {
            return Ok(());
        }
        if payload.proof.id != event_id
            || payload.proof.kind != crate::eventlog::EventKind::FileManifest
            || !payload.proof.verify_integrity()
            || !payload.proof.verify_signature()
            || payload.proof.author.user_id() != row.from
            || payload.proof.wall_clock != row.wall_clock
            || super::files::eligible_manifest_row(&row, Some(&payload.proof), None).is_none()
        {
            return Err(NodeError::Authorization("invalid signed file card".into()));
        }
        if let Some(peer) = &record.peer_account {
            let own = self.account_id();
            let author =
                if self.identity.public().ed25519_pub == *payload.proof.author.ed25519_pub()
                    || self.linked_devices().iter().any(|device| {
                        device.public.ed25519_pub == *payload.proof.author.ed25519_pub()
                    })
                {
                    Some(own)
                } else {
                    self.historical_author(payload.proof.author.ed25519_pub())
                        .and_then(|proof| proof.account_id())
                };
            if author
                .as_deref()
                .is_some_and(|author| self.private_dm_message_recalled(peer, event_id, author))
                || self
                    .recalls
                    .lock()
                    .expect("recalls mutex")
                    .targets(&host)
                    .contains(&event_id)
            {
                return Ok(());
            }
        }
        if let Some(existing) = self
            .log
            .lock()
            .expect("log mutex not poisoned")
            .get(&event_id)
            && existing != &payload.proof
        {
            return Err(NodeError::Authorization(
                "file proof conflicts with signed event".into(),
            ));
        }
        let logical = self.file_message_row_logical_id(&row);
        let suppressed = {
            let suppression = self
                .account_suppression
                .lock()
                .expect("account suppression mutex not poisoned");
            suppression.contains(host, event_id) || suppression.contains(host, logical)
        };
        let erased = {
            let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
            delivery.file_erased(host, event_id) || delivery.manifest_event_erased(event_id)
        };
        if suppressed || erased {
            return Ok(());
        }
        self.account_file_proofs
            .lock()
            .expect("file proofs mutex not poisoned")
            .insert(payload.proof)
            .map_err(NodeError::Log)?;
        let mut files = self
            .received_files
            .lock()
            .expect("files mutex not poisoned");
        let fresh = !files
            .entries_ref(&host)
            .iter()
            .any(|entry| entry.event_id == event_id);
        files
            .record_file_alias_durable(&row)
            .map_err(NodeError::Log)?;
        drop(files);
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        self.reseed_live_file_book(&delivery);
        if fresh {
            self.note_account_history_change();
        }
        Ok(())
    }

    fn personal_host_conversation(&self, host: ConversationId) -> DocumentConversation {
        let own = self.account_id();
        self.personal_account_ids()
            .into_iter()
            .find(|peer| account_conversation_id(&own, peer) == host)
            .map_or_else(
                || DocumentConversation::Channel(hex::encode(host.as_bytes())),
                DocumentConversation::Account,
            )
    }

    fn personal_attachment_allowed(
        &self,
        file: ConversationId,
        cut: &super::account_documents::personal::PersonalHistoryCut,
    ) -> bool {
        let hosts: Vec<_> = {
            let rows = self.received_files.lock().expect("files mutex");
            rows.conversations()
                .into_iter()
                .filter(|host| {
                    rows.entries_ref(host).iter().any(|row| {
                        super::files::validated_manifest(&row.plaintext)
                            .is_some_and(|manifest| manifest.file_conv() == file)
                    })
                })
                .collect()
        };
        hosts
            .into_iter()
            .any(|host| cut.allows(self, &self.personal_host_conversation(host)))
    }

    fn has_live_local_file_card(&self, file_conv: ConversationId) -> bool {
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        self.has_live_local_file_card_with(file_conv, &delivery)
    }

    fn has_live_local_file_card_with(
        &self,
        file_conv: ConversationId,
        delivery: &super::delivery_store::DeliveryStore,
    ) -> bool {
        let rows = self
            .received_files
            .lock()
            .expect("files mutex not poisoned");
        let suppression = self
            .account_suppression
            .lock()
            .expect("suppression mutex not poisoned");
        for host in rows.conversations() {
            for row in rows.entries_ref(&host) {
                if delivery.file_erased(host, row.event_id)
                    || delivery.manifest_event_erased(row.event_id)
                    || suppression.contains(host, row.event_id)
                {
                    continue;
                }
                if super::files::validated_manifest(&row.plaintext)
                    .is_some_and(|manifest| manifest.file_conv() == file_conv)
                {
                    return true;
                }
            }
        }
        false
    }

    fn attachment_for_sibling(
        &self,
        file_conv: ConversationId,
        requester: &PublicIdentity,
    ) -> Option<crate::file::AnyManifest> {
        self.select_attachment_for_sibling(file_conv, requester, None)
            .map(|selected| selected.manifest)
    }

    fn select_attachment_for_sibling(
        &self,
        file_conv: ConversationId,
        requester: &PublicIdentity,
        exact: Option<&SelectedAttachment>,
    ) -> Option<SelectedAttachment> {
        let (recalled, _) = self.account_archive_recalls();
        let own = self.account_id();
        if !self.account_private_sibling_current(requester) {
            return None;
        }
        let mut hosts = HashSet::new();
        hosts.insert(account_conversation_id(&own, &own));
        for other in self.known_account_ids() {
            hosts.insert(account_conversation_id(&own, &other));
        }
        let channel_scopes: HashMap<_, _> = if let Some(selected) = exact {
            selected
                .scope
                .as_ref()
                .and_then(|_| {
                    self.account_private_channel_export_scope(selected.row.conversation, requester)
                })
                .map(|scope| (selected.row.conversation, scope))
                .into_iter()
                .collect()
        } else {
            self.account_private_archive_channels(requester)
                .into_iter()
                .filter_map(|host| {
                    self.account_private_channel_export_scope(host, requester)
                        .map(|scope| (host, scope))
                })
                .collect()
        };
        hosts.extend(channel_scopes.keys().copied());
        hosts.retain(|host| {
            self.personal_source_applied(
                &self.personal_host_conversation(*host),
                &self.identity.public(),
            )
        });
        if let Some(selected) = exact
            && (!hosts.contains(&selected.row.conversation)
                || channel_scopes.get(&selected.row.conversation) != selected.scope.as_ref())
        {
            return None;
        }
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        let rows = self
            .received_files
            .lock()
            .expect("files mutex not poisoned");
        let log = self.log.lock().expect("log mutex not poisoned");
        let proofs = self
            .account_file_proofs
            .lock()
            .expect("file proofs mutex not poisoned");
        let suppression = self
            .account_suppression
            .lock()
            .expect("suppression mutex not poisoned");
        for host in hosts {
            for row in rows.entries_ref(&host) {
                if exact.is_some_and(|selected| row != &selected.row) {
                    continue;
                }
                if delivery.file_erased(host, row.event_id)
                    || delivery.manifest_event_erased(row.event_id)
                    || suppression.contains(host, row.event_id)
                    || self.recipient_expired(&row.plaintext, row.event_id, row.wall_clock)
                    || recalled
                        .get(&host)
                        .is_some_and(|ids| ids.contains(&row.event_id))
                {
                    continue;
                }
                let proof = log.get(&row.event_id).or_else(|| proofs.get(&row.event_id));
                if let Some(manifest) = super::files::eligible_manifest_row(row, proof, None)
                    && manifest.file_conv() == file_conv
                {
                    if exact.is_some_and(|selected| selected.manifest != manifest) {
                        continue;
                    }
                    return Some(SelectedAttachment {
                        file: file_conv,
                        row: row.clone(),
                        manifest,
                        scope: channel_scopes.get(&host).cloned(),
                    });
                }
            }
        }
        None
    }

    fn account_recalled_files(&self) -> HashSet<ConversationId> {
        let (recalled, _) = self.account_archive_recalls();
        let rows = self.received_files.lock().expect("files mutex");
        let mut files = HashSet::new();
        for (host, targets) in recalled {
            for row in rows.entries_ref(&host) {
                if targets.contains(&row.event_id)
                    && let Some(manifest) = super::files::validated_manifest(&row.plaintext)
                {
                    files.insert(manifest.file_conv());
                }
            }
        }
        files
    }

    pub(in crate::node) async fn serve_account_attachment_checked(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        account_id: &str,
        file_conv: ConversationId,
    ) -> bool {
        let (policy, revision) = self.device_sync_policy_snapshot();
        let manifest = {
            let _operation = self.privacy.gate.read().await;
            if account_id != self.account_id()
                || !channel.io_admitted()
                || !self.certified_sibling(channel.peer_identity())
                || !self.account_allowed(channel.peer_identity(), channel.peer_announcement())
            {
                return false;
            }
            self.attachment_for_sibling(file_conv, channel.peer_identity())
        };
        // An admitted sibling gets one uniform negative for absent, erased,
        // unshared, or unavailable copies. It learns no existence distinction.
        let available = manifest.as_ref().is_some_and(|manifest| {
            self.file_progress(file_conv)
                .is_some_and(|progress| progress.done == progress.total)
                || self.managed_media_for_export(file_conv, manifest).is_some()
        });
        if !policy.attachments || !available {
            return self
                .send_account_frame(
                    channel,
                    AccountSyncVersion::V2,
                    revision,
                    &AccountHistoryFrame::AttachmentUnavailable {
                        file_conv: *file_conv.as_bytes(),
                        source_disabled: !policy.attachments,
                    },
                )
                .await
                .is_ok();
        }
        // Availability is a bounded metadata check, never an integrity verdict.
        // Staging/decryption/checksum failures retain the existing error path.
        self.serve_account_attachment(channel, AccountSyncVersion::V2, account_id, file_conv)
            .await
    }

    pub(in crate::node) async fn serve_account_attachment(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        account_id: &str,
        file_conv: ConversationId,
    ) -> bool {
        let size = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .map_or(0, |manifest| manifest.size());
        let deadline = attachment_deadline(size);
        tokio::time::timeout(
            deadline,
            self.serve_account_attachment_inner(channel, version, account_id, file_conv),
        )
        .await
        .unwrap_or(false)
    }

    async fn serve_account_attachment_inner(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        version: AccountSyncVersion,
        account_id: &str,
        file_conv: ConversationId,
    ) -> bool {
        let (offered, revision) = self.device_sync_policy_snapshot();
        if account_id != self.account_id() || !channel.io_admitted() || !offered.attachments {
            return false;
        }
        let peer = channel.peer_identity().clone();
        if !self.linked_devices().iter().any(|device| {
            device.public == peer
                && device.certificate.verify()
                && device.certificate.account_id() == account_id
                && device.certificate.device_ed25519_pub == peer.ed25519_pub
        }) {
            return false;
        }
        let controls = self.private_dm_control_revision();
        let staging = self
            .account_blocking(move |node| {
                let selected = node
                    .select_attachment_for_sibling(file_conv, &peer, None)
                    .ok_or_else(|| {
                        NodeError::Authorization("attachment no longer shared".into())
                    })?;
                let staging =
                    tempfile::tempdir().map_err(|error| NodeError::File(error.to_string()))?;
                node.save_directory_archive(
                    file_conv,
                    &staging.path().join("verified-attachment"),
                )?;
                Ok((staging, selected))
            })
            .await;
        let Ok((staging, selected)) = staging else {
            return false;
        };
        let manifest = &selected.manifest;
        let path = staging.path().join("verified-attachment");
        let Ok(mut source) = tokio::fs::File::open(&path).await else {
            return false;
        };
        if self
            .select_attachment_for_sibling(file_conv, channel.peer_identity(), Some(&selected))
            .is_none()
        {
            return false;
        }
        if self
            .send_account_content_frame(
                channel,
                version,
                revision,
                controls,
                PrivateContent::Attachment(&selected),
                &AccountHistoryFrame::AttachmentStart {
                    file_conv: *file_conv.as_bytes(),
                    name: manifest.name().to_string(),
                    size: manifest.size(),
                    sha256: manifest.checksum(),
                },
            )
            .await
            .is_err()
        {
            return false;
        }
        let mut hasher = Sha256::new();
        let mut total = 0u64;
        let mut buffer = vec![0u8; MAX_CHUNK];
        loop {
            let Ok(read) = source.read(&mut buffer).await else {
                return false;
            };
            if read == 0 {
                break;
            }
            if self
                .select_attachment_for_sibling(file_conv, channel.peer_identity(), Some(&selected))
                .is_none()
            {
                return false;
            }
            total += read as u64;
            hasher.update(&buffer[..read]);
            if self
                .send_account_content_frame(
                    channel,
                    version,
                    revision,
                    controls,
                    PrivateContent::Attachment(&selected),
                    &AccountHistoryFrame::AttachmentChunk {
                        bytes: buffer[..read].to_vec(),
                    },
                )
                .await
                .is_err()
            {
                return false;
            }
        }
        let actual: [u8; 32] = hasher.finalize().into();
        if total == manifest.size()
            && actual == manifest.checksum()
            && self
                .select_attachment_for_sibling(file_conv, channel.peer_identity(), Some(&selected))
                .is_some()
        {
            self.send_account_content_frame(
                channel,
                version,
                revision,
                controls,
                PrivateContent::Attachment(&selected),
                &AccountHistoryFrame::AttachmentComplete,
            )
            .await
            .is_ok()
        } else {
            false
        }
    }

    #[cfg(test)]
    async fn pull_account_attachment(
        self: &Arc<Self>,
        peer: &PeerRecord,
        file_conv: ConversationId,
        version: AccountSyncVersion,
    ) -> Result<(), NodeError> {
        Box::pin(self.pull_account_attachment_inner(peer, file_conv, version, false))
            .await
            .map(|_| ())
    }

    async fn pull_account_attachment_optional(
        self: &Arc<Self>,
        peer: &PeerRecord,
        file_conv: ConversationId,
        version: AccountSyncVersion,
        checked: bool,
        unsupported: &mut bool,
    ) -> Result<AttachmentOutcome, NodeError> {
        if !checked || version == AccountSyncVersion::V1 {
            return Box::pin(self.pull_account_attachment_inner(peer, file_conv, version, false))
                .await;
        }
        match Box::pin(self.pull_account_attachment_inner(peer, file_conv, version, true)).await {
            Err(error) if optional_attachment_closed(&error) => {
                // A close proves no availability. Retry the frozen request and
                // remember only that this optional capability was unavailable.
                *unsupported = true;
                Box::pin(self.pull_account_attachment_inner(peer, file_conv, version, false)).await
            }
            result => result,
        }
    }

    async fn pull_account_attachment_inner(
        self: &Arc<Self>,
        peer: &PeerRecord,
        file_conv: ConversationId,
        version: AccountSyncVersion,
        checked: bool,
    ) -> Result<AttachmentOutcome, NodeError> {
        let (wanted, revision) = self.device_sync_policy_snapshot();
        let controls = self.private_dm_control_revision();
        let personal_cut = self.capture_personal_history_cut(&peer.public);
        if !wanted.attachments {
            return Ok(AttachmentOutcome::Complete);
        }
        let own = self.account_id();
        if peer.account_id.as_deref() != Some(own.as_str())
            || !self.linked_devices().iter().any(|device| {
                device.public == peer.public
                    && device.certificate.verify()
                    && device.certificate.account_id() == own
                    && device.certificate.device_ed25519_pub == peer.public.ed25519_pub
            })
        {
            return Err(NodeError::Authorization(
                "peer is not a certified linked device".into(),
            ));
        }
        let manifest = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .cloned()
            .ok_or_else(|| NodeError::File("unknown attachment".into()))?;
        let mut channel = self
            .begin_account_exchange(
                peer,
                version,
                if checked {
                    AccountHistoryFrame::AttachmentRequestChecked {
                        account_id: own,
                        file_conv: *file_conv.as_bytes(),
                    }
                } else {
                    AccountHistoryFrame::AttachmentRequest {
                        version: version.number(),
                        account_id: own,
                        file_conv: *file_conv.as_bytes(),
                    }
                },
            )
            .await?;
        let mut staging: Option<tempfile::TempDir> = None;
        let mut output: Option<tokio::fs::File> = None;
        let mut hasher = Sha256::new();
        let mut received = 0u64;
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::File("attachment read timed out".into()))?
                .map_err(transport)?;
            let (response_version, frame) = AccountHistoryFrame::decode_versioned(&bytes)
                .ok_or_else(|| NodeError::File("invalid attachment frame".into()))?;
            if response_version != version {
                return Err(NodeError::File("attachment codec changed".into()));
            }
            match frame {
                AccountHistoryFrame::AttachmentUnavailable {
                    file_conv: claimed,
                    source_disabled,
                } if checked && staging.is_none() && claimed == *file_conv.as_bytes() => {
                    if self.device_sync_policy_snapshot().1 != revision
                        || !channel.io_admitted()
                        || !self.certified_sibling(&peer.public)
                    {
                        return Err(NodeError::Authorization("attachment policy changed".into()));
                    }
                    channel.recycle(self);
                    return Ok(if source_disabled {
                        AttachmentOutcome::SourceDisabled
                    } else {
                        AttachmentOutcome::Unavailable
                    });
                }
                AccountHistoryFrame::AttachmentStart {
                    file_conv: claimed,
                    name,
                    size,
                    sha256,
                } if staging.is_none()
                    && claimed == *file_conv.as_bytes()
                    && name == manifest.name()
                    && size == manifest.size()
                    && sha256 == manifest.checksum() =>
                {
                    let dir = tempfile::tempdir()
                        .map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
                    let file = tokio::fs::File::create(dir.path().join("incoming"))
                        .await
                        .map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
                    output = Some(file);
                    staging = Some(dir);
                }
                AccountHistoryFrame::AttachmentChunk { bytes } if output.is_some() => {
                    if bytes.is_empty() || bytes.len() > MAX_CHUNK {
                        return Err(NodeError::File("invalid attachment chunk".into()));
                    }
                    received = received.saturating_add(bytes.len() as u64);
                    if received > manifest.size() {
                        return Err(NodeError::File("attachment exceeds manifest size".into()));
                    }
                    hasher.update(&bytes);
                    use tokio::io::AsyncWriteExt;
                    output
                        .as_mut()
                        .unwrap()
                        .write_all(&bytes)
                        .await
                        .map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
                }
                AccountHistoryFrame::AttachmentComplete if output.is_some() => {
                    let actual: [u8; 32] = hasher.finalize().into();
                    if received != manifest.size() || actual != manifest.checksum() {
                        return Err(NodeError::File("attachment checksum mismatch".into()));
                    }
                    output
                        .take()
                        .unwrap()
                        .sync_all()
                        .await
                        .map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
                    let dir = staging.take().unwrap();
                    self.account_blocking(move |node| {
                        node.commit_account_attachment(
                            file_conv,
                            revision,
                            controls,
                            &manifest,
                            dir,
                            &personal_cut,
                        )
                    })
                    .await?;
                    channel.recycle(self);
                    return Ok(AttachmentOutcome::Complete);
                }
                _ => return Err(NodeError::File("unexpected attachment frame".into())),
            }
        }
    }

    fn commit_account_attachment(
        &self,
        file_conv: ConversationId,
        revision: u64,
        controls: (u64, [u8; 32]),
        manifest: &crate::file::AnyManifest,
        dir: tempfile::TempDir,
        personal_cut: &super::account_documents::personal::PersonalHistoryCut,
    ) -> Result<(), NodeError> {
        // Bulk encryption and verification must not retain the delivery or
        // policy commit guards. Preparation remains inside its admitted blocking
        // worker; cancellation/retirement cannot detach it from that owner.
        let current = || {
            !self.runtime_work.is_closed()
                && !self
                    .delivery_suspended
                    .load(std::sync::atomic::Ordering::Acquire)
                && self.device_sync_policy_snapshot().1 == revision
        };
        let prepared = self
            .media
            .prepare_from_path_if(
                file_conv,
                manifest.name(),
                &dir.path().join("incoming"),
                || {
                    if current() {
                        Ok(())
                    } else {
                        Err(std::io::Error::new(
                            std::io::ErrorKind::Interrupted,
                            "attachment preparation no longer admitted",
                        ))
                    }
                },
            )
            .map_err(|error| NodeError::File(format!("attachment preparation: {error}")))?;
        let _commit = self
            .account_sync_commit
            .lock()
            .expect("account sync commit mutex not poisoned");
        if !current()
            || !self.personal_attachment_allowed(file_conv, personal_cut)
            || self.private_dm_control_revision() != controls
            || self.account_recalled_files().contains(&file_conv)
        {
            return Err(NodeError::Authorization(
                "attachment recall state changed".into(),
            ));
        }
        let delivery = self.delivery.lock().expect("delivery mutex not poisoned");
        let policy = self
            .account_sync_policy
            .lock()
            .expect("account sync policy mutex not poisoned");
        if policy.revision() != revision
            || !policy.current().attachments
            || self
                .files
                .lock()
                .expect("files mutex not poisoned")
                .manifest(&file_conv)
                .is_none()
            || !self.has_live_local_file_card_with(file_conv, &delivery)
        {
            return Err(NodeError::File("attachment was removed during sync".into()));
        }
        // Publish only after rechecking the exact policy and live file-card
        // lifetime. Deletion or recall during preparation cannot revive bytes.
        self.media
            .publish_prepared(prepared)
            .map_err(|e| NodeError::File(format!("attachment save: {e}")))?;
        self.note_account_history_change();
        Ok(())
    }

    fn preserve_available_attachment(
        &self,
        file_conv: ConversationId,
        revision: u64,
    ) -> Result<bool, NodeError> {
        if self.media.contains(file_conv) {
            return Ok(true);
        }
        if !self.has_live_local_file_card(file_conv)
            || self
                .file_progress(file_conv)
                .is_none_or(|progress| progress.done != progress.total)
        {
            return Ok(false);
        }
        let manifest = self
            .files
            .lock()
            .expect("files mutex not poisoned")
            .manifest(&file_conv)
            .cloned()
            .ok_or_else(|| NodeError::File("unknown attachment".into()))?;
        let controls = self.private_dm_control_revision();
        let personal_cut = self.capture_personal_history_cut(&self.identity.public());
        let staging =
            tempfile::tempdir().map_err(|e| NodeError::File(format!("attachment staging: {e}")))?;
        let path = staging.path().join("verified-attachment");
        self.save_directory_archive(file_conv, &path)?;
        // The same final current-policy/card guard covers local retention and remote pulls.
        // commit_account_attachment expects the verified file at this staging path.
        std::fs::rename(&path, staging.path().join("incoming"))
            .map_err(|error| NodeError::File(error.to_string()))?;
        self.commit_account_attachment(
            file_conv,
            revision,
            controls,
            &manifest,
            staging,
            &personal_cut,
        )?;
        Ok(true)
    }

    #[cfg(test)]
    fn apply_account_history_page(
        &self,
        page: &[HistoryRecord],
        version: AccountSyncVersion,
    ) -> Result<(), NodeError> {
        self.apply_account_history_page_at_revision(page, version, None)
    }

    #[cfg(test)]
    fn apply_account_history_page_at_revision(
        &self,
        page: &[HistoryRecord],
        version: AccountSyncVersion,
        expected_revision: Option<u64>,
    ) -> Result<(), NodeError> {
        self.apply_account_history_page_from_sibling(page, version, expected_revision, None, None)
    }
    fn apply_account_history_page_from_sibling(
        &self,
        page: &[HistoryRecord],
        version: AccountSyncVersion,
        expected_revision: Option<u64>,
        source: Option<&Announce>,
        personal_cut: Option<&super::account_documents::personal::PersonalHistoryCut>,
    ) -> Result<(), NodeError> {
        let _commit = self
            .account_sync_commit
            .lock()
            .expect("account sync commit mutex not poisoned");
        let (policy, revision) = self.device_sync_policy_snapshot();
        if expected_revision.is_some_and(|expected| expected != revision) {
            return Err(NodeError::Authorization(
                "account sync policy changed".into(),
            ));
        }
        if source.is_some_and(|proof| !self.certified_sibling(&proof.public())) {
            return Err(NodeError::Authorization(
                "account history source changed".into(),
            ));
        }
        // Eligibility belongs to the original request. A late ACK cannot make
        // an already-buffered pre-cleanup page eligible at its commit boundary.
        let admitted: Vec<_> = page
            .iter()
            .filter(|record| {
                self.personal_record_current(
                    record,
                    personal_cut,
                    source.map(Announce::public).as_ref(),
                )
            })
            .collect();
        if policy.history {
            for record in &admitted {
                if let HistoryRecord::Channel(record) = record {
                    let channel = ConversationId::new(record.conversation);
                    let allowed = source.map_or_else(
                        || {
                            self.channels
                                .lock()
                                .expect("channels mutex not poisoned")
                                .state(&channel)
                                .is_some_and(|state| {
                                    !state.pending_invite()
                                        && state.is_accepted_member(&self.user_id())
                                })
                        },
                        |proof| self.account_private_channel_import_allowed(channel, proof),
                    );
                    if !allowed {
                        return Err(NodeError::Authorization(
                            "channel history requires an entitled account source".into(),
                        ));
                    }
                }
            }
            {
                let log = self.log.lock().expect("log mutex not poisoned");
                for record in &admitted {
                    if let HistoryRecord::Channel(record) = record {
                        let channel = ConversationId::new(record.conversation);
                        if let Some(event) = log.get(&EventId::new(record.event_id))
                            && (event.conversation_id != channel
                                || event.kind != crate::eventlog::EventKind::Message
                                || event.author.user_id() != record.from)
                        {
                            return Err(NodeError::Authorization(
                                "channel history conflicts with signed event".into(),
                            ));
                        }
                    }
                }
            }
            let direct: Vec<_> = admitted
                .iter()
                .filter_map(|record| match record {
                    HistoryRecord::Direct(record) => Some(record.clone()),
                    HistoryRecord::Channel(_) | HistoryRecord::FileCard(_) => None,
                })
                .collect();
            self.import_account_backfill(&direct)?;
            for record in &admitted {
                if let HistoryRecord::Channel(record) = record {
                    self.record_private_channel_plaintext(&super::received_log::ReceivedEntry {
                        conversation: ConversationId::new(record.conversation),
                        event_id: EventId::new(record.event_id),
                        from: record.from.clone(),
                        wall_clock: record.wall_clock,
                        plaintext: record.plaintext.clone(),
                    })?;
                }
            }
        }
        if version.includes_file_cards(policy.history, policy.attachments) {
            for record in &admitted {
                if let HistoryRecord::FileCard(record) = record {
                    self.import_account_file_card_from_sibling(record, version, source)?;
                }
            }
        }
        Ok(())
    }

    /// Explicit own-send completion is independent of optional history sharing.
    /// Its caller verifies the original signed intent and exact public event.
    pub(in crate::node) fn import_own_account_channel_copy(
        &self,
        row: &super::received_log::ReceivedEntry,
    ) -> Result<(), NodeError> {
        let _commit = self
            .account_sync_commit
            .lock()
            .expect("account sync commit mutex");
        let conversation = DocumentConversation::Channel(hex::encode(row.conversation.as_bytes()));
        let state = self.conversation_personal_state(&conversation);
        if state.floor.is_some()
            && (state.hidden
                || !self.personal_channel_record_current(row.conversation, row.event_id))
        {
            return Ok(());
        }
        self.record_private_channel_plaintext(row)
    }

    fn record_private_channel_plaintext(
        &self,
        row: &super::received_log::ReceivedEntry,
    ) -> Result<(), NodeError> {
        if self.recipient_expired(&row.plaintext, row.event_id, row.wall_clock)
            || self
                .account_suppression
                .lock()
                .expect("account suppression mutex")
                .contains(row.conversation, row.event_id)
        {
            return Ok(());
        }
        let mut received = self.received.lock().expect("received mutex");
        if let Some(existing) = received.entry(row.event_id) {
            // Preserve the live receiver's existing timestamp for the same body.
            if existing.conversation == row.conversation
                && existing.from == row.from
                && existing.plaintext == row.plaintext
            {
                let retained = existing.clone();
                return received.record_durable(&retained).map_err(NodeError::Log);
            }
            return Err(NodeError::Authorization(
                "channel history conflicts with local plaintext".into(),
            ));
        }
        received.record_durable(row).map_err(NodeError::Log)?;
        drop(received);
        self.note_account_history_change();
        Ok(())
    }

    pub(in crate::node) async fn serve_account_file_message_identities(
        &self,
        channel: &mut SecureChannel<TcpStream>,
        account_id: &str,
        requests: Vec<([u8; 32], [u8; 32])>,
    ) -> bool {
        use crate::eventlog::sync::SyncStore;
        let (revision, bundles) = {
            let _operation = self.privacy.gate.read().await;
            if account_id != self.account_id()
                || requests.len() > super::session::MAX_FILE_IDENTITIES
                || !channel.io_admitted()
                || !self.certified_sibling(channel.peer_identity())
                || !self.account_allowed(channel.peer_identity(), channel.peer_announcement())
            {
                return false;
            }
            let (policy, revision) = self.device_sync_policy_snapshot();
            let bundles = if policy.history || policy.attachments {
                let store = self.serve_sync_store(channel.peer_identity());
                let mut seen = HashSet::new();
                requests
                    .into_iter()
                    .filter_map(|(file, manifest)| {
                        store
                            .lock()
                            .expect("sync store lock")
                            .file_message_identity(
                                &ConversationId::new(file),
                                &EventId::new(manifest),
                            )
                    })
                    .filter(|bundle| seen.insert(bundle.id))
                    .collect()
            } else {
                vec![]
            };
            (revision, bundles)
        };
        self.send_account_frame(
            channel,
            AccountSyncVersion::V2,
            revision,
            &AccountHistoryFrame::FileMessageIdentitiesResponse { bundles },
        )
        .await
        .is_ok()
    }

    async fn pull_account_file_message_identities(
        self: &Arc<Self>,
        peer: &PeerRecord,
        requests: &[(ConversationId, EventId)],
        revision: u64,
    ) {
        if !self.file_identity_capability_due(peer.addr, &peer.public) {
            return;
        }
        let mut requests: Vec<_> = requests
            .iter()
            .copied()
            .filter(|(_, manifest)| {
                let host = self
                    .received_files
                    .lock()
                    .expect("received files lock")
                    .entry(*manifest)
                    .map(|row| row.conversation);
                host.is_some_and(|host| {
                    host != account_conversation_id(&self.account_id(), &self.account_id())
                        && self
                            .channels
                            .lock()
                            .expect("channels lock")
                            .state(&host)
                            .is_none()
                        && !self.has_file_message_identity(host, *manifest)
                })
            })
            .collect();
        requests.extend(self.pending_file_message_identities(&peer.public));
        requests.sort_unstable_by_key(|(file, manifest)| (*file.as_bytes(), *manifest.as_bytes()));
        requests.dedup();
        if !requests.is_empty() {
            let offset = self.file_message_identity_cursor.fetch_add(
                super::session::MAX_FILE_IDENTITIES,
                std::sync::atomic::Ordering::Relaxed,
            ) % requests.len();
            requests.rotate_left(offset);
            requests.truncate(super::session::MAX_FILE_IDENTITIES);
        }
        for batch in requests.chunks(super::session::MAX_FILE_IDENTITIES) {
            let policy = self.device_sync_policy_snapshot();
            if policy.1 != revision
                || (!policy.0.history && !policy.0.attachments)
                || !self.certified_sibling(&peer.public)
            {
                return;
            }
            let opening = self
                .begin_account_exchange(
                    peer,
                    AccountSyncVersion::V2,
                    AccountHistoryFrame::FileMessageIdentitiesRequest {
                        account_id: self.account_id(),
                        requests: batch
                            .iter()
                            .map(|(file, manifest)| (*file.as_bytes(), *manifest.as_bytes()))
                            .collect(),
                    },
                )
                .await;
            let mut channel = match opening {
                Ok(channel) => channel,
                Err(_) => {
                    self.sessions.note_file_identity_unsupported(
                        self.file_identity_endpoint_key(peer.addr, &peer.public),
                    );
                    return;
                }
            };
            let key = match channel.tcp_peer_addr() {
                Ok(address) => self.peer_endpoint_key(address, &peer.public),
                Err(_) => return,
            };
            let response = tokio::time::timeout(READ_TIMEOUT, channel.recv()).await;
            let bundles = match response {
                Ok(Ok(bytes)) => match AccountHistoryFrame::decode_versioned(&bytes) {
                    Some((
                        AccountSyncVersion::V2,
                        AccountHistoryFrame::FileMessageIdentitiesResponse { bundles },
                    )) => bundles,
                    _ => {
                        self.sessions.note_file_identity_unsupported(key);
                        return;
                    }
                },
                _ => {
                    self.sessions.note_file_identity_unsupported(key);
                    return;
                }
            };
            if self
                .accept_file_message_identity_response(&peer.public, batch, bundles, Some(revision))
                .is_err()
            {
                return;
            }
            self.sessions.note_file_identity_supported(&key);
            channel.recycle(self);
        }
    }

    async fn pull_account_history_page(
        self: &Arc<Self>,
        peer: &PeerRecord,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
    ) -> Result<(Option<HistoryCursor>, [u8; 32], AccountSyncVersion), NodeError> {
        let mut rejected_before_reply = false;
        let result = self
            .pull_account_history_version(
                peer,
                cursor,
                known_digest,
                AccountSyncVersion::V2,
                &mut rejected_before_reply,
            )
            .await;
        match result {
            Ok((next, digest)) => Ok((next, digest, AccountSyncVersion::V2)),
            Err(_) if rejected_before_reply => {
                let mut ignored = false;
                self.pull_account_history_version(
                    peer,
                    cursor,
                    known_digest,
                    AccountSyncVersion::V1,
                    &mut ignored,
                )
                .await
                .map(|(next, digest)| (next, digest, AccountSyncVersion::V1))
            }
            Err(error) => Err(error),
        }
    }

    async fn pull_account_history_version(
        self: &Arc<Self>,
        peer: &PeerRecord,
        cursor: Option<HistoryCursor>,
        known_digest: [u8; 32],
        version: AccountSyncVersion,
        rejected_before_reply: &mut bool,
    ) -> Result<(Option<HistoryCursor>, [u8; 32]), NodeError> {
        let (wanted, revision) = self.device_sync_policy_snapshot();
        if !wanted.history && !wanted.attachments && !wanted.appearance && !wanted.contacts {
            return Ok((None, [0; 32]));
        }
        let own = self.account_id();
        if peer.account_id.as_deref() != Some(own.as_str())
            || !self.linked_devices().iter().any(|device| {
                device.public == peer.public
                    && device.certificate.verify()
                    && device.certificate.account_id() == own
                    && device.certificate.device_ed25519_pub == peer.public.ed25519_pub
            })
        {
            return Err(NodeError::Authorization(
                "peer is not a certified linked device".into(),
            ));
        }
        let personal_cut = self.capture_personal_history_cut(&peer.public);
        let mut channel = self
            .begin_account_exchange(
                peer,
                version,
                AccountHistoryFrame::Request {
                    version: version.number(),
                    account_id: own.clone(),
                    cursor: if wanted.history || wanted.attachments {
                        cursor
                    } else {
                        None
                    },
                    known_digest,
                    wanted,
                },
            )
            .await?;
        let source_proof = channel
            .peer_announcement()
            .cloned()
            .or_else(|| self.account_private_source_announcement(channel.peer_identity()))
            .ok_or_else(|| NodeError::Authorization("account source proof unavailable".into()))?;
        let mut page = HistoryPageAssembler::new();
        let mut seen_devices = HashSet::new();
        let mut seen_preferences = HashSet::new();
        let mut started_records = false;
        let mut valid_reply = channel.reusable();
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::Channel("account-history read timed out".into()))?
                .map_err(|error| {
                    *rejected_before_reply = !valid_reply && matches!(&error,
                        crate::transport::TransportError::Io(io) if matches!(io.kind(),
                            std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::BrokenPipe));
                    transport(error)
                })?;
            let (response_version, frame) = AccountHistoryFrame::decode_versioned(&bytes)
                .ok_or_else(|| NodeError::Channel("invalid account-history response".into()))?;
            if response_version != version {
                return Err(NodeError::Channel("account-history codec changed".into()));
            }
            valid_reply = true;
            match frame {
                AccountHistoryFrame::Device { device }
                    if !started_records && page.is_idle() && seen_devices.len() < 64 =>
                {
                    if !seen_devices.insert(device.public.user_id()) {
                        return Err(NodeError::Channel("duplicate linked device".into()));
                    }
                    self.account_devices
                        .lock()
                        .expect("account devices mutex not poisoned")
                        .insert(device)
                        .map_err(NodeError::Log)?;
                }
                AccountHistoryFrame::Preference { record }
                    if page.is_idle()
                        && page.is_empty()
                        && seen_preferences.len() < super::account_preferences::MAX_KEYS =>
                {
                    started_records = true;
                    let key = (
                        matches!(record.category, SyncCategory::Contacts),
                        record.key.clone(),
                    );
                    if !seen_preferences.insert(key) {
                        return Err(NodeError::Channel("duplicate account preference".into()));
                    }
                    if !self
                        .linked_devices()
                        .iter()
                        .any(|device| device.public.user_id() == record.device_id)
                    {
                        return Err(NodeError::Authorization("unknown preference author".into()));
                    }
                    self.account_blocking(move |node| {
                        let _commit = node
                            .account_sync_commit
                            .lock()
                            .expect("account sync commit mutex not poisoned");
                        let policy = node
                            .account_sync_policy
                            .lock()
                            .expect("account sync policy mutex not poisoned");
                        if policy.revision() == revision
                            && policy.current().enabled(record.category)
                        {
                            let changed = node
                                .account_preferences
                                .lock()
                                .expect("account preferences mutex not poisoned")
                                .merge(record)
                                .map_err(NodeError::Log)?;
                            if changed {
                                node.note_ui_change();
                            }
                        }
                        Ok(())
                    })
                    .await?;
                }
                other => {
                    if matches!(
                        other,
                        AccountHistoryFrame::RecordHeader { .. }
                            | AccountHistoryFrame::ChannelHeader { .. }
                            | AccountHistoryFrame::FileCardHeader { .. }
                    ) {
                        started_records = true;
                    }
                    if let Some(complete) = page.accept(other, &own)? {
                        // A disabled category may have skipped frames above. Never
                        // acknowledge their digest or retain a continuation cursor,
                        // even if off/on changes restored the original bools (ABA).
                        if self.device_sync_policy_snapshot().1 != revision {
                            return Ok((None, [0; 32]));
                        }
                        let next = complete.next_cursor;
                        let digest = complete.digest;
                        let identities: Vec<_> = complete
                            .records
                            .iter()
                            .filter_map(|record| {
                                let HistoryRecord::FileCard(record) = record else {
                                    return None;
                                };
                                let payload: FileCardPayload = bincode::DefaultOptions::new()
                                    .with_fixint_encoding()
                                    .with_limit(MAX_RECORD as u64)
                                    .reject_trailing_bytes()
                                    .deserialize(&record.payload)
                                    .ok()?;
                                let manifest = super::files::validated_manifest(&payload.manifest)?;
                                Some((manifest.file_conv(), EventId::new(record.event_id)))
                            })
                            .collect();
                        self.account_blocking(move |node| {
                            // Recheck after waiting for the blocking pool.
                            if node.device_sync_policy_snapshot().1 != revision {
                                return Err(NodeError::Authorization(
                                    "account sync policy changed".into(),
                                ));
                            }
                            node.apply_account_history_page_from_sibling(
                                &complete.records,
                                version,
                                Some(revision),
                                Some(&source_proof),
                                Some(&personal_cut),
                            )
                        })
                        .await?;
                        channel.recycle(self);
                        self.pull_account_file_message_identities(peer, &identities, revision)
                            .await;
                        return Ok((next, digest));
                    }
                }
            }
        }
    }

    pub(in crate::node) async fn run_account_history_loop(self: Arc<Self>) {
        let mut tick = tokio::time::interval(RETRY_INTERVAL);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        self.run_account_history_loop_with_tick(Some(tick)).await;
    }

    // Tests can exclude periodic repair to prove that retained work itself
    // schedules progress, without coupling fixture setup to the first tick.
    async fn run_account_history_loop_with_tick(
        self: Arc<Self>,
        mut tick: Option<tokio::time::Interval>,
    ) {
        use std::collections::VecDeque;
        // Partition the existing four slots: one each for control, directory,
        // optional documents, and history/favorites. Each lane retains its page
        // independently through the same globally admitted account session pool.
        const HISTORY_WORKERS: usize = 1;
        const DOCUMENT_WORKERS: usize = DocumentLane::ALL.len();
        const ATTACHMENT_WORKERS: usize = 2;
        let mut cursors: HashMap<String, PeerCursor> = HashMap::new();
        let mut queue: VecDeque<(String, PeerRecord)> = VecDeque::new();
        let mut active = HashSet::new();
        // A wake can describe data newer than an already running page. Keep one
        // follow-up per active worker instead of consuming that wake as a no-op.
        let mut pending_refresh = HashSet::new();
        let mut history = tokio::task::JoinSet::new();
        let mut history_owners: HashMap<tokio::task::Id, String> = HashMap::new();
        let mut documents = tokio::task::JoinSet::new();
        let mut document_owners: HashMap<tokio::task::Id, DocumentPeer> = HashMap::new();
        // An aborted read replays its exact checkpoint after resource release.
        let mut document_checkpoints: HashMap<tokio::task::Id, (PeerRecord, DocumentCheckpoint)> =
            HashMap::new();
        let mut document_aborts: HashMap<tokio::task::Id, tokio::task::AbortHandle> =
            HashMap::new();
        let mut document_preemptions = HashSet::new();
        let mut document_protected = HashSet::new();
        let mut document_resumes = HashSet::new();
        let mut control_pairs: HashMap<DocumentPeer, ControlComparisonPair> = HashMap::new();
        let mut document_selected_at_dispatch: HashMap<tokio::task::Id, ControlDemand> =
            HashMap::new();
        let mut same_source_preemptions = HashSet::new();
        let mut foreground_next = None;
        let mut document_cursors: HashMap<DocumentPeer, DocumentCheckpoint> = HashMap::new();
        let mut document_queues: [VecDeque<(DocumentPeer, PeerRecord)>; DOCUMENT_WORKERS] =
            std::array::from_fn(|_| VecDeque::new());
        let mut document_active: HashSet<DocumentPeer> = HashSet::new();
        let mut document_refresh: HashSet<DocumentPeer> = HashSet::new();
        // Cached projections survive while each independently owned cursor is
        // inside a network task; one completed lane cannot erase another
        // lane's failure or falsely declare its unfinished page done.
        let mut document_statuses: HashMap<DocumentPeer, DocumentStatus> = HashMap::new();
        let mut attachment_owners: HashMap<tokio::task::Id, String> = HashMap::new();
        let mut attachments = tokio::task::JoinSet::new();
        let mut operation_work_cursor: Option<super::AccountGroupOperationId> = None;
        let mut active_attachments = HashSet::new();
        let mut retry_attachment: HashMap<String, std::time::Instant> = HashMap::new();
        let mut unavailable: HashMap<(String, ConversationId), tokio::time::Instant> =
            HashMap::new();
        let mut attachment_unsupported: HashMap<String, tokio::time::Instant> = HashMap::new();
        let mut revision = self.device_sync_policy_snapshot().1;
        let mut custody_migration = None;
        let mut inventory_publication = None;
        let mut public_profile_publication = None;
        let mut personal_after = None;
        let mut personal_retry_at: Option<std::time::Instant> = None;
        let mut group_access_publication = None;
        let mut fanout = fanout::FanoutQueue::default();
        let mut fanout_cursor = 0u64;
        let mut fanout_tasks = tokio::task::JoinSet::new();
        let mut fanout_owners = HashMap::new();
        loop {
            if self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            {
                return;
            }
            if personal_retry_at.is_none_or(|at| at <= std::time::Instant::now())
                && let Some(conversation) = self.personal_cleanup_next(personal_after.as_ref())
            {
                personal_after = Some(conversation.clone());
                let result = self
                    .account_blocking(move |node| node.retry_personal_cleanup_for(&conversation))
                    .await;
                personal_retry_at = result
                    .is_err()
                    .then(|| std::time::Instant::now() + Duration::from_secs(5));
            }
            let affected = self.maintain_account_group_operation_effects();
            for (sequence, group) in self.account_group_fanout_page(fanout_cursor, fanout::CAPACITY)
            {
                if !fanout.observe(group, sequence) {
                    break;
                }
                fanout_cursor = sequence;
            }
            for group in affected {
                if let Some(sequence) = self.account_group_fanout_sequence(group) {
                    // A full queue leaves this group in the durable journal
                    // index. Do not advance the cursor past its older slot.
                    if !fanout.observe(group, sequence) {
                        fanout_cursor = fanout_cursor.min(sequence.saturating_sub(1));
                    }
                }
            }
            self.migrate_channel_custody(&mut custody_migration);
            self.publish_account_group_access(&mut group_access_publication);
            self.publish_group_inventory(&mut inventory_publication);
            if let Err(error) =
                self.publish_public_profile_documents_page(&mut public_profile_publication)
            {
                log::debug!("contact profile publication deferred: {error}");
            }
            // Explicit File copies and invitation capability/history preparation
            // share the existing attachment slots. Optional archive policy cannot stop
            // an already authorized account operation; control slots stay free.
            if attachments.len() < ATTACHMENT_WORKERS {
                let turn = self.account_group_data_work(operation_work_cursor);
                operation_work_cursor = turn.after;
                if turn.more {
                    self.account_sync_notify.notify_one();
                }
                for job in turn.work {
                    if attachments.len() >= ATTACHMENT_WORKERS {
                        break;
                    }
                    let worker_key = job.worker_key();
                    if active_attachments.contains(&worker_key)
                        || retry_attachment
                            .get(&worker_key)
                            .is_some_and(|at| *at > std::time::Instant::now())
                    {
                        continue;
                    }
                    let id = worker_key;
                    let file = job.conversation();
                    let operation = job.id();
                    let deadline = attachment_deadline(job.size());
                    let node = self.clone();
                    let Some(permit) = self.runtime_work.admit() else {
                        return;
                    };
                    active_attachments.insert(id.clone());
                    let owner = id.clone();
                    let task = attachments.spawn(permit.track(async move {
                        let result = tokio::time::timeout(
                            deadline,
                            node.execute_account_group_data_work(job),
                        )
                        .await
                        .unwrap_or_else(|_| {
                            Err(NodeError::File(
                                "account operation data exchange timed out".into(),
                            ))
                        })
                        .map(|_| AttachmentOutcome::Complete);
                        (id, file, false, result, Some(operation))
                    }));
                    attachment_owners.insert(task.id(), owner);
                }
            }
            // Reuse the cursor's existing authenticated observation-time rule
            // before calculating a paired cooldown. A successful newer proof
            // can retire an older failure without a fresh comparison bypass.
            {
                let support = self
                    .account_group_versions
                    .lock()
                    .expect("group versions mutex");
                let observed = |key: &DocumentPeer| {
                    support
                        .get(&key.device_id)
                        .filter(|value| value.public == key.public())
                        .map(|value| value.observed_at)
                };
                for (key, state) in &mut document_cursors {
                    state.observe_group_protocol(observed(key));
                }
                for (key, pair) in &mut control_pairs {
                    pair.parked.observe_group_protocol(observed(key));
                }
            }
            let selected_requests = self.pending_account_operation_source_requests();
            let selected: HashSet<_> = selected_requests.keys().copied().collect();
            let hints = self.pending_control_wakes();
            for (key, state) in &mut document_cursors {
                if key.lane == DocumentLane::Control
                    && !document_active.contains(key)
                    && !control_pairs.contains_key(key)
                    && state.refresh_timed_out_control_for_requests(
                        &key.public(),
                        selected_requests.get(&key.identity()),
                        hints.iter().find(|hint| hint.public == key.public()),
                    )
                {
                    // This failed initial negotiation has no parked page. The
                    // original pending request and newer authenticated source
                    // wake justify one fresh comparison, not a new retry owner.
                    #[cfg(test)]
                    self.trace_account_client(&key.public(), || {
                        "control_timeout_wake_refresh".into()
                    });
                }
            }
            let control_queue = &mut document_queues[DocumentLane::Control.index()];
            let ready = |key: &DocumentPeer| {
                control_pairs
                    .get(key)
                    .map_or_else(
                        || {
                            document_cursors
                                .get(key)
                                .and_then(|state: &DocumentCheckpoint| state.retry_at)
                        },
                        |pair| pair.deadline(document_cursors.get(key)),
                    )
                    .is_none_or(|at| at <= std::time::Instant::now())
            };
            #[cfg(test)]
            for (key, _) in control_queue.iter().filter(|(key, _)| {
                selected.contains(&key.identity())
                    || hints.iter().any(|hint| hint.public == key.public())
            }) {
                self.trace_account_client(&key.public(), || format!("control_queue selected={} hint={} ready={} retry_ms={} active_same={} active_other={}",
                    selected.contains(&key.identity()), hints.iter().any(|hint| hint.public == key.public()), ready(key),
                    document_cursors.get(key).and_then(|state| state.retry_at).map_or(0, |at| at.saturating_duration_since(std::time::Instant::now()).as_millis()),
                    document_active.contains(key), document_active.iter().any(|active| active.lane == DocumentLane::Control && active != key)));
            }
            let selected_waiting = control_queue
                .iter()
                .any(|(key, _)| selected.contains(&key.identity()) && ready(key));
            let wake_waiting = hints
                .iter()
                .find(|hint| {
                    control_queue
                        .iter()
                        .any(|(key, _)| key.public() == hint.public && ready(key))
                })
                .map(|hint| wake::wake_identity(&hint.public));
            for (key, pair) in &mut control_pairs {
                #[cfg(test)]
                let previous_followup = (pair.followup, pair.wake.clone());
                pair.observe(
                    selected_requests
                        .get(&key.identity())
                        .cloned()
                        .unwrap_or_default(),
                    hints.iter().find(|hint| hint.public == key.public()),
                );
                #[cfg(test)]
                if pair.followup && (!previous_followup.0 || previous_followup.1 != pair.wake) {
                    self.trace_account_client(&key.public(), || {
                        "control_pair_followup_observed".into()
                    });
                }
            }
            for (task, key) in &document_owners {
                let fresh_same_source = key.lane == DocumentLane::Control
                    && !control_pairs.contains_key(key)
                    && !document_protected.contains(task)
                    && ((selected_requests
                        .get(&key.identity())
                        .is_some_and(|generation| {
                            document_selected_at_dispatch
                                .get(task)
                                .is_none_or(|seen| !generation.is_subset(seen))
                        }))
                        || hints.iter().any(|hint| hint.public == key.public()));
                let wake_preempts = control_wake_preempts(
                    key.identity(),
                    document_protected.contains(task),
                    wake_waiting,
                );
                if key.lane == DocumentLane::Control
                    && (fresh_same_source
                        || (!selected.contains(&key.identity())
                            && (selected_waiting || wake_preempts)))
                    && document_preemptions.insert(*task)
                    && let Some(handle) = document_aborts.get(task)
                {
                    if fresh_same_source {
                        same_source_preemptions.insert(*task);
                    } else if !selected_waiting {
                        foreground_next = wake_waiting;
                    }
                    #[cfg(test)]
                    self.trace_account_client(&key.public(), || {
                        format!(
                            "control_cancel task={task:?} reason={}",
                            if fresh_same_source {
                                "same_source"
                            } else if selected_waiting {
                                "selected"
                            } else {
                                "wake"
                            }
                        )
                    });
                    #[cfg(test)]
                    self.trace_account_client(&key.public(), || {
                        format!(
                            "control_owned_abort reason={}",
                            if fresh_same_source {
                                "same_source"
                            } else if selected_waiting {
                                "selected"
                            } else {
                                "wake"
                            }
                        )
                    });
                    handle.abort();
                }
            }
            control_queue.make_contiguous().sort_by_key(|(key, _)| {
                control_queue_priority(
                    selected.contains(&key.identity()),
                    foreground_next == Some(key.identity())
                        || control_pairs
                            .get(key)
                            .is_some_and(ControlComparisonPair::prefix_turn_owed),
                    document_resumes.contains(key)
                        || control_pairs
                            .get(key)
                            .is_some_and(ControlComparisonPair::resume_turn_owed),
                    hints.iter().position(|hint| hint.public == key.public()),
                )
            });
            for lane in DocumentLane::ALL {
                // Reserve one existing slot per document lane. A held archive
                // or directory stream cannot occupy the control/optional slot.
                if document_active
                    .iter()
                    .any(|key: &DocumentPeer| key.lane == lane)
                {
                    continue;
                }
                let Some((key, peer)) = take_ready_mandatory_peer(
                    &mut document_queues[lane.index()],
                    |key| {
                        control_pairs.get(key).map_or_else(
                            || {
                                document_cursors
                                    .get(key)
                                    .and_then(|state: &DocumentCheckpoint| state.retry_at)
                            },
                            |pair| pair.deadline(document_cursors.get(key)),
                        )
                    },
                    std::time::Instant::now(),
                ) else {
                    continue;
                };
                if !document_active.insert(key.clone()) {
                    continue;
                }
                debug_assert!(document_active.len() <= DOCUMENT_WORKERS);
                debug_assert_eq!(
                    document_active
                        .iter()
                        .filter(|active| active.lane == lane)
                        .count(),
                    1
                );
                let mut state = document_cursors
                    .remove(&key)
                    .unwrap_or_else(|| lane.cursor().into());
                if key.lane == DocumentLane::Control {
                    let requests = selected_requests
                        .get(&key.identity())
                        .cloned()
                        .unwrap_or_default();
                    let hint = hints.iter().find(|hint| hint.public == key.public());
                    if !control_pairs.contains_key(&key)
                        && state.predates_control_demand(&requests, hint)
                    {
                        if state.control_comparison_complete() {
                            let refreshed = state.begin_refresh();
                            debug_assert!(refreshed);
                        } else {
                            // The old page has already released its resources.
                            // Move its exact continuation aside rather than
                            // querying a stale suffix before the new intent.
                            let foreground = state.fresh_control_comparison();
                            control_pairs.insert(
                                key.clone(),
                                ControlComparisonPair::new(state, requests.clone(), hint.cloned()),
                            );
                            state = foreground;
                        }
                    }
                    state.start_control_comparison(&requests, hint);
                }
                let node = self.clone();
                let Some(permit) = self.runtime_work.admit() else {
                    return;
                };
                let owner = key.clone();
                #[cfg(test)]
                let control_origin = (
                    document_resumes.contains(&key)
                        || control_pairs.get(&key).is_some_and(|pair| !pair.foreground),
                    foreground_next == Some(key.identity()),
                );
                let protected = key.lane == DocumentLane::Control
                    && (control_turn_protected(
                        document_resumes.remove(&key),
                        foreground_next == Some(key.identity()),
                    ) || control_pairs
                        .get(&key)
                        .is_some_and(ControlComparisonPair::current_turn_owed)
                        || selected.contains(&key.identity()));
                if key.lane == DocumentLane::Control {
                    if foreground_next == Some(key.identity()) {
                        foreground_next = None;
                    }
                    if let Some(hint) = hints.iter().find(|hint| hint.public == key.public()) {
                        self.consume_control_wake(hint);
                    }
                }
                #[cfg(test)]
                if key.lane == DocumentLane::Control {
                    self.trace_account_client(&key.public(), || {
                        format!(
                            "control_turn_origin resumed={} replacement={}",
                            control_origin.0, control_origin.1
                        )
                    });
                    self.trace_account_client(&key.public(), || {
                        format!(
                            "control_dispatch protected={} hint={}",
                            protected,
                            hints.iter().any(|hint| hint.public == key.public())
                        )
                    });
                }
                let checkpoint = (peer.clone(), state.clone());
                #[cfg(test)]
                let control_task_trace = (key.lane == DocumentLane::Control).then(|| {
                    format!(
                        "control_task paired={} protected={} requests={:?} resumed={} replacement={} hint={}",
                        control_pairs.contains_key(&key),
                        protected,
                        selected_requests
                            .get(&key.identity())
                            .cloned()
                            .unwrap_or_default(),
                        control_origin.0,
                        control_origin.1,
                        hints.iter().any(|hint| hint.public == key.public())
                    )
                });
                let task = documents.spawn(permit.track(async move {
                    #[cfg(test)]
                    let _lifetime = control_task_trace.map(|event| {
                        // Only the child owns this token across the pull future.
                        // Abort, return and panic drop it without fixture callbacks.
                        let lifetime = Arc::new(());
                        node.trace_control_task(&peer.public, event, Arc::downgrade(&lifetime));
                        lifetime
                    });
                    let _ = node.pull_account_documents_page(&peer, &mut state).await;
                    (key, peer, state)
                }));
                if protected {
                    document_protected.insert(task.id());
                }
                if let Some(generation) = selected_requests.get(&owner.identity()) {
                    document_selected_at_dispatch.insert(task.id(), generation.clone());
                }
                document_checkpoints.insert(task.id(), checkpoint);
                document_aborts.insert(task.id(), task.clone());
                document_owners.insert(task.id(), owner);
            }
            while let Some(group) = fanout.start_next() {
                // Bootstrap recovery may commit a group outside this turn's
                // ordinary publication slice. Refresh that exact source before
                // its owned public distribution can start.
                self.publish_account_group_access_for(group);
                self.publish_group_inventory_for(group);
                self.publish_account_group_operations_for(group);
                #[cfg(test)]
                {
                    let hook = self.control_fanout_start_test_hook.lock().unwrap().take();
                    if let Some(hook) = hook {
                        hook();
                    }
                }
                let node = self.clone();
                let Some(permit) = self.runtime_work.admit() else {
                    return;
                };
                let task = fanout_tasks.spawn(permit.track(async move {
                    let members = node.channel_members(group);
                    let deferred = node.distribute_account_group_effects(group, &members).await;
                    (group, deferred)
                }));
                fanout_owners.insert(task.id(), group);
            }
            while active.len() < HISTORY_WORKERS {
                let Some((id, peer)) = queue.pop_front() else {
                    break;
                };
                if !active.insert(id.clone()) {
                    continue;
                }
                let mut state = cursors.remove(&id).unwrap_or_default();
                if state
                    .history_retry_at
                    .is_some_and(|at| at > std::time::Instant::now())
                {
                    active.remove(&id);
                    cursors.insert(id, state);
                    continue;
                }
                state.history_retry_at = None;
                let node = self.clone();
                let Some(permit) = self.runtime_work.admit() else {
                    return;
                };
                let owner = id.clone();
                let task = history.spawn(permit.track(async move {
                    #[cfg(test)]
                    node.trace_account_client(&peer.public, || {
                        format!("history_begin after={}", state.next.is_some())
                    });
                    let result = tokio::time::timeout(
                        Duration::from_secs(20),
                        node.pull_account_history_page(&peer, state.next, state.known_digest),
                    )
                    .await
                    .unwrap_or_else(|_| {
                        Err(NodeError::Channel(
                            "account history exchange timed out".into(),
                        ))
                    });
                    #[cfg(test)]
                    node.trace_account_client(&peer.public, || {
                        format!(
                            "history_end outcome={}",
                            super::account_session::client_trace_outcome(&result)
                        )
                    });
                    state.favorite_error = None;
                    if result.is_ok() && node.device_sync_policy().history {
                        if state
                            .favorite_retry_at
                            .is_none_or(|at| at <= std::time::Instant::now())
                        {
                            state.favorite_retry_at = None;
                            let first = state.favorite_next.is_none();
                            #[cfg(test)]
                            node.trace_account_client(&peer.public, || "favorite_begin".into());
                            match tokio::time::timeout(
                                Duration::from_secs(10),
                                node.pull_message_favorites_page_at_digest(
                                    &peer,
                                    state.favorite_next.clone(),
                                    state.favorite_digest,
                                    &mut state.favorites_supported,
                                ),
                            )
                            .await
                            {
                                Ok(Ok((next, digest))) => {
                                    if first {
                                        state.favorite_sweep_digest = digest;
                                    }
                                    state.favorite_next = next;
                                    if state.favorite_next.is_none() {
                                        state.favorite_digest =
                                            if state.favorite_sweep_digest == digest {
                                                digest
                                            } else {
                                                [0; 32]
                                            };
                                    }
                                }
                                Ok(Err(error)) if local_sync_capacity_busy(&error) => {
                                    state.favorite_retry_at =
                                        Some(std::time::Instant::now() + Duration::from_secs(5));
                                }
                                failure => {
                                    state.favorite_next = None;
                                    state.favorite_digest = [0; 32];
                                    if state.favorites_supported {
                                        state.favorite_error = Some(match failure {
                                            Ok(Err(error)) => error.to_string(),
                                            _ => "favorites exchange timed out".into(),
                                        });
                                    }
                                }
                            }
                            #[cfg(test)]
                            node.trace_account_client(&peer.public, || {
                                format!("favorite_end failed={}", state.favorite_error.is_some())
                            });
                        }
                    } else if !result.as_ref().is_err_and(local_sync_capacity_busy) {
                        state.favorite_next = None;
                    }
                    // Selection belongs to this existing history worker. A slow
                    // local store must not park the parent document scheduler.
                    let missing = if node.device_sync_policy().attachments {
                        let favorites_supported = state.favorites_supported;
                        node.account_blocking(move |node| {
                            #[cfg(test)]
                            node.trace_account_client(&node.identity.public(), || {
                                "attachment_selection_begin".into()
                            });
                            let recalled = node.account_recalled_files();
                            let mut missing = node
                                .files
                                .lock()
                                .expect("files mutex not poisoned")
                                .file_convs();
                            missing.sort_by_key(|id| *id.as_bytes());
                            missing.retain(|file| {
                                !node.media.contains(*file) && !recalled.contains(file)
                            });
                            let mut queue: Vec<_> =
                                missing.into_iter().map(|file| (file, false)).collect();
                            if favorites_supported && node.device_sync_policy().history {
                                queue.extend(
                                    node.missing_favorite_files()
                                        .into_iter()
                                        .map(|file| (file, true)),
                                );
                            }
                            Ok(queue)
                        })
                        .await
                        .unwrap_or_default()
                    } else {
                        Vec::new()
                    };
                    (id, peer, state, result, missing)
                }));
                history_owners.insert(task.id(), owner);
            }
            let mut enqueue_all = false;
            let fanout_retry = fanout.next_retry();
            // Deferred peers keep their cursor and queued refresh, but do not
            // consume another worker/maintenance turn before the exact deadline.
            let document_retry = DocumentLane::ALL
                .into_iter()
                .filter(|lane| !document_active.iter().any(|key| key.lane == *lane))
                .filter_map(|lane| {
                    mandatory_retry_deadline(&document_queues[lane.index()], |key| {
                        control_pairs.get(key).map_or_else(
                            || document_cursors.get(key).and_then(|state| state.retry_at),
                            |pair| pair.deadline(document_cursors.get(key)),
                        )
                    })
                })
                .min();
            tokio::select! {
                _ = async {
                    if let Some(at) = document_retry {
                        tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {}
                _ = async {
                    if let Some(at) = fanout_retry {
                        tokio::time::sleep_until(at).await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => {}
                _ = async {
                    match tick.as_mut() {
                        Some(tick) => { tick.tick().await; }
                        None => std::future::pending::<()>().await,
                    }
                } => { enqueue_all = true; }
                _ = self.account_sync_notify.notified() => { enqueue_all = true; }
                result = fanout_tasks.join_next_with_id(), if !fanout_tasks.is_empty() => {
                    let completion = match result {
                        Some(Ok((task, completion))) => {
                            fanout_owners.remove(&task);
                            Some(completion)
                        },
                        Some(Err(error)) => {
                            log::debug!("account control fanout worker terminated: {error}");
                            fanout_owners.remove(&error.id()).map(|group| (group, false))
                        }
                        None => None,
                    };
                    if let Some((group, deferred)) = completion {
                        if deferred {
                            if !fanout.defer(group)
                                && let Some(sequence) = self.account_group_fanout_sequence(group)
                            {
                                fanout_cursor = fanout_cursor.min(sequence.saturating_sub(1));
                            }
                        } else if fanout.finished(group)
                            && let Some(sequence) = self.account_group_fanout_sequence(group)
                            && !fanout.observe(group, sequence)
                        {
                            fanout_cursor = fanout_cursor.min(sequence.saturating_sub(1));
                        }
                    }
                }
                result = documents.join_next_with_id(), if !documents.is_empty() => {
                    if let Some(Err(error)) = &result
                        && let Some(key) = document_owners.remove(&error.id()) {
                            let id = key.device_id.clone();
                            document_active.remove(&key);
                            document_aborts.remove(&error.id());
                            document_protected.remove(&error.id());
                            document_selected_at_dispatch.remove(&error.id());
                            let same_source = same_source_preemptions.remove(&error.id());
                            if document_preemptions.remove(&error.id()) {
                                #[cfg(test)]
                                self.trace_account_client(&key.public(), || "control_owned_abort_released".into());
                                if let Some((peer, state)) = document_checkpoints.remove(&error.id())
                                    && self.certified_sibling(&peer.public) {
                                    if same_source {
                                        let foreground = state.fresh_control_comparison();
                                        control_pairs.insert(key.clone(), ControlComparisonPair::new(state, selected_requests.get(&key.identity()).cloned().unwrap_or_default(), hints.iter().find(|hint| hint.public == key.public()).cloned()));
                                        document_cursors.insert(key.clone(), foreground);
                                    } else {
                                        document_cursors.insert(key.clone(), state);
                                    }
                                    if let Some(pair) = control_pairs.get(&key) {
                                        let projection = pair.status(document_cursors.get(&key).expect("paired current checkpoint"), self.device_sync_policy());
                                        document_statuses.insert(key.clone(), projection);
                                        let mut status = self.account_sync_status.lock().expect("account sync status mutex not poisoned")
                                            .get(&id).cloned().unwrap_or_else(|| initial_sync_status(id.clone()));
                                        let (phase, error, upgrade) = document_sync_status(&document_statuses, &peer.public);
                                        status.document_phase = phase;
                                        status.document_error = error;
                                        status.document_requires_upgrade = upgrade;
                                        self.set_account_sync_status(id.clone(), status);
                                    }
                                    document_resumes.insert(key.clone());
                                    document_queues[key.lane.index()].push_back((key, peer));
                                }
                            } else {
                            document_checkpoints.remove(&error.id());
                            document_refresh.remove(&key);
                            document_cursors.remove(&key);
                            control_pairs.remove(&key);
                            same_source_preemptions.remove(&error.id());
                            if self.certified_sibling(&key.public()) {
                            document_statuses.insert(key.clone(), (SyncPhase::Error,
                                Some("account document worker terminated".into()), false));
                            let mut status = self.account_sync_status.lock().expect("account sync status mutex not poisoned")
                                .get(&id).cloned().unwrap_or_else(|| initial_sync_status(id.clone()));
                            let (phase, error, upgrade) = document_sync_status(&document_statuses, &key.public());
                            status.document_phase = phase;
                            status.document_error = error;
                            status.document_requires_upgrade = upgrade;
                            self.set_account_sync_status(id, status);
                            } else { document_statuses.remove(&key); }
                            }
                    }
                    if let Some(Ok((task, (key, peer, mut state)))) = result {
                        document_checkpoints.remove(&task);
                        document_aborts.remove(&task);
                        document_preemptions.remove(&task);
                        document_protected.remove(&task);
                        document_selected_at_dispatch.remove(&task);
                        let same_source = same_source_preemptions.remove(&task);
                        let id = key.device_id.clone();
                        document_owners.remove(&task);
                        document_active.remove(&key);
                        let (policy, current_revision) = self.device_sync_policy_snapshot();
                        if current_revision != revision { enqueue_all = true; }
                        else if self.certified_sibling(&peer.public) {
                            if same_source {
                                let foreground = state.fresh_control_comparison();
                                control_pairs.insert(key.clone(), ControlComparisonPair::new(state, selected_requests.get(&key.identity()).cloned().unwrap_or_default(), hints.iter().find(|hint| hint.public == key.public()).cloned()));
                                state = foreground;
                            }
                            let paired = if same_source { Some(false) } else { control_pairs.get_mut(&key).map(|pair| pair.finish_page(&mut state)) };
                            if paired == Some(true) {
                                control_pairs.remove(&key);
                                #[cfg(test)] self.trace_account_client(&key.public(), || "control_pair_retired".into());
                            }
                            let refresh = document_refresh.contains(&key);
                            if !control_pairs.contains_key(&key) && refresh && state.begin_refresh() {
                                document_refresh.remove(&key);
                            }
                            let continuation = control_pairs.contains_key(&key) || (refresh && !state.admission_deferred()) || state.pending;
                            let projection = control_pairs.get(&key).map_or_else(|| state.status(policy), |pair| pair.status(&state, policy));
                            document_statuses.insert(key.clone(), projection);
                            let mut status = self.account_sync_status.lock().expect("account sync status mutex not poisoned")
                                .get(&id).cloned().unwrap_or_else(|| initial_sync_status(id.clone()));
                            let (phase, error, upgrade) = document_sync_status(&document_statuses, &peer.public);
                            status.document_phase = phase;
                            status.document_error = error;
                            status.document_requires_upgrade = upgrade;
                            self.set_account_sync_status(id.clone(), status);
                            document_cursors.insert(key.clone(), state);
                            if continuation {
                                document_queues[key.lane.index()].push_back((key, peer));
                                tokio::task::yield_now().await;
                            }
                        } else {
                            document_refresh.remove(&key);
                            document_statuses.remove(&key);
                            control_pairs.remove(&key);
                        }
                    }
                }
                result = history.join_next_with_id(), if !history.is_empty() => {
                    if let Some(Err(error)) = &result
                        && let Some(id) = history_owners.remove(&error.id()) {
                            active.remove(&id); pending_refresh.remove(&id); cursors.remove(&id);
                            let mut status = self.account_sync_status.lock().expect("account sync status mutex not poisoned")
                                .get(&id).cloned().unwrap_or_else(|| initial_sync_status(id.clone()));
                            status.phase = SyncPhase::Error;
                            status.error = Some("account history worker terminated".into());
                            self.set_account_sync_status(id, status);
                    }
                    if let Some(Ok((task, (id, peer, mut state, result, missing)))) = result {
                        history_owners.remove(&task);
                        active.remove(&id);
                        let (policy, current_revision) = self.device_sync_policy_snapshot();
                        if current_revision != revision { enqueue_all = true; }
                        else if self.certified_sibling(&peer.public) {
                            let refresh = pending_refresh.contains(&id);
                            let (document_phase,document_error,document_requires_upgrade)=document_sync_status(&document_statuses, &peer.public);
                            let mut status = DeviceSyncStatus { device_id: id.clone(), phase: SyncPhase::CatchingUp,
                                pending_attachments: 0, legacy_file_metadata: false, error: None,document_phase,document_error,document_requires_upgrade };
                            let favorites_waiting = state.favorite_retry_at.is_some_and(|at|at > std::time::Instant::now());
                            let favorites_pending = (state.favorite_next.is_some() || (state.favorites_supported && state.favorite_digest==[0;32] && state.favorite_error.is_none()))
                                && !favorites_waiting;
                            let mut continuation = favorites_pending;
                            match result {
                                Ok((next, digest, version)) => {
                                    status.legacy_file_metadata = version == AccountSyncVersion::V1;
                                    let history_complete = state.complete_page(next, digest);
                                    if history_complete && !favorites_pending && !favorites_waiting { status.phase = SyncPhase::UpToDate; }
                                    else { continuation |= !history_complete; }
                                    if let Some(error) = &state.favorite_error {
                                        status.phase = SyncPhase::Error;
                                        status.error = Some(error.chars().take(160).collect());
                                        // A favorites failure alone must not request another turn.
                                        // Preserve unfinished history and the coalesced refresh below;
                                        // consuming that wake before clearing continuation loses work.
                                        continuation = !history_complete;
                                    }
                                    // Finish retained history/favorite pages before consuming
                                    // one coalesced wake as a fresh comparison sweep.
                                    if refresh && history_complete && state.favorite_next.is_none() {
                                        pending_refresh.remove(&id);
                                        continuation = true;
                                    }
                                }
                                Err(error) if local_sync_capacity_busy(&error) => {
                                    // Admission never began an exchange. Retain history and
                                    // favorite cursors; the regular tick retries without a spin.
                                    status.phase = SyncPhase::CatchingUp;
                                    state.history_retry_at = Some(std::time::Instant::now() + Duration::from_secs(5));
                                    continuation = false;
                                }
                                Err(error) => {
                                    status.phase = SyncPhase::Error;
                                    status.error = Some(error.to_string().chars().take(160).collect());
                                }
                            }
                            if policy.attachments {
                                unavailable.retain(|_, until| *until > tokio::time::Instant::now());
                                attachment_unsupported.retain(|_, until| *until > tokio::time::Instant::now());
                                status.pending_attachments = missing.len();
                                let selected = (0..missing.len()).map(|offset| (state.next_file.wrapping_add(offset)) % missing.len())
                                    .find(|index| !unavailable.contains_key(&(id.clone(), missing[*index].0)));
                                if let Some(index) = selected.filter(|_| attachments.len() < ATTACHMENT_WORKERS
                                    && !active_attachments.contains(&id)
                                    && retry_attachment.get(&id).is_none_or(|at| *at <= std::time::Instant::now())) {
                                    let (file,favorite) = missing[index];
                                    state.next_file = index.wrapping_add(1);
                                    active_attachments.insert(id.clone());
                                    let node = self.clone(); let peer = peer.clone(); let id = id.clone();
                                    let version = if status.legacy_file_metadata { AccountSyncVersion::V1 } else { AccountSyncVersion::V2 };
                                    let transfer_size = if favorite { self.favorite_file_size(file).unwrap_or(0) } else { self.files.lock().expect("files mutex not poisoned")
                                        .manifest(&file).map_or(0, |manifest| manifest.size()) };
                                    // Bound the complete exchange without rejecting large legitimate
                                    // archives solely because they need more than a fixed short timeout.
                                    let deadline = attachment_deadline(transfer_size);
                                    let Some(permit) = self.runtime_work.admit() else { return; };
                                    let owner = id.clone();
                                    let checked = !attachment_unsupported.contains_key(&id);
                                    let task = attachments.spawn(permit.track(async move {
                                        let mut unsupported = false;
                                        let result = tokio::time::timeout(deadline, async {
                                            if favorite {
                                                if node.account_blocking(move |node|node.preserve_favorite_file(file,revision)).await? {Ok(AttachmentOutcome::Complete)}
                                                else {node.pull_favorite_attachment(&peer,file,revision).await.map(|_|AttachmentOutcome::Complete)}
                                            } else if node.account_blocking(move |node| node.preserve_available_attachment(file, revision)).await? {
                                                Ok(AttachmentOutcome::Complete)
                                            } else { Box::pin(node.pull_account_attachment_optional(&peer, file, version, checked, &mut unsupported)).await }
                                        }).await.unwrap_or_else(|_| Err(NodeError::File("account attachment exchange timed out".into())));
                                        (id, file, unsupported, result, None)
                                    }));
                                    attachment_owners.insert(task.id(), owner);
                                }
                                if status.pending_attachments > 0 && status.phase == SyncPhase::UpToDate {
                                    status.phase = if missing.iter().any(|(file,_)|unavailable.contains_key(&(id.clone(),*file))) { SyncPhase::WaitingForFiles } else { SyncPhase::CatchingUp };
                                }
                            }
                            if continuation && matches!(status.phase,SyncPhase::UpToDate|SyncPhase::WaitingForFiles) {
                                status.phase = SyncPhase::CatchingUp;
                            }
                            self.set_account_sync_status(id.clone(), status);
                            cursors.insert(id.clone(), state);
                            if continuation {
                                // One page per peer per turn: tail insertion gives waiting siblings
                                // their turn while bounded workers immediately drain the backlog.
                                queue.push_back((id, peer));
                                tokio::task::yield_now().await;
                            }
                        }
                    }
                }
                result = attachments.join_next_with_id(), if !attachments.is_empty() => {
                    if let Some(Err(error)) = &result
                        && let Some(id) = attachment_owners.remove(&error.id()) {
                            active_attachments.remove(&id);
                            retry_attachment.insert(id, std::time::Instant::now() + RETRY_INTERVAL);
                    }
                    if let Some(Ok((task, (id, file, unsupported, result, file_operation)))) = result {
                        attachment_owners.remove(&task);
                        active_attachments.remove(&id);
                        if file_operation.is_some() {
                            if result.is_ok() {
                                retry_attachment.remove(&id);
                            } else {
                                retry_attachment.insert(id.clone(), std::time::Instant::now() + RETRY_INTERVAL);
                            }
                            self.account_sync_notify.notify_one();
                            enqueue_all = true;
                        } else {
                        if unsupported {
                            attachment_unsupported.insert(id.clone(), tokio::time::Instant::now() + ATTACHMENT_AVAILABILITY_TTL);
                        }
                        if let Err(error) = result {
                            retry_attachment.insert(id.clone(), std::time::Instant::now() + RETRY_INTERVAL);
                            let previous_status = self.account_sync_status.lock().expect("account sync status mutex not poisoned").get(&id).cloned();
                            if let Some(mut status) = previous_status && !local_sync_capacity_busy(&error) {
                                status.phase = SyncPhase::Error;
                                status.error = Some(error.to_string().chars().take(160).collect());
                                self.set_account_sync_status(id, status);
                            }
                        } else {
                            retry_attachment.remove(&id);
                            if matches!(result, Ok(AttachmentOutcome::Unavailable | AttachmentOutcome::SourceDisabled)) {
                                if unavailable.len() >= MAX_UNAVAILABLE_ATTACHMENTS {
                                    // Keep the metadata bound without an immediate eviction/reprobe loop.
                                    retry_attachment.insert(id.clone(),std::time::Instant::now()+RETRY_INTERVAL);
                                    if let Some(oldest)=unavailable.iter().min_by_key(|(_,until)|**until).map(|(key,_)|key.clone()) { unavailable.remove(&oldest); }
                                }
                                unavailable.insert((id.clone(),file),tokio::time::Instant::now()+ATTACHMENT_AVAILABILITY_TTL);

                            }
                            enqueue_all = true;
                        }
                        }
                    }
                }
            }
            let (_, current_revision) = self.device_sync_policy_snapshot();
            if current_revision != revision {
                // Cancellation drops every network future. Blocking writers retain retirement
                // admission and perform their own current-policy checks before commit.
                history.abort_all();
                documents.abort_all();
                attachments.abort_all();
                while history.join_next().await.is_some() {}
                while documents.join_next().await.is_some() {}
                while attachments.join_next().await.is_some() {}
                active.clear();
                pending_refresh.clear();
                active_attachments.clear();
                history_owners.clear();
                document_owners.clear();
                document_checkpoints.clear();
                document_aborts.clear();
                document_preemptions.clear();
                document_protected.clear();
                document_resumes.clear();
                control_pairs.clear();
                document_selected_at_dispatch.clear();
                same_source_preemptions.clear();
                foreground_next = None;
                self.clear_control_wakes();
                document_active.clear();
                document_refresh.clear();
                for queue in &mut document_queues {
                    queue.clear();
                }
                document_cursors.clear();
                document_statuses.clear();
                attachment_owners.clear();
                queue.clear();
                cursors.clear();
                retry_attachment.clear();
                unavailable.clear();
                attachment_unsupported.clear();
                revision = current_revision;
                enqueue_all = true;
            }
            if enqueue_all {
                let devices = self.linked_devices();
                let certified: HashSet<_> = devices
                    .iter()
                    .map(|device| device.public.user_id())
                    .collect();
                // Certification churn must not retain retired peer cursors or
                // queued retries. Active exchanges retain their exact admission
                // and recheck current authority before any durable import.
                cursors.retain(|id, _| certified.contains(id));
                let document_identities: HashSet<_> = devices
                    .iter()
                    .map(|device| (device.public.ed25519_pub, device.public.x25519_pub))
                    .collect();
                document_cursors.retain(|key, _| document_identities.contains(&key.identity()));
                control_pairs.retain(|key, _| document_identities.contains(&key.identity()));
                document_statuses.retain(|key, _| document_identities.contains(&key.identity()));
                document_resumes.retain(|key| document_identities.contains(&key.identity()));
                if foreground_next.is_some_and(|identity| !document_identities.contains(&identity))
                {
                    foreground_next = None;
                }
                queue.retain(|(id, _)| certified.contains(id));
                for queue in &mut document_queues {
                    queue.retain(|(key, _)| document_identities.contains(&key.identity()));
                }
                pending_refresh.retain(|id| certified.contains(id));
                document_refresh.retain(|key| document_identities.contains(&key.identity()));
                for device in devices {
                    let id = device.public.user_id();
                    if id == self.user_id() {
                        continue;
                    }
                    for lane in DocumentLane::ALL {
                        let key = DocumentPeer::new(&device.public, lane);
                        let queue = &mut document_queues[lane.index()];
                        #[cfg(test)]
                        if lane == DocumentLane::Control {
                            self.trace_account_client(&key.public(), || {
                                format!(
                                    "control_refresh active={} queued={} retry_ms={}",
                                    document_active.contains(&key),
                                    queue.iter().any(|(queued, _)| queued == &key),
                                    document_cursors
                                        .get(&key)
                                        .and_then(|state| state.retry_at)
                                        .map_or(0, |at| at
                                            .saturating_duration_since(std::time::Instant::now())
                                            .as_millis())
                                )
                            });
                        }
                        if document_active.contains(&key) {
                            document_refresh.insert(key);
                        } else if !queue.iter().any(|(queued, _)| queued == &key)
                            && let Some(peer) = self.routing_peer(&id)
                            && peer.public == device.public
                        {
                            if !control_pairs.contains_key(&key)
                                && let Some(state) = document_cursors.get_mut(&key)
                                && !state.begin_refresh()
                            {
                                document_refresh.insert(key.clone());
                            }
                            queue.push_back((key, peer));
                        }
                    }
                    if active.contains(&id) {
                        pending_refresh.insert(id);
                        continue;
                    }
                    if queue.iter().any(|(queued, _)| queued == &id) {
                        continue;
                    }
                    let Some(peer) = self.routing_peer(&id) else {
                        self.set_account_sync_status(
                            id.clone(),
                            DeviceSyncStatus {
                                device_id: id,
                                phase: SyncPhase::Offline,
                                pending_attachments: 0,
                                legacy_file_metadata: false,
                                document_phase: SyncPhase::Offline,
                                document_error: None,
                                document_requires_upgrade: false,
                                error: None,
                            },
                        );
                        continue;
                    };
                    if peer.public != device.public {
                        self.set_account_sync_status(
                            id.clone(),
                            DeviceSyncStatus {
                                device_id: id,
                                phase: SyncPhase::Error,
                                pending_attachments: 0,
                                legacy_file_metadata: false,
                                document_phase: SyncPhase::Error,
                                document_error: Some("discovered device identity changed".into()),
                                document_requires_upgrade: false,
                                error: Some("discovered device identity changed".into()),
                            },
                        );
                        continue;
                    }
                    if cursors
                        .get(&id)
                        .is_some_and(|state| state.next.is_some() || state.favorite_next.is_some())
                    {
                        pending_refresh.insert(id.clone());
                    }
                    queue.push_back((id, peer));
                }
            }
            // Self notifications may already be ready on every turn. Let the
            // bounded data and public fanout tasks run before another synchronous
            // maintenance/publication pass; a ready Notify is not a yield.
            tokio::task::yield_now().await;
        }
    }

    fn set_account_sync_status(&self, id: String, status: DeviceSyncStatus) {
        let mut statuses = self
            .account_sync_status
            .lock()
            .expect("account sync status mutex not poisoned");
        if statuses.get(&id) != Some(&status) {
            statuses.insert(id, status);
            drop(statuses);
            self.note_ui_change();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::account::Account;

    #[test]
    fn timed_out_control_demand_requires_original_request_and_same_full_peer() {
        use super::super::account_documents::sync::DocumentSyncCursor;
        let peer = crate::identity::device::DeviceIdentity::generate().public();
        let failed = std::time::Instant::now();
        let original: DocumentCheckpoint = DocumentSyncCursor::control_test_timeout(failed).into();
        let request =
            super::super::AccountGroupOperationId::from_hex(&hex::encode([19; 32])).unwrap();
        let demand = ControlDemand::from([request]);
        let mut hint = wake::ControlWakeHint::test_token(peer.clone(), [17; 16], 1);
        hint.received_at = failed + Duration::from_millis(1);
        let mut cursor = original.clone();
        assert!(!cursor.refresh_timed_out_control_for_requests(&peer, None, Some(&hint)));
        assert!(!cursor.refresh_timed_out_control_for_requests(
            &peer,
            Some(&ControlDemand::new()),
            Some(&hint)
        ));
        assert!(!cursor.refresh_timed_out_control_for_requests(&peer, Some(&demand), None));
        let mut wrong_peer = peer.clone();
        wrong_peer.x25519_pub[0] ^= 1;
        assert!(!cursor.refresh_timed_out_control_for_requests(
            &wrong_peer,
            Some(&demand),
            Some(&hint)
        ));
        assert_eq!(
            cursor, original,
            "polling, absent original demand and another full identity retain the cooldown"
        );
        assert!(cursor.refresh_timed_out_control_for_requests(&peer, Some(&demand), Some(&hint)));
        assert!(!cursor.refresh_timed_out_control_for_requests(&peer, Some(&demand), Some(&hint)));
    }

    #[test]
    fn timed_out_control_accepts_one_new_wake_received_during_the_failed_attempt() {
        use super::super::account_documents::sync::DocumentSyncCursor;
        let peer = crate::identity::device::DeviceIdentity::generate().public();
        let started = std::time::Instant::now();
        let failed = started + Duration::from_secs(3);
        let original: DocumentCheckpoint =
            DocumentSyncCursor::control_test_timeout_window(started, failed).into();
        let demand = ControlDemand::from([super::super::AccountGroupOperationId::for_origin(
            &peer, [29; 16],
        )]);
        let mut hint = wake::ControlWakeHint::test_token(peer.clone(), [27; 16], 1);
        hint.received_at = started;
        let mut checkpoint = original.clone();
        assert!(!checkpoint.refresh_timed_out_control_for_requests(
            &peer,
            Some(&demand),
            Some(&hint)
        ));
        assert_eq!(checkpoint, original, "a pre-attempt token retains backoff");
        hint.received_at = started + Duration::from_millis(1);
        assert!(hint.received_at < failed);
        assert!(checkpoint.refresh_timed_out_control_for_requests(
            &peer,
            Some(&demand),
            Some(&hint)
        ));
        assert!(!checkpoint.refresh_timed_out_control_for_requests(
            &peer,
            Some(&demand),
            Some(&hint)
        ));
        let next_started = failed + Duration::from_millis(1);
        let mut retry: DocumentCheckpoint = DocumentSyncCursor::control_test_timeout_window(
            next_started,
            next_started + Duration::from_secs(3),
        )
        .into();
        assert!(
            !retry.refresh_timed_out_control_for_requests(&peer, Some(&demand), Some(&hint)),
            "the same received token cannot renew a later failure"
        );
        let mut newer = wake::ControlWakeHint::test_token(peer.clone(), [27; 16], 2);
        newer.received_at = next_started + Duration::from_millis(1);
        assert!(retry.refresh_timed_out_control_for_requests(&peer, Some(&demand), Some(&newer)));
        assert!(!retry.refresh_timed_out_control_for_requests(&peer, Some(&demand), Some(&newer)));
    }

    #[test]
    fn control_checkpoint_preserves_comparison_start_across_partial_pages_and_rollback() {
        use super::super::account_documents::{
            AccountDocumentKey, sync::DocumentSyncCursor as Cursor,
        };
        let first = super::super::AccountGroupOperationId::from_hex(&hex::encode([1; 32])).unwrap();
        let second =
            super::super::AccountGroupOperationId::from_hex(&hex::encode([2; 32])).unwrap();
        let initial = ControlDemand::from([first]);
        let newer = ControlDemand::from([first, second]);
        let public = PublicIdentity {
            ed25519_pub: [3; 32],
            x25519_pub: [4; 32],
        };
        let old = wake::ControlWakeHint::test_token(public.clone(), [1; 16], 2);
        let next = wake::ControlWakeHint::test_token(public, [1; 16], 3);
        let key = AccountDocumentKey::GroupOperation {
            channel: "08".repeat(32),
            source_device: "01".repeat(16),
        };
        let mut queued: DocumentCheckpoint = Cursor::control_test_page(Some(key)).into();
        queued.retry_at = Some(std::time::Instant::now() + Duration::from_secs(5));
        queued.start_control_comparison(&initial, Some(&old));
        let saved = queued.clone();
        for _ in 0..64 {
            queued.start_control_comparison(&newer, Some(&next));
            assert!(!queued.predates_control_demand(&initial, Some(&old)));
            assert!(!queued.predates_control_demand(&ControlDemand::new(), None));
        }
        assert_eq!(
            queued, saved,
            "continuation/rollback retains the original comparison witness"
        );
        assert!(queued.predates_control_demand(&newer, Some(&old)));
        assert!(queued.predates_control_demand(&initial, Some(&next)));
        let mut foreground = queued.fresh_control_comparison();
        assert!(foreground.control_start.is_none());
        foreground.start_control_comparison(&newer, Some(&next));
        assert!(!foreground.predates_control_demand(&newer, Some(&next)));
        assert_eq!(
            queued, saved,
            "fresh comparison cannot alter its parked checkpoint or cooldown"
        );
    }

    #[test]
    fn control_checkpoint_refresh_clears_start_only_after_comparison_completion() {
        use super::super::account_documents::{
            AccountDocumentKey, sync::DocumentSyncCursor as Cursor,
        };
        let requests =
            ControlDemand::from([
                super::super::AccountGroupOperationId::from_hex(&hex::encode([1; 32])).unwrap(),
            ]);
        let mut state: DocumentCheckpoint =
            Cursor::control_test_page(Some(AccountDocumentKey::GroupOperation {
                channel: "08".repeat(32),
                source_device: "01".repeat(16),
            }))
            .into();
        state.start_control_comparison(&requests, None);
        let saved = state.clone();
        assert!(!state.begin_refresh());
        assert_eq!(
            state, saved,
            "refresh cannot reset an unfinished continuation"
        );
        state.cursor = Cursor::control_test_page(None);
        assert!(state.begin_refresh());
        assert!(state.control_start.is_none());
        state.start_control_comparison(&ControlDemand::new(), None);
        assert!(state.predates_control_demand(&requests, None));
    }

    #[test]
    fn paid_pair_credit_is_spent_after_prefix_and_original_resume_without_followup_renewal() {
        use super::super::account_documents::{
            AccountDocumentKey, sync::DocumentSyncCursor as Cursor,
        };
        let background: DocumentCheckpoint =
            Cursor::control_test_page(Some(AccountDocumentKey::GroupLegacyOperation {
                channel: "4".repeat(64),
                source_device: "1".repeat(32),
            }))
            .into();
        let foreground: DocumentCheckpoint =
            Cursor::control_test_page(Some(AccountDocumentKey::GroupOperation {
                channel: "2".repeat(64),
                source_device: "1".repeat(32),
            }))
            .into();
        let mut pair = ControlComparisonPair::new(background.clone(), ControlDemand::new(), None);
        let mut unpaid: DocumentCheckpoint = Cursor::control().into();
        assert!(!pair.finish_page(&mut unpaid));
        assert!(
            pair.prefix_turn_owed(),
            "a failed/admission-deferred prefix did not pay a page"
        );
        let mut current = foreground.clone();
        assert!(!pair.finish_page(&mut current));
        assert_eq!(
            current, background,
            "the exact interrupted checkpoint owns the next protected turn"
        );
        assert!(pair.resume_turn_owed());
        assert!(!pair.finish_page(&mut unpaid));
        assert!(
            pair.resume_turn_owed(),
            "a failed resume cannot consume its credit"
        );
        current = Cursor::control_test_page(None).into();
        assert!(!pair.finish_page(&mut current));
        assert_eq!(
            current, foreground,
            "paying the resume must retain the still-partial foreground"
        );
        assert!(pair.foreground && !pair.current_turn_owed());
        assert!(
            control_queue_priority(false, false, false, Some(0))
                < control_queue_priority(
                    false,
                    pair.prefix_turn_owed(),
                    pair.resume_turn_owed(),
                    None
                ),
            "a fresh authenticated sender precedes unrelated work after both initial credits are paid"
        );
        let hint = wake::ControlWakeHint::test_token(
            crate::identity::device::DeviceIdentity::generate().public(),
            [9; 16],
            1,
        );
        pair.observe(ControlDemand::new(), Some(&hint));
        current = Cursor::control_test_page(None).into();
        assert!(!pair.finish_page(&mut current));
        assert!(
            !pair.current_turn_owed(),
            "coalesced publication followups never renew global priority"
        );
    }

    #[test]
    fn completed_parked_control_needs_no_resume_credit() {
        use super::super::account_documents::{
            AccountDocumentKey, sync::DocumentSyncCursor as Cursor,
        };
        let background = Cursor::control_test_page(None).into();
        let mut current = Cursor::control_test_page(Some(AccountDocumentKey::GroupOperation {
            channel: "2".repeat(64),
            source_device: "1".repeat(32),
        }))
        .into();
        let mut pair = ControlComparisonPair::new(background, ControlDemand::new(), None);
        assert!(pair.prefix_turn_owed());
        assert!(!pair.finish_page(&mut current));
        assert!(
            !pair.current_turn_owed(),
            "an already completed parked scope has no interrupted page to repay"
        );
    }

    #[test]
    fn paired_control_pages_preserve_both_partial_checkpoints_and_alternate() {
        use super::super::account_documents::{
            AccountDocumentKey, sync::DocumentSyncCursor as Cursor,
        };
        let key = |value: u8| AccountDocumentKey::GroupOperation {
            channel: hex::encode([value; 32]),
            source_device: "01".repeat(16),
        };
        let mut background: DocumentCheckpoint = Cursor::control_test_page(Some(key(8))).into();
        background.retry_at = Some(std::time::Instant::now() + Duration::from_secs(5));
        background.start_control_comparison(&ControlDemand::new(), None);
        let original = background.clone();
        let mut pair = ControlComparisonPair::new(background, ControlDemand::new(), None);
        let mut current: DocumentCheckpoint = Cursor::control_test_page(Some(key(2))).into();
        let demand =
            ControlDemand::from([
                super::super::AccountGroupOperationId::from_hex(&hex::encode([1; 32])).unwrap(),
            ]);
        current.start_control_comparison(&demand, None);
        let foreground = current.clone();
        assert!(!pair.finish_page(&mut current));
        assert!(!pair.foreground);
        assert_eq!(
            current, original,
            "background resumes its exact original key/digests/retry"
        );
        assert_eq!(
            pair.parked, foreground,
            "foreground continuation is retained independently"
        );
        assert_eq!(pair.deadline(Some(&current)), original.retry_at);
        let background = current.clone();
        assert!(!pair.finish_page(&mut current));
        assert!(pair.foreground);
        assert_eq!(current, foreground);
        assert_eq!(pair.parked, background);
        assert_eq!(
            pair.status(&current, DeviceSyncPolicy::default()).0,
            SyncPhase::CatchingUp
        );
        current = Cursor::control_test_page(None).into();
        assert!(!pair.finish_page(&mut current));
        assert!(!pair.foreground);
        current = Cursor::control_test_page(None).into();
        assert!(pair.finish_page(&mut current));
        assert!(current.control_comparison_complete());
    }

    #[test]
    fn paired_control_new_publication_reuses_completed_background_without_dropping_partial_foreground()
     {
        use super::super::account_documents::{
            AccountDocumentKey, sync::DocumentSyncCursor as Cursor,
        };
        let key = AccountDocumentKey::GroupOperation {
            channel: "08".repeat(32),
            source_device: "01".repeat(16),
        };
        let first = super::super::AccountGroupOperationId::from_hex(&hex::encode([1; 32])).unwrap();
        let requests = ControlDemand::from([first]);
        let mut background: DocumentCheckpoint =
            Cursor::control_test_page(Some(key.clone())).into();
        background.start_control_comparison(&requests, None);
        let mut pair = ControlComparisonPair::new(background, requests.clone(), None);
        let mut foreground: DocumentCheckpoint = Cursor::control_test_page(Some(key)).into();
        foreground.start_control_comparison(&requests, None);
        foreground.retry_at = Some(std::time::Instant::now() + Duration::from_secs(10));
        let original_foreground = foreground.clone();
        assert!(!pair.finish_page(&mut foreground));
        assert!(!pair.foreground);
        foreground.cursor = Cursor::control_test_page(None);
        foreground.retry_at = Some(std::time::Instant::now() + Duration::from_secs(5));
        let public = PublicIdentity {
            ed25519_pub: [3; 32],
            x25519_pub: [4; 32],
        };
        let token = wake::ControlWakeHint::test_token(public, [1; 16], 3);
        pair.observe(requests, Some(&token));
        assert!(pair.followup);
        let mut expected_fresh = foreground.clone();
        assert!(expected_fresh.begin_refresh());
        assert!(!pair.finish_page(&mut foreground));
        assert_eq!(
            foreground, expected_fresh,
            "new publication must use the completed slot for a fresh prefix before the obsolete foreground suffix"
        );
        assert!(pair.foreground && !foreground.control_comparison_complete());
        assert_eq!(
            pair.parked, original_foreground,
            "unfinished foreground checkpoint must become the exact parked background"
        );
        assert_eq!(
            pair.deadline(Some(&foreground)),
            original_foreground.retry_at
        );
        assert!(foreground.control_start.is_none());
        assert!(!pair.followup);
        foreground.cursor = Cursor::control_test_page(None);
        assert!(!pair.finish_page(&mut foreground));
        assert!(!pair.foreground);
        assert_eq!(
            foreground, original_foreground,
            "new prefix pays one protected old page before its next continuation"
        );
        foreground.cursor = Cursor::control_test_page(None);
        assert!(pair.finish_page(&mut foreground));
        assert!(foreground.up_to_date);
    }

    #[test]
    fn paired_control_recycles_parked_completed_background_only_for_genuine_followup() {
        use super::super::account_documents::{
            AccountDocumentKey, sync::DocumentSyncCursor as Cursor,
        };
        let first = super::super::AccountGroupOperationId::from_hex(&hex::encode([1; 32])).unwrap();
        let second =
            super::super::AccountGroupOperationId::from_hex(&hex::encode([2; 32])).unwrap();
        let requests = ControlDemand::from([first]);
        let mut background: DocumentCheckpoint = Cursor::control_test_page(None).into();
        background.start_control_comparison(&ControlDemand::new(), None);
        background.retry_at = Some(std::time::Instant::now() + Duration::from_secs(10));
        let mut expected = background.clone();
        assert!(expected.begin_refresh());
        let mut pair = ControlComparisonPair::new(background, requests.clone(), None);
        let mut foreground: DocumentCheckpoint =
            Cursor::control_test_page(Some(AccountDocumentKey::GroupOperation {
                channel: "08".repeat(32),
                source_device: "01".repeat(16),
            }))
            .into();
        foreground.start_control_comparison(&requests, None);
        foreground.retry_at = Some(std::time::Instant::now() + Duration::from_secs(5));
        let saved = foreground.clone();
        for _ in 0..64 {
            pair.observe(requests.clone(), None);
        }
        assert!(!pair.finish_page(&mut foreground));
        assert_eq!(
            foreground, saved,
            "unchanged demand cannot discard or rotate a partial foreground"
        );
        pair.observe(ControlDemand::from([first, second]), None);
        assert!(!pair.finish_page(&mut foreground));
        assert_eq!(
            foreground, expected,
            "completed parked slot becomes the fresh foreground"
        );
        assert!(pair.foreground && !pair.followup);
        assert_eq!(pair.parked, saved);
        assert_eq!(pair.deadline(Some(&foreground)), expected.retry_at);
    }

    #[test]
    fn paired_control_deferral_keeps_owner_and_waits_for_both_retry_deadlines() {
        use super::super::account_documents::sync::DocumentSyncCursor as Cursor;
        let mut background: DocumentCheckpoint = Cursor::control().into();
        background.retry_at = Some(std::time::Instant::now() + Duration::from_secs(5));
        let saved = background.clone();
        let mut current = background.fresh_control_comparison();
        current.retry_at = Some(std::time::Instant::now() + Duration::from_secs(10));
        let latest = current.retry_at;
        let mut pair = ControlComparisonPair::new(background, ControlDemand::new(), None);
        assert!(
            !pair.finish_page(&mut current),
            "no validated page means no fairness turn paid"
        );
        assert!(pair.foreground);
        assert_eq!(pair.parked, saved);
        assert_eq!(pair.deadline(Some(&current)), latest);
        assert_eq!(
            pair.parked.retry_at, saved.retry_at,
            "shared readiness never overwrites exact parked retry"
        );
    }

    #[test]
    fn paired_control_followup_requires_a_new_admitted_operation_or_wake_token() {
        use super::super::account_documents::{
            AccountDocumentKey, sync::DocumentSyncCursor as Cursor,
        };
        let partial = AccountDocumentKey::GroupOperation {
            channel: "08".repeat(32),
            source_device: "01".repeat(16),
        };
        let first = super::super::AccountGroupOperationId::from_hex(&hex::encode([1; 32])).unwrap();
        let second =
            super::super::AccountGroupOperationId::from_hex(&hex::encode([2; 32])).unwrap();
        let initial = ControlDemand::from([first]);
        let mut pair = ControlComparisonPair::new(
            Cursor::control_test_page(Some(partial)).into(),
            initial.clone(),
            None,
        );
        let mut current = Cursor::control_test_page(None).into();
        for _ in 0..64 {
            pair.observe(initial.clone(), None);
        }
        assert!(
            !pair.followup,
            "polls and unchanged operation IDs do not rearm completed foreground"
        );
        assert!(!pair.finish_page(&mut current));
        assert!(
            !pair.foreground,
            "one background page is owed after foreground completion"
        );
        pair.observe(ControlDemand::from([first, second]), None);
        assert!(pair.followup);
        let background = current.clone();
        assert!(!pair.finish_page(&mut current));
        assert!(pair.foreground && !current.control_comparison_complete());
        assert_eq!(pair.parked, background);
        assert!(!pair.followup);
        for _ in 0..64 {
            pair.observe(ControlDemand::from([first, second]), None);
        }
        assert!(
            !pair.followup,
            "the same admitted request cannot create a third comparison"
        );
        for value in 3..=2048u64 {
            pair.observe(ControlDemand::new(), None);
            let mut bytes = [0; 32];
            bytes[..8].copy_from_slice(&value.to_be_bytes());
            let id = super::super::AccountGroupOperationId::from_hex(&hex::encode(bytes)).unwrap();
            pair.observe(ControlDemand::from([id]), None);
            assert_eq!(
                pair.local_requests.len(),
                1,
                "terminal/new admission cycles retain only the current bounded journal snapshot"
            );
        }
        let peer = PublicIdentity {
            ed25519_pub: [3; 32],
            x25519_pub: [4; 32],
        };
        let old = wake::ControlWakeHint::test_token(peer.clone(), [1; 16], 2);
        pair.wake = Some(old.clone());
        pair.followup = false;
        let ids = pair.local_requests.clone();
        for _ in 0..64 {
            pair.observe(ids.clone(), Some(&old));
        }
        assert!(
            !pair.followup,
            "an identical authenticated wake token cannot rearm foreground"
        );
        let newer = wake::ControlWakeHint::test_token(peer, [1; 16], 3);
        pair.observe(ids.clone(), Some(&newer));
        assert!(
            pair.followup,
            "a genuinely newer publication coalesces one follow-up"
        );
        pair.followup = false;
        for _ in 0..64 {
            pair.observe(ids.clone(), Some(&newer));
        }
        assert!(!pair.followup, "the consumed token cannot repeatedly rearm");
    }

    #[test]
    fn incoming_wake_yields_one_natural_turn_but_protects_replacement_and_resume() {
        let first = ([1; 32], [2; 32]);
        let later = ([3; 32], [4; 32]);
        let natural = control_turn_protected(false, false);
        assert!(
            !natural,
            "a natural publication hint does not reserve a foreground turn"
        );
        assert!(control_wake_preempts(first, natural, Some(later)));
        assert!(!control_wake_preempts(first, natural, Some(first)));
        assert!(!control_wake_preempts(first, natural, None));
        let replacement = control_turn_protected(false, true);
        let resumed = control_turn_protected(true, false);
        for value in 3..=66 {
            let fresh = Some(([value; 32], [value + 1; 32]));
            assert!(
                !control_wake_preempts(first, replacement, fresh),
                "a hint storm cannot interrupt the owned replacement"
            );
            assert!(
                !control_wake_preempts(first, resumed, fresh),
                "a hint storm cannot prevent repayment of the saved comparison"
            );
        }
        let foreground_rank = control_queue_priority(false, true, false, Some(1));
        let resumed_rank = control_queue_priority(false, false, true, None);
        let later_hint = control_queue_priority(false, false, false, Some(0));
        assert!(foreground_rank < resumed_rank && resumed_rank < later_hint);
        assert!(control_queue_priority(true, false, false, None) < foreground_rank);
    }

    #[test]
    fn mandatory_lane_status_never_inherits_a_retired_x25519_identity() {
        let old = crate::identity::device::DeviceIdentity::generate().public();
        let mut current = old.clone();
        current.x25519_pub = crate::identity::device::DeviceIdentity::generate()
            .public()
            .x25519_pub;
        assert_eq!(old.user_id(), current.user_id());
        let mut statuses = HashMap::new();
        for lane in DocumentLane::ALL {
            statuses.insert(
                DocumentPeer::new(&old, lane),
                (SyncPhase::UpToDate, None, false),
            );
        }
        assert_eq!(
            document_sync_status(&statuses, &current),
            (SyncPhase::CatchingUp, None, false),
            "a new full identity has no completed lane despite the same device ID"
        );
        statuses.insert(
            DocumentPeer::new(&old, DocumentLane::Directory),
            (
                SyncPhase::Error,
                Some("retired directory failure".into()),
                true,
            ),
        );
        statuses.insert(
            DocumentPeer::new(&current, DocumentLane::Control),
            (SyncPhase::UpToDate, None, false),
        );
        assert_eq!(
            document_sync_status(&statuses, &current),
            (SyncPhase::CatchingUp, None, false)
        );
        statuses.insert(
            DocumentPeer::new(&current, DocumentLane::Directory),
            (SyncPhase::UpToDate, None, false),
        );
        assert_eq!(
            document_sync_status(&statuses, &current),
            (SyncPhase::CatchingUp, None, false)
        );
        statuses.insert(
            DocumentPeer::new(&current, DocumentLane::Optional),
            (SyncPhase::UpToDate, None, false),
        );
        assert_eq!(
            document_sync_status(&statuses, &current),
            (SyncPhase::UpToDate, None, false)
        );
    }

    #[test]
    fn mandatory_lane_status_requires_both_lanes_and_preserves_independent_errors() {
        let public = crate::identity::device::DeviceIdentity::generate().public();
        let mut statuses = HashMap::new();
        statuses.insert(
            DocumentPeer::new(&public, DocumentLane::Optional),
            (SyncPhase::UpToDate, None, false),
        );
        let control = DocumentPeer::new(&public, DocumentLane::Control);
        let directory = DocumentPeer::new(&public, DocumentLane::Directory);
        statuses.insert(control.clone(), (SyncPhase::UpToDate, None, false));
        assert_eq!(
            document_sync_status(&statuses, &public).0,
            SyncPhase::CatchingUp
        );
        statuses.insert(
            directory.clone(),
            (SyncPhase::Error, Some("directory failure".into()), true),
        );
        statuses.insert(control, (SyncPhase::UpToDate, None, false));
        assert_eq!(
            document_sync_status(&statuses, &public),
            (SyncPhase::Error, Some("directory failure".into()), true)
        );
        statuses.insert(directory, (SyncPhase::UpToDate, None, false));
        assert_eq!(
            document_sync_status(&statuses, &public),
            (SyncPhase::UpToDate, None, false)
        );
    }

    #[tokio::test]
    async fn category_toggle_during_sibling_page_restarts_catch_up_including_off_on_aba() {
        use crate::{discovery::Roster, identity::device::DeviceIdentity};
        use std::sync::Mutex;
        use tokio::{net::TcpListener, sync::mpsc};

        for aba in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let account = Account::generate();
            let open = |name: &str| {
                let (dm, _) = mpsc::unbounded_channel();
                let (channel, _) = mpsc::unbounded_channel();
                let (file, _) = mpsc::unbounded_channel();
                Node::open_with_account(
                    DeviceIdentity::generate(),
                    Account::from_secret_bytes(account.secret_bytes()),
                    Arc::new(Mutex::new(Roster::default())),
                    dm,
                    channel,
                    file,
                    &directory.path().join(format!("{name}-events.log")),
                    &directory.path().join(format!("{name}-sent.log")),
                    "pw",
                )
                .unwrap()
            };
            let local = open("local");
            let remote = open("remote");
            for (owner, sibling) in [(&local, &remote), (&remote, &local)] {
                owner
                    .account_devices
                    .lock()
                    .unwrap()
                    .insert(super::super::account_devices::AccountDevice {
                        public: sibling.identity.public(),
                        certificate: account.certify(&sibling.identity.public().ed25519_pub),
                    })
                    .unwrap();
                owner
                    .set_device_sync_category(SyncCategory::Appearance, true)
                    .unwrap();
            }
            remote
                .set_account_preference(
                    SyncCategory::Appearance,
                    "theme".into(),
                    Some("dark".into()),
                )
                .unwrap();
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            local.roster.lock().unwrap().update(
                &crate::discovery::Announce::new_with_account(
                    &remote.identity,
                    &account,
                    "Remote",
                    address.port(),
                ),
                address.ip(),
                &local.user_id(),
            );
            let (request_seen, first_request) = tokio::sync::oneshot::channel();
            let (release, released) = tokio::sync::oneshot::channel();
            let (second_seen, second_request) = tokio::sync::oneshot::channel();
            let server_remote = remote.clone();
            let server = tokio::spawn(async move {
                let mut first = Some((request_seen, released));
                let mut second_seen = Some(second_seen);
                let mut previous_wanted = None;
                loop {
                    let (mut channel, bytes) =
                        selection_tests::accept_legacy_request_after_policy_retirement(
                            &listener,
                            &server_remote,
                        )
                        .await;
                    let Some(AccountHistoryFrame::Request {
                        account_id,
                        cursor,
                        known_digest,
                        wanted,
                        ..
                    }) = AccountHistoryFrame::decode(&bytes)
                    else {
                        panic!("expected authenticated account-history request");
                    };
                    if let Some((request_seen, released)) = first.take() {
                        assert!(wanted.appearance);
                        request_seen.send(()).unwrap();
                        released.await.unwrap();
                    } else {
                        // A retained wake may add another ordinary pull after
                        // the policy reset. Only the first follow-up and actual
                        // category transitions must restart the archive digest.
                        if second_seen.is_some() || previous_wanted != Some(wanted) {
                            assert_eq!(
                                known_digest, [0; 32],
                                "changed category policy must not reuse an in-flight page digest (ABA={aba})"
                            );
                            assert!(
                                cursor.is_none(),
                                "changed policy restarts at the first page"
                            );
                        }
                        if let Some(second_seen) = second_seen.take() {
                            let _ = second_seen.send(());
                        }
                    }
                    previous_wanted = Some(wanted);
                    server_remote
                        .serve_account_history(
                            &mut channel,
                            AccountSyncVersion::V2,
                            &account_id,
                            cursor,
                            known_digest,
                            wanted,
                        )
                        .await;
                }
            });
            let worker = tokio::spawn(local.clone().run_account_history_loop());
            let outcome = tokio::time::timeout(Duration::from_secs(3), async {
                first_request.await.unwrap();
                local
                    .set_device_sync_category(SyncCategory::Appearance, false)
                    .unwrap();
                if aba {
                    local
                        .set_device_sync_category(SyncCategory::Appearance, true)
                        .unwrap();
                }
                release.send(()).unwrap();
                loop {
                    if local
                        .device_sync_status()
                        .iter()
                        .any(|status| status.phase == SyncPhase::UpToDate)
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                if !aba {
                    assert!(
                        local
                            .account_preferences(SyncCategory::Appearance)
                            .is_empty(),
                        "disabled category must not import the in-flight preference"
                    );
                    local
                        .set_device_sync_category(SyncCategory::Appearance, true)
                        .unwrap();
                }
                loop {
                    if local
                        .account_preferences(SyncCategory::Appearance)
                        .iter()
                        .any(|record| record.value.as_deref() == Some("dark"))
                    {
                        break;
                    }
                    tokio::task::yield_now().await;
                }
                // The ABA case can merge its first page; still require the
                // queued follow-up request to discard the stale page digest.
                second_request
                    .await
                    .expect("server observes an invalidated follow-up request");
            })
            .await;
            // Cancel the fixture receiver before dropping the worker's live
            // channel; teardown EOF is not a legacy-protocol test outcome.
            server.abort();
            worker.abort();
            let _ = worker.await;
            let error = server.await.unwrap_err();
            assert!(error.is_cancelled(), "{error}");
            outcome.expect("re-enabled appearance catches up after an interrupted category page");
        }
    }

    #[test]
    fn history_page_assembler_accepts_only_complete_bounded_records() {
        let own = Account::generate().account_id();
        let mut page = HistoryPageAssembler::new();
        let header = AccountHistoryFrame::ChannelHeader {
            channel: [1; 32],
            from: "ab".repeat(16),
            wall_clock: 1,
            event_id: [2; 32],
            total_len: 3,
        };
        assert!(page.accept(header, &own).unwrap().is_none());
        assert!(
            page.accept(
                AccountHistoryFrame::Complete {
                    next_cursor: None,
                    digest: [3; 32],
                },
                &own,
            )
            .is_err()
        );
        assert!(
            page.accept(AccountHistoryFrame::RecordChunk { bytes: vec![4; 4] }, &own,)
                .is_err()
        );
        assert!(
            page.accept(AccountHistoryFrame::RecordChunk { bytes: vec![4; 3] }, &own,)
                .unwrap()
                .is_none()
        );
        let complete = page
            .accept(
                AccountHistoryFrame::Complete {
                    next_cursor: None,
                    digest: [3; 32],
                },
                &own,
            )
            .unwrap()
            .unwrap();
        assert_eq!(complete.records.len(), 1);
        assert_eq!(complete.digest, [3; 32]);
        assert!(
            matches!(&complete.records[0], HistoryRecord::Channel(record) if record.plaintext == vec![4; 3])
        );
    }

    #[test]
    fn archive_page_uses_exclusive_cursor_and_bounded_payloads() {
        let build = |id: u8| {
            HistoryRecord::Channel(ChannelRecord {
                conversation: [1; 32],
                from: "ab".repeat(16),
                wall_clock: id as u64,
                event_id: [id; 32],
                plaintext: vec![id; MAX_RECORD],
            })
        };
        let mut first = PageBuilder::new(None);
        for id in (1..=40).rev() {
            first.offer(
                HistoryCursor {
                    tag: 1,
                    id: [id; 32],
                },
                || build(id),
            );
            assert!(first.records.len() <= RECORDS_PER_PAGE + 1);
        }
        let (rows, next, digest) = first.finish();
        assert_eq!(rows.len(), RECORDS_PER_PAGE);
        assert_eq!(rows[0].id(), [1; 32]);
        assert_eq!(next.unwrap().id, [16; 32]);

        let mut second = PageBuilder::new(next);
        for id in 1..=40 {
            second.offer(
                HistoryCursor {
                    tag: 1,
                    id: [id; 32],
                },
                || build(id),
            );
        }
        let (rows, next, second_digest) = second.finish();
        assert_eq!(rows.len(), RECORDS_PER_PAGE);
        assert_eq!(rows[0].id(), [17; 32]);
        assert_eq!(next.unwrap().id, [32; 32]);
        assert_eq!(
            digest, second_digest,
            "scan order and cursor must not change the digest"
        );
    }

    #[test]
    fn pending_history_record_rejects_cross_account_and_tampered_headers() {
        let own = Account::generate().account_id();
        let peer = Account::generate().account_id();
        let outsider = Account::generate().account_id();
        let id = [9; 32];
        let plaintext =
            DmEnvelope::new(peer.clone(), own.clone(), id, b"history".to_vec()).encode();
        let valid = PendingRecord {
            kind: 0,
            conversation: *account_conversation_id(&own, &peer).as_bytes(),
            peer_account: None,
            from: peer.clone(),
            wall_clock: 1,
            event_id: id,
            total_len: plaintext.len(),
            plaintext,
        };
        assert!(valid.clone().finish(&own).is_some());
        assert!(valid.clone().finish(&outsider).is_none());
        assert!(
            PendingRecord {
                from: outsider,
                ..valid.clone()
            }
            .finish(&own)
            .is_none()
        );
        assert!(
            PendingRecord {
                event_id: [8; 32],
                ..valid.clone()
            }
            .finish(&own)
            .is_none()
        );
        assert!(
            PendingRecord {
                conversation: [7; 32],
                ..valid.clone()
            }
            .finish(&own)
            .is_none()
        );
        assert!(
            PendingRecord {
                total_len: valid.total_len + 1,
                ..valid
            }
            .finish(&own)
            .is_none()
        );
    }

    #[tokio::test]
    async fn unrelated_account_cannot_request_sibling_history_or_retained_attachment_over_tcp() {
        use crate::{discovery::Roster, identity::device::DeviceIdentity};
        use std::sync::Mutex;
        use tokio::{net::TcpListener, sync::mpsc};

        let directory = tempfile::tempdir().unwrap();
        let open = |name: &str| {
            let (dm, _) = mpsc::unbounded_channel();
            let (channel, _) = mpsc::unbounded_channel();
            let (file, _) = mpsc::unbounded_channel();
            Node::open(
                DeviceIdentity::generate(),
                Arc::new(Mutex::new(Roster::default())),
                dm,
                channel,
                file,
                &directory.path().join(format!("{name}-events.log")),
                &directory.path().join(format!("{name}-sent.log")),
                "pw",
            )
            .unwrap()
        };
        let victim = open("victim");
        let attacker = open("attacker");
        assert_ne!(victim.account_id(), attacker.account_id());
        assert!(
            !victim
                .linked_devices()
                .iter()
                .any(|device| device.public == attacker.identity.public())
        );
        victim
            .set_device_sync_category(SyncCategory::Attachments, true)
            .unwrap();
        victim
            .set_device_sync_category(SyncCategory::Appearance, true)
            .unwrap();
        victim
            .set_account_preference(
                SyncCategory::Appearance,
                "theme".into(),
                Some("private-theme".into()),
            )
            .unwrap();
        let group = victim
            .create_channel("victim archive", vec![])
            .await
            .unwrap();
        victim
            .send_channel_message(group, b"private archived text")
            .await
            .unwrap();
        let contents = b"private retained attachment bytes";
        let source = directory.path().join("private.bin");
        std::fs::write(&source, contents).unwrap();
        let attachment = victim
            .send_file_channel(group, &source, crate::file::FileKind::File)
            .await
            .unwrap();
        let verified = directory.path().join("retained.bin");
        victim
            .save_directory_archive(attachment, &verified)
            .unwrap();
        assert_eq!(
            std::fs::read(verified).unwrap(),
            contents,
            "the requested attachment actually exists and is readable locally"
        );
        assert!(!victim.channel_history(group, 10).is_empty());
        assert!(
            !victim
                .account_preferences(SyncCategory::Appearance)
                .is_empty()
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        struct Server(tokio::task::JoinHandle<()>);
        impl Drop for Server {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let _server = Server(tokio::spawn(Arc::clone(&victim).run_accept_loop(listener)));
        let victim_account = victim.account_id();
        let requests = [
            AccountHistoryFrame::Request {
                version: VERSION,
                account_id: victim_account.clone(),
                cursor: None,
                known_digest: [0; 32],
                wanted: DeviceSyncPolicy::default(),
            },
            AccountHistoryFrame::Request {
                version: VERSION,
                account_id: victim_account.clone(),
                cursor: None,
                known_digest: [0; 32],
                wanted: DeviceSyncPolicy {
                    history: true,
                    attachments: true,
                    profile: true,
                    appearance: true,
                    contacts: true,
                },
            },
            AccountHistoryFrame::AttachmentRequest {
                version: VERSION,
                account_id: victim_account,
                file_conv: *attachment.as_bytes(),
            },
        ];
        let requests = requests.into_iter().flat_map(|request| {
            let mut session_request = request.clone();
            match &mut session_request {
                AccountHistoryFrame::Request { version, .. }
                | AccountHistoryFrame::AttachmentRequest { version, .. } => {
                    *version = AccountSyncVersion::V2.number();
                }
                _ => unreachable!("only history and attachment requests are in this table"),
            }
            let wrapped = AccountHistoryFrame::SessionRequest {
                request_id: [7; 16],
                request: super::super::account_sync_wire::AccountSessionRequest::from_frame(
                    &session_request,
                )
                .unwrap(),
            };
            [request, wrapped]
        });
        for request in requests {
            // Bypass sibling-selection client helpers: the unrelated account
            // completes Noise authentication and directly submits a valid frame.
            let mut channel = attacker
                .privacy_dial(address, &victim.identity.public())
                .await
                .unwrap();
            let version = if matches!(request, AccountHistoryFrame::SessionRequest { .. }) {
                AccountSyncVersion::V2
            } else {
                AccountSyncVersion::V1
            };
            channel
                .send(&request.encode_version(version).unwrap())
                .await
                .unwrap();
            let response = tokio::time::timeout(Duration::from_secs(3), channel.recv())
                .await
                .expect("server promptly closes an unauthorized sibling request");
            assert!(
                response.is_err(),
                "server must disclose no history, device certificates, preferences, or attachment frame to an unrelated account"
            );
        }
    }
}
