//! Desktop-owned temporary disclosure of one exact message over LAN HTTP.
use crate::{
    chat_commands::NodeState,
    commands::CommandError,
    runtime_admission::RuntimeAdmission,
    state::{AppState, SessionLease},
};
use mesh_talk_core::{
    discovery::LanIpv4Scope,
    node::{FavoriteSource, LanReviewText, Node},
};
use std::{
    sync::{Arc, Mutex, Weak},
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{OwnedSemaphorePermit, Semaphore, watch},
    time::{Instant, timeout},
};

#[cfg(test)]
type BlockingGate =
    Arc<Mutex<Option<(std::sync::mpsc::Sender<()>, std::sync::mpsc::Receiver<()>)>>>;

#[cfg(test)]
fn hold_test_work(
    gate: &BlockingGate,
) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
    let (entered, observe) = std::sync::mpsc::channel();
    let (release, resume) = std::sync::mpsc::channel();
    *gate.lock().unwrap() = Some((entered, resume));
    (observe, release)
}

#[cfg(test)]
fn wait_for_test_projection(gate: &BlockingGate) {
    let pending = gate.lock().unwrap().take();
    if let Some((entered, resume)) = pending {
        let _ = entered.send(());
        let _ = resume.recv_timeout(Duration::from_secs(5));
    }
}

#[derive(Clone)]
pub(crate) struct LanReview {
    active: Arc<Mutex<Option<PendingReview>>>,
    preview: Arc<Semaphore>,
    #[cfg(test)]
    projection_gate: BlockingGate,
    #[cfg(test)]
    interface_gate: BlockingGate,
}

impl Default for LanReview {
    fn default() -> Self {
        Self {
            active: Arc::default(),
            preview: Arc::new(Semaphore::new(1)),
            #[cfg(test)]
            projection_gate: Arc::default(),
            #[cfg(test)]
            interface_gate: Arc::default(),
        }
    }
}

#[derive(Clone)]
enum InspectionSlot {
    Preview {
        _permit: Arc<OwnedSemaphorePermit>,
        #[cfg(test)]
        projection_gate: BlockingGate,
    },
    Startup(Arc<Reservation>),
}

impl InspectionSlot {
    fn reservation(&self) -> Option<&Reservation> {
        match self {
            Self::Preview { .. } => None,
            Self::Startup(reservation) => Some(reservation),
        }
    }

    #[cfg(test)]
    fn projection_gate(&self) -> &BlockingGate {
        match self {
            Self::Preview {
                projection_gate, ..
            } => projection_gate,
            Self::Startup(reservation) => &reservation.state.projection_gate,
        }
    }
}

struct PendingReview {
    owner: SessionLease,
    id: String,
    stop: watch::Sender<bool>,
}

struct Reservation {
    state: LanReview,
    id: String,
    stop: watch::Sender<bool>,
}

struct StopOnDrop(Option<watch::Sender<bool>>);

impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if let Some(stop) = self.0.take() {
            stop.send_replace(true);
        }
    }
}

impl Reservation {
    async fn wait_for_reads(self: Arc<Self>, reads: &Semaphore) {
        // Releasing a request future does not free its blocking-work slot.
        // Keep the grant reserved until the real worker gives up its permit.
        let _finished = reads.acquire().await;
    }
}

impl Drop for Reservation {
    fn drop(&mut self) {
        self.stop.send_replace(true);
        *self.state.active.lock().expect("LAN review state") = None;
    }
}

impl LanReview {
    #[cfg(test)]
    pub(crate) fn hold_next_projection(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        hold_test_work(&self.projection_gate)
    }

    #[cfg(test)]
    pub(crate) fn hold_next_interface_scan(
        &self,
    ) -> (std::sync::mpsc::Receiver<()>, std::sync::mpsc::Sender<()>) {
        hold_test_work(&self.interface_gate)
    }

    #[cfg(test)]
    pub(crate) fn has_reservation(&self) -> bool {
        self.active.lock().unwrap().is_some()
    }

