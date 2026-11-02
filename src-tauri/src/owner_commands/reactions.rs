//! Selected reaction summaries use the same runtime admission as visible history.
use super::{parse_id, validate_account};
use crate::{
    chat_commands::NodeState, commands::CommandError, runtime_admission::RuntimeAdmission,
    state::AppState,
};
use mesh_talk_core::{
    eventlog::{ConversationId, EventId},
    node::ReactionSummaryPage,
};
use serde::Deserialize;

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReactionScopeKind {
    Account,
    Channel,
}

fn selected_targets(
    targets: &[String],
    cursor: Option<&str>,
) -> Result<Vec<EventId>, CommandError> {
    if targets.len() > 64 || cursor.is_some_and(|value| value.len() > 4096) {
        return Err(CommandError::InvalidInput(
            "reaction request exceeds page limits".into(),
        ));
    }
    targets
        .iter()
        .map(|target| {
            if target.len() != 64 {
                return Err(CommandError::InvalidInput(
                    "malformed reaction target".into(),
                ));
            }
            parse_id(target)
        })
        .collect()
}

#[tauri::command]
pub async fn owner_reaction_summaries(
    owner: String,
    kind: ReactionScopeKind,
    target: String,
    targets: Vec<String>,
    cursor: Option<String>,
    app: tauri::State<'_, AppState>,
    node: tauri::State<'_, NodeState>,
) -> Result<ReactionSummaryPage, CommandError> {
    let admission = RuntimeAdmission::capture(&app, &node, &owner)?;
    let targets = selected_targets(&targets, cursor.as_deref())?;
    let channel = match kind {
        ReactionScopeKind::Account => {
            validate_account(&target)?;
            None
        }
        ReactionScopeKind::Channel => {
            if target.len() != 64 {
                return Err(CommandError::InvalidInput("malformed channel id".into()));
            }
            Some(ConversationId::new(*parse_id(&target)?.as_bytes()))
        }
    };
    let page = admission
        .clone()
        .blocking(move |runtime, _| {
            Ok(match channel {
                Some(channel) => {
                    runtime.reaction_summaries(channel, &targets, cursor.as_deref())?
                }
                None => runtime.account_reaction_summaries(&target, &targets, cursor.as_deref())?,
            })
        })
        .await?;
    admission.with_runtime(move |_| Ok(page)).await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn reaction_request_admits_only_bounded_exact_targets_and_continuation() {
        let target = "01".repeat(32);
        assert_eq!(
            selected_targets(&vec![target.clone(); 64], Some("p:continuation"))
                .unwrap()
                .len(),
            64
        );
        assert!(selected_targets(&vec![target.clone(); 65], None).is_err());
        assert!(selected_targets(&["x".repeat(1_000_000)], None).is_err());
        assert!(selected_targets(&[target], Some(&"x".repeat(4097))).is_err());
    }
}
