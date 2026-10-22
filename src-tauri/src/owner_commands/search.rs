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
                .search_recent_limited(&query, RESULT_LIMIT + 1);
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
            let context = runtime.handle().search_context(&core_reference);
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