    fn reserve(&self, owner: SessionLease) -> Result<Reservation, CommandError> {
        let mut active = self.active.lock().expect("LAN review state");
        if active.is_some() {
            return Err(invalid("Stop the existing LAN review first"));
        }
        let (stop, _) = watch::channel(false);
        let id = hex::encode(rand::random::<[u8; 16]>());
        *active = Some(PendingReview {
            owner,
            id: id.clone(),
            stop: stop.clone(),
        });
        Ok(Reservation {
            state: self.clone(),
            id,
            stop,
        })
    }

    pub(crate) fn stop(&self, lease: &SessionLease) {
        self.stop_matching(lease, None);
    }

    fn stop_matching(&self, lease: &SessionLease, id: Option<&str>) {
        if let Some(review) = self
            .active
            .lock()
            .expect("LAN review state")
            .as_ref()
            .filter(|review| &review.owner == lease && id.is_none_or(|id| id == review.id))
        {
            review.stop.send_replace(true);
        }
    }
}

#[derive(serde::Serialize)]
pub struct LanReviewLink {
    id: String,
    url: String,
}

fn invalid(detail: &str) -> CommandError {
    CommandError::Service(detail.into())
}

async fn observe_interface(scope: LanIpv4Scope, reservation: Arc<Reservation>) -> bool {
    let observation = tokio::task::spawn_blocking(move || {
        #[cfg(test)]
        wait_for_test_projection(&reservation.state.interface_gate);
        let _reservation = reservation;
        scope.is_current()
    });
    matches!(
        timeout(Duration::from_secs(2), observation).await,
        Ok(Ok(true))
    )
}

struct SourcePreview {
    node: Weak<Node>,
    changes: watch::Receiver<u64>,
    network: watch::Receiver<u64>,
    message: LanReviewText,
    epoch: u64,
}

