//! Desktop search adapts explicit core references and retained-history coverage.
use super::{parse_id, validate_account};
use crate::{
    chat_commands::{HistoryItem, NodeState, SearchHitInfo},
    commands::CommandError,
    runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::{eventlog::event::ConversationId, node::SearchReference};
use serde::{Deserialize, Serialize};

const RESULT_LIMIT: usize = 200;
const QUERY_BYTES: usize = 4096;
const RESPONSE_BYTES: usize = 1024 * 1024;

/// Host tagging stays separate from the core's namespace-specific identity.
#[derive(Clone, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum SearchReferenceInfo {
    Account {
        target: String,
        message_id: Option<String>,
    },
    Channel {
        target: String,
        message_id: Option<String>,
    },
    LegacyDevice {
        target: String,
        message_id: Option<String>,
    },
}

impl SearchReferenceInfo {
    fn to_core(&self) -> Result<SearchReference, CommandError> {
        match self {
            Self::Account { target, message_id } => {
                validate_account(target)?;
                Ok(SearchReference::Account {
                    account_id: target.clone(),
                    message_id: message_id.as_deref().map(parse_id).transpose()?,
                })
            }
            Self::Channel { target, message_id } => {
                let id = parse_id(target)
                    .map_err(|_| CommandError::InvalidInput("malformed channel id".into()))?;
                Ok(SearchReference::Channel {
                    channel_id: ConversationId::new(*id.as_bytes()),
                    message_id: message_id.as_deref().map(parse_id).transpose()?,
                })
            }
            Self::LegacyDevice { target, message_id } => {
                validate_account(target)
                    .map_err(|_| CommandError::InvalidInput("malformed device id".into()))?;
                Ok(SearchReference::LegacyDevice {
                    device_id: target.clone(),
                    message_id: message_id.as_deref().map(parse_id).transpose()?,
                })
            }
        }
    }
}

impl From<SearchReference> for SearchReferenceInfo {
    fn from(reference: SearchReference) -> Self {
        match reference {
            SearchReference::Account {
                account_id,
                message_id,
            } => Self::Account {
                target: account_id,
                message_id: message_id.map(|id| hex::encode(id.as_bytes())),
            },
            SearchReference::Channel {
                channel_id,
                message_id,
            } => Self::Channel {
                target: hex::encode(channel_id.as_bytes()),
                message_id: message_id.map(|id| hex::encode(id.as_bytes())),
            },
            SearchReference::LegacyDevice {
                device_id,
                message_id,
            } => Self::LegacyDevice {
                target: device_id,
                message_id: message_id.map(|id| hex::encode(id.as_bytes())),
            },
        }
    }
}

#[derive(Serialize)]
pub struct ReferencedSearchHitInfo {
    #[serde(flatten)]
    hit: SearchHitInfo,
    reference: SearchReferenceInfo,
    expires_at_ms: Option<u64>,
}

#[derive(Serialize)]
pub struct SearchCoverage {
    scope: &'static str,
    per_conversation_limit: usize,
    result_limit: usize,
    truncated: bool,
}

#[derive(Serialize)]
pub struct RecentSearchInfo {
    hits: Vec<ReferencedSearchHitInfo>,
    coverage: SearchCoverage,
}

#[derive(Serialize)]
pub struct SearchContextInfo {
    status: &'static str,
    reference: SearchReferenceInfo,
    messages: Vec<HistoryItem>,
    newer_entries: Option<usize>,
}

