//! One scoped wake and one bounded demand page for the installed WebView owner.
use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::node::{HostReception, ReceptionConsumer, ReceptionCursor, ReceptionLimit};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
use tauri::{Emitter, Manager};
mod gate;
#[cfg(test)]
mod ipc_tests;
use gate::{Gate, Pending};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Scope {
    pub owner: String,
    pub account_id: String,
    pub device_id: String,
    pub runtime_id: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct Cursor {
    epoch: String,
    sequence: String,
}
impl From<ReceptionCursor> for Cursor {
    fn from(value: ReceptionCursor) -> Self {
        Self {
            epoch: hex::encode(value.epoch),
            sequence: value.sequence.to_string(),
        }
    }
}
impl Cursor {
    fn core(&self) -> Result<ReceptionCursor, CommandError> {
        if self.epoch.len() != 32 || self.sequence.len() > 20 {
            return Err(invalid("invalid reception cursor"));
        }
        Ok(ReceptionCursor {
            epoch: hex::decode(&self.epoch)
                .map_err(|_| invalid("invalid reception epoch"))?
                .try_into()
                .map_err(|_| invalid("invalid reception epoch"))?,
            sequence: self
                .sequence
                .parse()
                .map_err(|_| invalid("invalid reception sequence"))?,
        })
    }
}
fn invalid(message: &str) -> CommandError {
    CommandError::InvalidInput(message.into())
}
fn stale() -> CommandError {
    CommandError::Authorization("reception runtime replaced".into())
}

struct Delivery {
    gate: Gate,
    progress: BTreeMap<(String, &'static str), (u64, crate::events::FileProgressEvent)>,
}
#[derive(Clone, Default)]
pub(crate) struct ReceptionHostState(Arc<Mutex<Option<Delivery>>>);
impl ReceptionHostState {
    pub fn install(&self, scope: Scope) {
        *self.0.lock().expect("reception host lock not poisoned") = Some(Delivery {
            gate: Gate::new(scope),
            progress: BTreeMap::new(),
        });
    }
    pub fn scope(&self) -> Option<Scope> {
        self.0
            .lock()
            .ok()?
            .as_ref()
            .map(|state| state.gate.scope.clone())
    }
}
pub(crate) fn invalidate<R: tauri::Runtime>(app: &tauri::AppHandle<R>, scope: &Scope) {
    let Some(state) = app.try_state::<ReceptionHostState>() else {
        return;
    };
    let publish = {
        let mut guard = state.0.lock().expect("reception host lock not poisoned");
        guard
            .as_mut()
            .is_some_and(|state| state.gate.scope == *scope && state.gate.invalidate())
    };
    if publish {
        let success = app.emit("node-reception-wake", scope).is_ok();
        if let Some(state) = state
            .0
            .lock()
            .expect("reception host lock not poisoned")
            .as_mut()
            .filter(|state| state.gate.scope == *scope)
        {
            state.gate.emitted(success);
        }
    }
}
pub(crate) fn invalidate_identity<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    owner: &str,
    account: &str,
    device: &str,
) -> bool {
    let Some(scope) = app
        .try_state::<ReceptionHostState>()
        .and_then(|state| state.scope())
    else {
        return false;
    };
    if scope.owner != owner || scope.account_id != account || scope.device_id != device {
        return true;
    }
    invalidate(app, &scope);
    true
}
pub(crate) fn progress<R: tauri::Runtime>(
    app: &tauri::AppHandle<R>,
    scope: &Scope,
    event: crate::events::FileProgressEvent,
) {
    let Some(state) = app.try_state::<ReceptionHostState>() else {
        return;
    };
    {
        let mut guard = state.0.lock().expect("reception host lock not poisoned");
        let Some(state) = guard.as_mut().filter(|state| state.gate.scope == *scope) else {
            return;
        };
        let key = (event.file_conv.clone(), event.direction);
        if state.progress.len() == 64 && !state.progress.contains_key(&key) {
            // Progress is a current-value hint. Authoritative transfer status is
            // queried on the same wake, including terminals evicted from this cache.
            state.progress.pop_first();
        }
        let generation = state.gate.generation.wrapping_add(1);
        state.progress.insert(key, (generation, event));
    }
    invalidate(app, scope);
}

#[derive(Serialize)]
#[serde(tag = "kind", content = "event", rename_all = "snake_case")]
pub(crate) enum Receipt {
    Dm(crate::events::DmReceivedEvent),
    Channel(crate::events::ChannelMessageEvent),
    File(crate::events::FileReceivedEvent),
    Profile(crate::events::ProfileReceivedEvent),
    Call(crate::events::CallSignalEvent),
    Progress(crate::events::FileProgressEvent),
    State,
}
#[derive(Serialize)]
pub(crate) struct Page {
    scope: Scope,
    token: String,
    after: Cursor,
    next: Cursor,
    through: Cursor,
    complete: bool,
    receipts: Vec<Receipt>,
}
fn require_scope(
    node: &NodeState,
    admission: &RuntimeAdmission,
    runtime: &mesh_talk_core::node::NodeRuntime,
    scope: &Scope,
) -> Result<(), CommandError> {
    if runtime.account_id() != scope.account_id
        || runtime.user_id() != scope.device_id
        || node.installed_runtime_id(admission.lease())?.as_deref() != Some(&scope.runtime_id)
    {
        return Err(stale());
    }
    Ok(())
}

/// Native notifications are best effort: begin at this launch's high-water,
/// coalesce to at most eight observations per second, and never replay a former
/// session's unread backlog as a live alarm. Durable desktop receipt ACKs are
/// independent of this optional presentation consumer.
pub(crate) async fn native_alerts<R: tauri::Runtime>(
    app: tauri::AppHandle<R>,
    authority: crate::chat_commands::RuntimeAuthority,
    node: Arc<mesh_talk_core::node::Node>,
    mut cursor: ReceptionCursor,
) {
    let mut changes = node.subscribe_ui_changes();
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(1));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut first = true;
    loop {
        interval.tick().await;
        if !first && !changes.has_changed().unwrap_or(false) {
            continue;
        }
        first = false;
        changes.borrow_and_update();
        if authority.current(|_| ()).is_err() {
            return;
        }
        // This registered host task owns its synchronous read through completion.
        // No session guard crosses the bounded SQL observation or native dispatch.
        let Ok(head) = node.reception_head() else {
            return;
        };
        if head == cursor {
            continue;
        }
        let page = node.reception_page(
            cursor,
            Some(head),
            ReceptionLimit {
                rows: 8,
                ..Default::default()
            },
        );
        cursor = head;
        let Ok(page) = page else {
            continue;
        };
        let own_account = node.account_id();
        let own_device = node.user_id();
        let receipts: Vec<_> = page.receipts.into_iter().filter(|receipt| {
            !matches!(receipt, HostReception::DirectMessage(dm)
                if dm.from_account.as_deref() == Some(own_account.as_str()) || dm.from == own_device)
        }).collect();
        if receipts.is_empty() {
            continue;
        }
        let owner = authority.lease.owner().to_owned();
        let completion =
            match crate::events::dispatch_runtime_presentation(&app, &authority, move |app| {
                for receipt in receipts {
                    crate::events::notify_reception(app, &owner, receipt);
                }
            }) {
                Ok(completion) => completion,
                Err(error) => {
                    log::warn!("Unable to queue reception notifications: {error}");
                    return;
                }
            };
        // Main-thread congestion cannot accumulate notification payload batches.
        // A cancelled observer does not revoke the queued task's authority guard.
        if completion.wait().await.is_err() {
            return;
        }
    }
}
#[tauri::command]
pub(crate) async fn owner_reception_page(
    scope: Scope,
    after: Option<Cursor>,
    through: Option<Cursor>,
    reset: bool,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
    state: tauri::State<'_, ReceptionHostState>,
) -> Result<Page, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &scope.owner)?;
    let node = node.inner().clone();
    let state = state.inner().clone();
    admission.blocking(move |runtime,admission|{
        require_scope(&node,admission,runtime,&scope)?;
        let mut guard=state.0.lock().map_err(|_|invalid("reception host unavailable"))?;
        let delivery=guard.as_mut().filter(|state|state.gate.scope==scope).ok_or_else(stale)?;
        delivery.gate.begin(reset).map_err(invalid)?;
        let handle=runtime.handle();
        let enrolled=handle.registered_reception(ReceptionConsumer::Desktop)?.is_some();
        let committed=handle.reception_resume(ReceptionConsumer::Desktop)?;
        let after=match after {
            Some(ref cursor) => {
                let cursor = cursor.core()?;
                if cursor.epoch != committed.epoch || cursor.sequence < committed.sequence {
                    return Err(invalid("saved reception cursor is behind the committed consumer"));
                }
                if cursor != committed {
                    handle.acknowledge_reception(ReceptionConsumer::Desktop, committed, cursor)?;
                }
                cursor
            },
            None if !enrolled || committed.sequence==0=>committed,
            None=>return Err(invalid("committed reception state is missing; restore its snapshot before continuing")),
        };
        let page=handle.reception_page(after,through.as_ref().map(Cursor::core).transpose()?,ReceptionLimit::default())?;
        let mut receipts=page.receipts.into_iter().map(|receipt|match receipt {
            HostReception::DirectMessage(dm)=>Receipt::Dm(crate::events::dm_event(dm)),
            HostReception::ChannelMessage(message)=>Receipt::Channel(crate::events::channel_event(message)),
            HostReception::File(file)=>Receipt::File(crate::events::file_event(file)),
        }).collect::<Vec<_>>();
        let mut profiles = delivery.gate.profiles.clone();
        profiles.observe(handle.current_profile_revision()?);
        if !profiles.complete {
            let current = handle.current_profiles_page(profiles.revision, profiles.after.as_deref())?;
            profiles.after = current.after;
            profiles.revision = Some(current.revision);
            profiles.complete = current.complete;
            receipts.extend(current.profiles.into_iter().map(|profile| Receipt::Profile(crate::events::ProfileReceivedEvent {
                account_id: profile.account_id,
                avatar: profile.avatar.map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
            })));
        }
        receipts.extend(handle.take_call_signals(16,256*1024)?.into_iter().map(|signal|Receipt::Call(crate::events::CallSignalEvent{from:signal.from,payload:String::from_utf8_lossy(&signal.payload).into_owned()})));
        receipts.extend(delivery.progress.values().map(|(_,event)|Receipt::Progress(event.clone())));
        receipts.push(Receipt::State);
        let token=format!("{:032x}",rand::random::<u128>());
        let after:Cursor=page.after.into();let next:Cursor=page.next.into();
        let complete=page.complete && profiles.complete;
        delivery.gate.pending=Some(Pending{token:token.clone(),after:after.clone(),next:next.clone(),generation:delivery.gate.generation,profiles});
        Ok(Page{scope:scope.clone(),token,after,next,through:page.through.into(),complete,receipts})
    }).await
}
#[tauri::command]
pub(crate) async fn owner_reception_ack(
    scope: Scope,
    token: String,
    after: Cursor,
    next: Cursor,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
    state: tauri::State<'_, ReceptionHostState>,
) -> Result<bool, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &scope.owner)?;
    let node = node.inner().clone();
    let state = state.inner().clone();
    admission
        .blocking(move |runtime, admission| {
            require_scope(&node, admission, runtime, &scope)?;
            let mut guard = state
                .0
                .lock()
                .map_err(|_| invalid("reception host unavailable"))?;
            let delivery = guard
                .as_mut()
                .filter(|state| state.gate.scope == scope)
                .ok_or_else(stale)?;
            let matches = |pending: &&Pending| {
                pending.token == token && pending.after == after && pending.next == next
            };
            if delivery.gate.pending.is_some()
                && delivery.gate.pending.as_ref().filter(matches).is_none()
                && delivery.gate.last_ack.as_ref().filter(matches).is_some()
            {
                // A delayed duplicate ACK must not clear a later reserved page.
                return Ok(true);
            }
            let pending = delivery
                .gate
                .pending
                .as_ref()
                .filter(matches)
                .or_else(|| delivery.gate.last_ack.as_ref().filter(matches))
                .cloned()
                .ok_or_else(|| invalid("reception page acknowledgement does not match"))?;
            let handle = runtime.handle();
            handle.acknowledge_reception(
                ReceptionConsumer::Desktop,
                after.core()?,
                next.core()?,
            )?;
            let more = handle.reception_head()?.sequence > next.core()?.sequence;
            delivery
                .progress
                .retain(|_, (generation, _)| *generation > pending.generation);
            Ok(delivery.gate.acknowledge(pending, more))
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tauri::Listener;
    fn scope() -> Scope {
        Scope {
            owner: "host".into(),
            account_id: "account".into(),
            device_id: "device".into(),
            runtime_id: "installed".into(),
        }
    }
    #[test]
    fn actual_emitter_coalesces_stalled_renderer_state_and_progress() {
        let app = tauri::test::mock_app();
        let state = ReceptionHostState::default();
        let scope = scope();
        state.install(scope.clone());
        app.manage(state.clone());
        let wakes = Arc::new(AtomicUsize::new(0));
        let observed = wakes.clone();
        app.listen("node-reception-wake", move |_| {
            observed.fetch_add(1, Ordering::Relaxed);
        });
        let old_pushes = Arc::new(AtomicUsize::new(0));
        for event in ["node-state-changed", "file-progress", "channel-changed"] {
            let observed = old_pushes.clone();
            app.listen(event, move |_| {
                observed.fetch_add(1, Ordering::Relaxed);
            });
        }
        for n in 0..10_000 {
            crate::events::emit_node_state_changed(
                app.handle(),
                crate::events::NodeStateChangedEvent {
                    owner: scope.owner.clone(),
                    account_id: scope.account_id.clone(),
                    device_id: scope.device_id.clone(),
                    kind: "state",
                },
            );
            progress(
                app.handle(),
                &scope,
                crate::events::FileProgressEvent {
                    file_conv: format!("file-{}", n % 100),
                    direction: "receive",
                    done: n,
                    total: 10_000,
                },
            );
        }
        assert_eq!(wakes.load(Ordering::Relaxed), 1);
        assert_eq!(old_pushes.load(Ordering::Relaxed), 0);
        assert_eq!(state.0.lock().unwrap().as_ref().unwrap().progress.len(), 64);
        let replacement = Scope {
            runtime_id: "replacement".into(),
            ..scope.clone()
        };
        state.install(replacement.clone());
        progress(
            app.handle(),
            &scope,
            crate::events::FileProgressEvent {
                file_conv: "old-transfer".into(),
                direction: "receive",
                done: 1,
                total: 1,
            },
        );
        assert!(
            state
                .0
                .lock()
                .unwrap()
                .as_ref()
                .unwrap()
                .progress
                .is_empty()
        );
        invalidate(app.handle(), &replacement);
        assert_eq!(wakes.load(Ordering::Relaxed), 2);
    }
    #[test]
    fn cursor_preserves_u64_precision_and_rejects_invalid_widths() {
        let core = ReceptionCursor {
            epoch: [255; 16],
            sequence: u64::MAX,
        };
        let cursor: Cursor = core.into();
        assert_eq!(cursor.core().unwrap(), core);
        assert!(
            Cursor {
                sequence: "18446744073709551616".into(),
                ..cursor.clone()
            }
            .core()
            .is_err()
        );
        assert!(
            Cursor {
                epoch: "00".repeat(17),
                ..cursor
            }
            .core()
            .is_err()
        );
    }
}