async fn inspect_source(
    admission: RuntimeAdmission,
    node: &NodeState,
    revision: u64,
    source: FavoriteSource,
    slot: InspectionSlot,
    deadline: Instant,
) -> Result<SourcePreview, CommandError> {
    let deadline = deadline.min(Instant::now() + Duration::from_secs(2));
    let mut stopping = slot.reservation().map(|owner| owner.stop.subscribe());
    let wait_for_stop = async {
        if let Some(stopping) = stopping.as_mut() {
            let _ = stopping.wait_for(|stopped| *stopped).await;
        } else {
            std::future::pending::<()>().await;
        }
    };
    let inspect = async {
        loop {
            let installed = node.0.clone().read_owned().await;
            let runtime = installed.as_ref().ok_or_else(CommandError::not_started)?;
            let owner = runtime.blocking_work();
            let source = source.clone();
            let slot = slot.clone();
            let host = node.clone();
            let authority = admission.clone();
            let mut work = tokio::task::JoinSet::new();
            if !owner.spawn(&mut work, move || {
                let runtime = installed.as_ref().ok_or_else(CommandError::not_started)?;
                let check_deadline = || {
                    if Instant::now() >= deadline {
                        return Err(invalid("LAN review source read timed out"));
                    }
                    Ok(())
                };
                #[cfg(test)]
                wait_for_test_projection(slot.projection_gate());
                authority.checked_at_lock_revision(runtime, revision, check_deadline)?;
                let core = runtime.handle();
                let changes = core.subscribe_lan_review_source_changes();
                let network = core.subscribe_network_changes();
                let epoch = core.lan_review_source_epoch();
                drop(installed);
                if slot.reservation().is_some_and(|owner| *owner.stop.borrow()) {
                    return Err(invalid("LAN review was stopped before publication"));
                }
                check_deadline()?;
                let _slot = slot;
                let message = core.inspect_lan_review_text(&source)?;
                let Ok(installed) = host.0.try_read() else {
                    return Ok(None);
                };
                let runtime = installed.as_ref().ok_or_else(CommandError::not_started)?;
                authority.checked_at_lock_revision(runtime, revision, || {
                    check_deadline()?;
                    if !Arc::ptr_eq(&runtime.handle(), &core) {
                        return Err(invalid("LAN review runtime changed"));
                    }
                    Ok(core
                        .with_lan_review_source(epoch, || ())
                        .map(|()| SourcePreview {
                            node: Arc::downgrade(&core),
                            changes,
                            network,
                            message,
                            epoch,
                        }))
                })
            }) {
                return Err(invalid("LAN review runtime has retired"));
            }
            let approved = work
                .join_next()
                .await
                .ok_or_else(|| invalid("LAN review source worker not admitted"))?
                .map_err(|_| invalid("LAN review source worker stopped"))??;
            if let Some(approved) = approved {
                return Ok(approved);
            }
            // First projection can warm indexes; retry within the existing
            // command deadline after releasing desktop and storage guards.
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    };
    tokio::select! {
        biased;
        _ = wait_for_stop => Err(invalid("LAN review was stopped before publication")),
        result = tokio::time::timeout_at(deadline, inspect) => {
            result.map_err(|_| invalid("LAN review source read timed out"))?
        }
    }
}

#[tauri::command]
pub async fn owner_preview_lan_review(
    owner: String,
    source: FavoriteSource,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<String, CommandError> {
    let permit = app
        .lan_review
        .preview
        .clone()
        .try_acquire_owned()
        .map_err(|_| invalid("A LAN review preview is already running"))?;
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let revision = app.app_lock().authorization_revision()?;
    let slot = InspectionSlot::Preview {
        _permit: Arc::new(permit),
        #[cfg(test)]
        projection_gate: app.lan_review.projection_gate.clone(),
    };
    let deadline = Instant::now() + Duration::from_secs(2);
    Ok(
        inspect_source(admission, &node, revision, source, slot, deadline)
            .await?
            .message
            .text,
    )
}

#[tauri::command]
pub async fn owner_start_lan_review(
    owner: String,
    source: FavoriteSource,
    preview: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<LanReviewLink, CommandError> {
    if preview.is_empty() || preview.len() > 16 * 1024 {
        return Err(invalid("Invalid LAN review preview"));
    }
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let revision = app.app_lock().authorization_revision()?;
    let reservation = Arc::new(app.lan_review.reserve(admission.lease().clone())?);
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut abort_start = StopOnDrop(Some(reservation.stop.clone()));
    let startup = async {
        let id = reservation.id.clone();
        let observing = reservation.clone();
        let scope = timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || {
                let _observing = observing;
                LanIpv4Scope::current()
            }),
        )
        .await
        .map_err(|_| invalid("LAN interface observation timed out"))?
        .map_err(|_| invalid("LAN interface observation stopped"))?
        .ok_or_else(|| invalid("LAN review needs one active physical IPv4 interface"))?;
        let approved = inspect_source(
            admission.clone(),
            &node,
            revision,
            source.clone(),
            InspectionSlot::Startup(reservation.clone()),
            deadline,
        )
        .await?;
        if approved.message.text != preview {
            return Err(invalid("LAN review source changed after confirmation"));
        }
        let observed_network = approved.network;
        if !observe_interface(scope.clone(), reservation.clone()).await {
            return Err(invalid("LAN review interface changed during startup"));
        }
        let listener = TcpListener::bind((scope.address(), 0))
            .await
            .map_err(|_| invalid("Could not listen on the LAN address"))?;
        let address = listener
            .local_addr()
            .map_err(|_| invalid("Missing LAN address"))?;
        let path = format!("/{}", hex::encode(rand::random::<[u8; 24]>()));
        let url = format!("http://{address}{path}");
        let node = node.inner().clone();
        let mut stopping = reservation.stop.subscribe();
        let mut guard = tokio::select! {
            biased;
            _ = stopping.wait_for(|stopped| *stopped) => {
                return Err(invalid("LAN review was stopped before publication"));
            }
            guard = timeout(Duration::from_secs(2), node.0.clone().write_owned()) => {
                guard.map_err(|_| invalid("LAN review runtime is busy"))?
            }
        };
        let runtime = guard.as_mut().ok_or_else(CommandError::not_started)?;
        let (runtime_stop, mut runtime_ended) = tokio::sync::oneshot::channel();
        let (activate, activated) = tokio::sync::oneshot::channel();
        let mut content_changes = approved.changes.clone();
        let mut network_changes = observed_network.clone();
        let task_admission = admission.clone();
        let task_scope = scope.clone();
        let task_stopping = stopping.clone();
        let closing = StopOnDrop(Some(reservation.stop.clone()));
        let publishing_reservation = reservation.clone();
        let task = tokio::spawn(async move {
            let _closing = closing;
            let mut pending_stop = task_stopping.clone();
            let activated = tokio::select! {
                biased;
                _ = pending_stop.wait_for(|stopped| *stopped) => return,
                result = activated => result,
            };
            let Ok(deadline) = activated else {
                return;
            };
            let grant = Arc::new(Grant {
                admission: task_admission,
                node,
                source,
                preview,
                scope: task_scope,
                path,
                revision,
                deadline,
                stopping: task_stopping,
                reads: Arc::new(Semaphore::new(1)),
                _reservation: reservation.clone(),
                #[cfg(test)]
                projection_gate: reservation.state.projection_gate.clone(),
            });
            let authorization = grant.clone();
            let server = crate::lan_review_http::serve(
                listener,
                grant.stopping.clone(),
                move |peer, path, request_deadline| {
                    let grant = authorization.clone();
                    async move {
                        if path != grant.path || !grant.scope.admits(peer) {
                            return None;
                        }
                        match grant.read(request_deadline, true).await {
                            GrantRead::Allowed(text) => Some(text),
                            GrantRead::Busy | GrantRead::Invalid => None,
                        }
                    }
                },
            );
            tokio::pin!(server);
            let source_changed = async {
                while content_changes.changed().await.is_ok() {
                    if grant.invalidated().await {
                        break;
                    }
                }
            };
            let network_changed = async {
                while network_changes.changed().await.is_ok() {
                    if !observe_interface(grant.scope.clone(), reservation.clone()).await {
                        break;
                    }
                }
            };
            let serve_result = tokio::select! {
                result = &mut server => Some(result),
                _ = &mut runtime_ended => None,
                _ = tokio::time::sleep_until(deadline) => None,
                _ = source_changed => None,
                _ = network_changed => None,
            };
            reservation.stop.send_replace(true);
            if serve_result.is_none() {
                let _ = server.await;
            }
            reservation.wait_for_reads(&grant.reads).await;
        });
        runtime.register_graceful_host_task(runtime_stop, task);
        tokio::task::spawn_blocking(move || {
            let _reservation = publishing_reservation;
            let runtime = guard.as_ref().ok_or_else(CommandError::not_started)?;
            admission.checked_at_lock_revision(runtime, revision, || {
                let stop = stopping.borrow();
                let core = runtime.handle();
                if *stop
                    || Instant::now() >= deadline
                    || !Weak::ptr_eq(&Arc::downgrade(&core), &approved.node)
                {
                    return Err(invalid("LAN review authority, source or interface changed"));
                }
                core.with_lan_review_source(approved.epoch, || {
                    if Instant::now() >= deadline || observed_network.has_changed().unwrap_or(true)
                    {
                        return Err(invalid("LAN review interface changed before publication"));
                    }
                    let monotonic = Instant::now();
                    let now = Duration::from_millis(core.expiry_now());
                    let duration =
                        approved
                            .message
                            .expires_at_ms
                            .map_or(Duration::from_secs(900), |end| {
                                Duration::from_millis(end)
                                    .saturating_sub(now)
                                    .min(Duration::from_secs(900))
                            });
                    if duration.is_zero() {
                        return Err(invalid("LAN review source expired"));
                    }
                    let deadline = approved
                        .message
                        .recipient_expires_at
                        .map_or(monotonic + duration, |received| {
                            (monotonic + duration).min(Instant::from_std(received))
                        });
                    if Instant::now() >= deadline {
                        return Err(invalid("LAN review source expired"));
                    }
                    activate
                        .send(deadline)
                        .map_err(|_| invalid("LAN review listener stopped before activation"))?;
                    Ok(LanReviewLink { id, url })
                })
                .ok_or_else(|| invalid("LAN review source changed before publication"))?
            })
        })
        .await
        .map_err(|_| invalid("LAN review publication stopped"))?
    };
    let result = tokio::time::timeout_at(deadline, startup)
        .await
        .map_err(|_| invalid("LAN review startup timed out"))?;
    if result.is_ok() {
        abort_start.0 = None;
    }
    result
}

struct Grant {
    admission: RuntimeAdmission,
    node: NodeState,
    source: FavoriteSource,
    preview: String,
    scope: LanIpv4Scope,
    path: String,
    revision: u64,
    deadline: Instant,
    stopping: watch::Receiver<bool>,
    reads: Arc<Semaphore>,
    _reservation: Arc<Reservation>,
    #[cfg(test)]
    projection_gate: BlockingGate,
}

enum GrantRead {
    Allowed(String),
    Busy,
    Invalid,
}

impl Grant {
    fn stopped(&self) -> bool {
        *self.stopping.borrow() || Instant::now() >= self.deadline
    }

    async fn invalidated(self: &Arc<Self>) -> bool {
        loop {
            let deadline = Instant::now() + Duration::from_secs(2);
            match tokio::time::timeout_at(deadline, self.read(deadline, false)).await {
                Ok(GrantRead::Allowed(_)) => return false,
                Ok(GrantRead::Invalid) if Instant::now() < deadline => return true,
                Ok(GrantRead::Busy | GrantRead::Invalid) | Err(_) => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }

    async fn read(self: &Arc<Self>, request_deadline: Instant, _guest_request: bool) -> GrantRead {
        let deadline = self.deadline.min(request_deadline);
        if self.stopped() || Instant::now() >= deadline {
            return GrantRead::Invalid;
        }
        let Ok(Ok(permit)) =
            timeout(Duration::from_secs(2), self.reads.clone().acquire_owned()).await
        else {
            return GrantRead::Busy;
        };
        let Ok(installed) = timeout(Duration::from_secs(2), self.node.0.clone().read_owned()).await
        else {
            return GrantRead::Busy;
        };
        let Some(runtime) = installed.as_ref() else {
            return GrantRead::Invalid;
        };
        let work = runtime.blocking_work();
        let grant = self.clone();
        let mut projection = tokio::task::JoinSet::new();
        if !work.spawn(&mut projection, move || {
            let _permit = permit;
            // Session and app-lock checks can contend with other desktop work.
            // Both checks and storage I/O run off the HTTP deadline poll.
            let Some(runtime) = installed.as_ref() else {
                return GrantRead::Invalid;
            };
            let core = runtime.handle();
            let observed_network = core.subscribe_network_changes();
            let epoch = core.lan_review_source_epoch();
            let permitted = grant
                .admission
                .checked_at_lock_revision(runtime, grant.revision, || {
                    Ok(!grant.stopped() && Instant::now() < deadline)
                })
                .unwrap_or(false);
            drop(installed);
            if !permitted {
                return GrantRead::Invalid;
            }

            let projected = core.inspect_lan_review_text(&grant.source);
            #[cfg(test)]
            if _guest_request {
                wait_for_test_projection(&grant.projection_gate);
            }
            if grant.stopped() || Instant::now() >= deadline || !grant.scope.is_current() {
                return GrantRead::Invalid;
            }
            let Ok(installed) = grant.node.0.try_read() else {
                return GrantRead::Busy;
            };
            let Some(runtime) = installed.as_ref() else {
                return GrantRead::Invalid;
            };
            grant
                .admission
                .checked_at_lock_revision(runtime, grant.revision, || {
                    let stopping = grant.stopping.borrow();
                    if *stopping || !Arc::ptr_eq(&runtime.handle(), &core) {
                        return Ok(GrantRead::Invalid);
                    }
                    Ok(core
                        .with_lan_review_source(epoch, || {
                            if Instant::now() >= deadline
                                || observed_network.has_changed().unwrap_or(true)
                            {
                                return GrantRead::Invalid;
                            }
                            match projected {
                                Ok(message)
                                    if message.text == grant.preview
                                        && message
                                            .expires_at_ms
                                            .is_none_or(|end| end > core.expiry_now())
                                        && message.recipient_expires_at.is_none_or(|end| {
                                            Instant::now() < Instant::from_std(end)
                                        }) =>
                                {
                                    GrantRead::Allowed(message.text)
                                }
                                _ => GrantRead::Invalid,
                            }
                        })
                        .unwrap_or(GrantRead::Busy))
                })
                .unwrap_or(GrantRead::Invalid)
        }) {
            return GrantRead::Invalid;
        }
        projection
            .join_next()
            .await
            .and_then(Result::ok)
            .unwrap_or(GrantRead::Invalid)
    }
}

#[tauri::command]
pub fn owner_revoke_lan_review(
    owner: String,
    grant_id: Option<String>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<(), CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    app.session()
        .matching(admission.lease(), |_| {
            app.lan_review
                .stop_matching(admission.lease(), grant_id.as_deref())
        })
        .map_err(CommandError::Authorization)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{services::user::User, state::SessionState};

    fn fixture_user() -> User {
        User {
            user_id: "alice".into(),
            account_login_name: None,
            name: "alice".into(),
            display_name: "alice".into(),
            address: "fixture".into(),
            created_at: 0,
            last_seen: 0,
            is_online: true,
        }
    }

    #[tokio::test]
    async fn withdrawn_share_retains_its_slot_until_blocking_read_finishes() {
        let session = SessionState::default();
        let lease = session.publish("fixture".into(), fixture_user(), "pw".into());
        let owner = LanReview::default();
        let pending = owner.reserve(lease.clone()).unwrap();
        let reads = Arc::new(Semaphore::new(1));
        let permit = reads.clone().acquire_owned().await.unwrap();
        let (release, wait) = std::sync::mpsc::channel();
        let blocking = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            wait.recv().unwrap();
        });

        owner.stop(&lease);
        let drain = Arc::new(pending).wait_for_reads(&reads);
        tokio::pin!(drain);
        assert!(
            std::future::poll_fn(|context| {
                use std::future::Future;
                std::task::Poll::Ready(drain.as_mut().poll(context).is_pending())
            })
            .await
        );
        assert!(owner.reserve(lease.clone()).is_err());
        release.send(()).unwrap();
        blocking.await.unwrap();
        drain.await;
        assert!(owner.reserve(lease).is_ok());
    }

    #[tokio::test]
    #[ignore = "observes a physical LAN interface for native acceptance"]
    async fn timed_out_interface_observation_retains_its_reservation() {
        let session = SessionState::default();
        let lease = session.publish("fixture".into(), fixture_user(), "pw".into());
        let review = LanReview::default();
        let reservation = Arc::new(review.reserve(lease.clone()).unwrap());
        let scope = LanIpv4Scope::current().expect("one physical LAN interface");
        let (entered, started) = std::sync::mpsc::channel();
        let (release, resume) = std::sync::mpsc::channel();
        *review.interface_gate.lock().unwrap() = Some((entered, resume));
        let observation = tokio::spawn(observe_interface(scope, reservation.clone()));
        tokio::task::spawn_blocking(move || started.recv_timeout(Duration::from_secs(1)))
            .await
            .unwrap()
            .unwrap();
        review.stop(&lease);
        drop(reservation);
        assert!(!observation.await.unwrap());
        assert!(review.reserve(lease.clone()).is_err());

        release.send(()).unwrap();
        timeout(Duration::from_secs(1), async {
            while review.reserve(lease.clone()).is_err() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("the actual observer must release the slot");
    }

    #[test]
    fn revoke_cancels_unpublished_reservation_and_allows_fresh_start() {
        let session = SessionState::default();
        let user = fixture_user();
        let lease = session.publish("fixture-first".into(), user.clone(), "pw".into());
        let owner = LanReview::default();
        let pending = owner.reserve(lease.clone()).unwrap();
        let stopping = pending.stop.subscribe();

        let replacement = session.publish("fixture-next".into(), user, "pw".into());
        owner.stop(&replacement);
        assert!(!*stopping.borrow());
        owner.stop(&lease);
        assert!(*stopping.borrow());
        assert!(owner.reserve(replacement.clone()).is_err());
        drop(pending);

        let fresh = owner.reserve(replacement).unwrap();
        assert!(!*fresh.stop.borrow());
    }
}