#[tauri::command]
pub async fn owner_search_recent(
    owner: String,
    query: String,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<RecentSearchInfo, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    if query.len() > QUERY_BYTES {
        return Err(CommandError::InvalidInput(
            "search query exceeds byte limit".into(),
        ));
    }
    let response = admission
        .clone()
        .blocking(move |runtime, _| {
            // Keep one extra result to report truncation without retaining all
            // matching plaintext across every conversation in the desktop host.
            let results = runtime
                .handle()
                .search_recent_limited(&query, RESULT_LIMIT + 1)?;
            let truncated = results.hits.len() > RESULT_LIMIT;
            let candidates = results.hits.into_iter().take(RESULT_LIMIT).map(|result| {
                let hit = result.hit;
                ReferencedSearchHitInfo {
                    hit: SearchHitInfo {
                        is_channel: hit.is_channel,
                        account_id: hit.account_id,
                        target: hit.target,
                        label: hit.label,
                        from_me: hit.from_me,
                        who: hit.who,
                        text: String::from_utf8_lossy(&hit.text).into_owned(),
                        wall_clock: hit.wall_clock,
                    },
                    reference: result.reference.into(),
                    expires_at_ms: result.expires_at_ms,
                }
            });
            let mut response = RecentSearchInfo {
                hits: Vec::new(),
                coverage: SearchCoverage {
                    scope: "recent-window",
                    per_conversation_limit: results.per_conversation_limit,
                    result_limit: RESULT_LIMIT,
                    // Use the shorter truncated envelope while selecting a prefix.
                    truncated: true,
                },
            };
            let mut bytes = serialized_bytes(&response)?;
            let mut discarded = truncated;
            for hit in candidates {
                let next = bytes + serialized_bytes(&hit)? + usize::from(!response.hits.is_empty());
                if next > RESPONSE_BYTES {
                    discarded = true;
                    break;
                }
                bytes = next;
                response.hits.push(hit);
            }
            response.coverage.truncated = discarded;
            // With every candidate retained, the final non-truncated envelope
            // can be longer. Measure it too, including all punctuation/escaping.
            if !discarded && serialized_bytes(&response)? > RESPONSE_BYTES {
                response.hits.pop();
                response.coverage.truncated = true;
            }
            Ok(response)
        })
        .await?;
    admission.with_runtime(move |_| Ok(response)).await
}

#[tauri::command]
pub async fn owner_search_context(
    owner: String,
    reference: serde_json::Value,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<SearchContextInfo, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    if reference.get("message_id").is_none() {
        return Err(CommandError::InvalidInput(
            "malformed search reference".into(),
        ));
    }
    let reference: SearchReferenceInfo = serde_json::from_value(reference)
        .map_err(|_| CommandError::InvalidInput("malformed search reference".into()))?;
    let core_reference = reference.to_core()?;
    let response = admission
        .clone()
        .blocking(move |runtime, _| {
            let context = runtime.handle().search_context(&core_reference)?;
            let mut response = SearchContextInfo {
                status: "unavailable",
                reference,
                messages: Vec::new(),
                newer_entries: None,
            };
            if let Some(context) = context {
                response.status = "found";
                response.messages = context
                    .messages
                    .into_iter()
                    .map(HistoryItem::from)
                    .collect();
                response.newer_entries = Some(context.newer_entries);
                if serialized_bytes(&response)? > RESPONSE_BYTES {
                    response.status = "unavailable";
                    response.messages.clear();
                    response.newer_entries = None;
                }
            }
            Ok(response)
        })
        .await?;
    admission.with_runtime(move |_| Ok(response)).await
}

#[derive(Serialize)]
pub struct RevealedMessageInfo {
    text: String,
    visible_for_ms: u64,
}

#[tauri::command]
pub async fn owner_reveal_message(
    owner: String,
    reference: serde_json::Value,
    password: Option<String>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<RevealedMessageInfo, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let reference = protected_reference(reference)?;
    let lock_revision = app.app_lock().authorization_revision()?;
    let admission_lease = admission.lease().clone();
    reveal_message_with_verifier(
        admission,
        app.app_lock().clone(),
        lock_revision,
        reference,
        || {
            crate::app_lock::verify_session_identity(
                &app,
                &admission_lease,
                password,
                "view a protected message in Mesh-Talk",
            )
        },
    )
    .await
}

fn protected_reference(reference: serde_json::Value) -> Result<SearchReference, CommandError> {
    let reference: SearchReferenceInfo = serde_json::from_value(reference)
        .map_err(|_| CommandError::InvalidInput("malformed protected message reference".into()))?;
    match &reference {
        SearchReferenceInfo::Account {
            message_id: Some(_),
            ..
        }
        | SearchReferenceInfo::Channel {
            message_id: Some(_),
            ..
        } => {}
        _ => {
            return Err(CommandError::InvalidInput(
                "an exact account or channel message is required".into(),
            ));
        }
    }
    reference.to_core()
}

#[tauri::command]
pub async fn owner_reveal_image(
    owner: String,
    reference: serde_json::Value,
    password: Option<String>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<tauri::ipc::Response, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let reference = protected_reference(reference)?;
    let lock_revision = app.app_lock().authorization_revision()?;
    let lease = admission.lease().clone();
    reveal_image_with_verifier(
        admission,
        app.app_lock().clone(),
        lock_revision,
        reference,
        || {
            crate::app_lock::verify_session_identity(
                &app,
                &lease,
                password,
                "view a protected image in Mesh-Talk",
            )
        },
    )
    .await
}

pub(super) async fn reveal_image_with_verifier<F, Fut>(
    admission: RuntimeAdmission,
    app_lock: crate::app_lock::AppLock,
    lock_revision: u64,
    reference: SearchReference,
    verify: F,
) -> Result<tauri::ipc::Response, CommandError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), CommandError>>,
{
    reveal_content_with_verifier(
        admission,
        app_lock,
        lock_revision,
        reference,
        ProtectedContentReader {
            protection: mesh_talk_core::node::Node::protected_image_protection,
            content: mesh_talk_core::node::Node::protected_image_content,
        },
        verify,
        tauri::ipc::Response::new,
    )
    .await
}

pub(super) async fn reveal_message_with_verifier<F, Fut>(
    admission: RuntimeAdmission,
    app_lock: crate::app_lock::AppLock,
    lock_revision: u64,
    reference: SearchReference,
    verify: F,
) -> Result<RevealedMessageInfo, CommandError>
where
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), CommandError>>,
{
    reveal_content_with_verifier(
        admission,
        app_lock,
        lock_revision,
        reference,
        ProtectedContentReader {
            protection: |node, reference| {
                node.protected_message_content(reference)
                    .map(|content| content.map(|(mode, _)| mode))
            },
            content: mesh_talk_core::node::Node::protected_message_content,
        },
        verify,
        |text| RevealedMessageInfo {
            text: String::from_utf8_lossy(&text).into_owned(),
            visible_for_ms: 30_000,
        },
    )
    .await
}

type ProtectedContent = (mesh_talk_core::node::MessageProtection, Vec<u8>);

#[derive(Clone, Copy)]
struct ProtectedContentReader {
    protection: fn(
        &mesh_talk_core::node::Node,
        &SearchReference,
    ) -> Result<
        Option<mesh_talk_core::node::MessageProtection>,
        mesh_talk_core::node::NodeError,
    >,
    content: fn(
        &mesh_talk_core::node::Node,
        &SearchReference,
    ) -> Result<Option<ProtectedContent>, mesh_talk_core::node::NodeError>,
}

async fn reveal_content_with_verifier<T, F, Fut>(
    admission: RuntimeAdmission,
    app_lock: crate::app_lock::AppLock,
    lock_revision: u64,
    reference: SearchReference,
    reader: ProtectedContentReader,
    verify: F,
    project: impl FnOnce(Vec<u8>) -> T + Send + 'static,
) -> Result<T, CommandError>
where
    T: Send + 'static,
    F: FnOnce() -> Fut,
    Fut: std::future::Future<Output = Result<(), CommandError>>,
{
    let (protection, original_node) = admission
        .with_runtime(|runtime| {
            let handle = runtime.handle();
            let protection = (reader.protection)(&handle, &reference)?.ok_or_else(|| {
                CommandError::Authorization("protected message unavailable".into())
            })?;
            Ok((protection, handle))
        })
        .await?;
    if protection == mesh_talk_core::node::MessageProtection::Authenticate {
        verify().await?;
    }
    // Authentication can outlive recall, expiry, app lock, logout or replacement.
    // Re-read the exact event only while the original session/runtime is admitted.
    admission
        .blocking(move |runtime, _| {
            let handle = runtime.handle();
            if !std::sync::Arc::ptr_eq(&original_node, &handle) {
                return Err(CommandError::Authorization(
                    "runtime replaced during authentication".into(),
                ));
            }
            let (current_protection, content) =
                (reader.content)(&handle, &reference)?.ok_or_else(|| {
                    CommandError::Authorization("protected message unavailable".into())
                })?;
            if current_protection != protection {
                return Err(CommandError::Authorization(
                    "protected message changed".into(),
                ));
            }
            app_lock.with_unchanged_revision(lock_revision, || Ok(project(content)))
        })
        .await
}

/// Count actual JSON bytes without allocating a second copy of message content.
fn serialized_bytes(value: &impl Serialize) -> Result<usize, CommandError> {
    struct Counter(usize);
    impl std::io::Write for Counter {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut counter = Counter(0);
    serde_json::to_writer(&mut counter, value)
        .map_err(|_| CommandError::Internal("search serialization failed".into()))?;
    Ok(counter.0)
}
