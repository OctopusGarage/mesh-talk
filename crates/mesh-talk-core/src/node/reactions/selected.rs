//! Selected reaction summaries. Preparation and output have independent bounds.
use super::*;
use crate::eventlog::LogError;
use crate::eventlog::sync::network::PreparationBudget;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReactionSummary {
    pub target: String,
    pub emoji: String,
    pub count: u64,
    pub from_me: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ReactionSummaryPage {
    pub items: Vec<ReactionSummary>,
    pub next: Option<String>,
    pub pending: bool,
}

mod sources;
mod store;

/// Serializes selected-query preparation turns so superseded proof work cannot
/// replace a newer decode projection between its source check and commit. The
/// shared checkpoint's causal continuations attach at this same runtime seam.
#[derive(Default)]
pub(in crate::node) struct ReactionQueryWork;

#[derive(serde::Serialize)]
struct Scope {
    conversation: ConversationId,
    peer: Option<String>,
    channel: bool,
}
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct DocumentPosition {
    alias: usize,
    after: Option<crate::node::AccountDocumentKey>,
}
struct Snapshot {
    query: [u8; 32],
    decode: [u8; 32],
    through: i64,
    evidence: sources::Evidence,
}
impl Node {
    /// One turn of a selected-target reaction query. Source pages and output are
    /// bounded; pending pages expose no partial summary. Modern-channel causal
    /// preparation currently delegates to the existing checkpoint query owner.
    pub fn reaction_summaries(
        &self,
        conversation: ConversationId,
        targets: &[EventId],
        cursor: Option<&str>,
    ) -> Result<ReactionSummaryPage, NodeError> {
        let channel = self
            .channels
            .lock()
            .expect("channels mutex")
            .state(&conversation)
            .is_some();
        self.selected_reactions(
            Scope {
                conversation,
                peer: None,
                channel,
            },
            targets,
            cursor,
        )
    }
    pub fn account_reaction_summaries(
        &self,
        peer: &str,
        targets: &[EventId],
        cursor: Option<&str>,
    ) -> Result<ReactionSummaryPage, NodeError> {
        self.selected_reactions(
            Scope {
                conversation: account_conversation_id(&self.account_id(), peer),
                peer: Some(peer.into()),
                channel: false,
            },
            targets,
            cursor,
        )
    }
    fn reaction_snapshot(
        &self,
        store: &store::Store,
        scope: &Scope,
        targets: &[EventId],
    ) -> Result<Snapshot, NodeError> {
        use sha2::{Digest, Sha256};
        let evidence = sources::Evidence::capture(self)?;
        let (epoch, authored) = store.source()?;
        let (through, destructive) = {
            let log = self.log.lock().expect("log mutex");
            let repository = log.repository()?;
            let revisions = if scope.peer.is_some() {
                repository.raw_policy_revisions()?
            } else {
                repository.raw_kind_revisions(scope.conversation)?
            }
            .ok_or(LogError::StaleCursor)?;
            (
                revisions[EventKind::React as usize],
                repository.destructive_policy_revision()?,
            )
        };
        let channel = if scope.channel {
            self.channel_content_generation(scope.conversation)?
        } else {
            None
        };
        let decode: [u8; 32] = Sha256::digest(
            bincode::serialize(&(evidence.stamp, channel))
                .map_err(|e| LogError::Serialization(e.to_string()))?,
        )
        .into();
        let aliases = self
            .file_message_identities
            .lock()
            .expect("identities mutex")
            .revision()?;
        let suppression = self
            .account_suppression
            .lock()
            .expect("suppression mutex")
            .len()?;
        let documents = scope
            .peer
            .as_ref()
            .map(|_| self.selected_reaction_documents_revision());
        let bytes = bincode::serialize(&(
            scope,
            targets,
            through,
            epoch,
            authored,
            decode,
            destructive,
            aliases,
            suppression,
            documents,
        ))
        .map_err(|e| LogError::Serialization(e.to_string()))?;
        Ok(Snapshot {
            query: Sha256::digest(bytes).into(),
            decode,
            through,
            evidence,
        })
    }
    fn visible_reaction_facts(
        &self,
        conversation: ConversationId,
        facts: Vec<store::Fact>,
    ) -> Result<Vec<store::Fact>, LogError> {
        let suppression = self.account_suppression.lock().expect("suppression mutex");
        let mut visible = Vec::new();
        for fact in facts {
            if !suppression.contains(conversation, fact.target)? {
                visible.push(fact);
            }
        }
        Ok(visible)
    }
    fn reaction_target_aliases(
        &self,
        scope: &Scope,
        target: EventId,
    ) -> Result<Vec<EventId>, LogError> {
        let mut aliases = if scope.peer.is_some() {
            self.file_message_identities
                .lock()
                .expect("identities mutex")
                .aliases(scope.conversation, target)?
        } else {
            Vec::new()
        };
        aliases.push(target);
        aliases.sort_unstable();
        aliases.dedup();
        Ok(aliases)
    }
    fn selected_reaction_target(
        &self,
        scope: &Scope,
        targets: &[EventId],
        state: &store::State,
    ) -> Result<Option<(EventId, EventId, DocumentPosition)>, LogError> {
        let Some(logical) = targets.get(state.target) else {
            return Ok(None);
        };
        let aliases = self.reaction_target_aliases(scope, *logical)?;
        let position: DocumentPosition = state
            .document_cursor
            .as_ref()
            .map(|bytes| serde_json::from_slice(bytes).map_err(|_| LogError::StaleCursor))
            .transpose()?
            .unwrap_or_default();
        Ok(aliases
            .get(position.alias)
            .map(|physical| (*logical, *physical, position)))
    }
    fn advance_reaction_target(
        &self,
        store: &store::Store,
        state: &store::State,
        next: Option<i64>,
        position: DocumentPosition,
        facts: Vec<store::Fact>,
        conversation: ConversationId,
        own: &[String; 2],
    ) -> Result<(), LogError> {
        let position = DocumentPosition {
            alias: position.alias + usize::from(next.is_none()),
            after: None,
        };
        let bytes =
            serde_json::to_vec(&position).map_err(|e| LogError::Serialization(e.to_string()))?;
        store.advance(
            state,
            state.phase,
            next.unwrap_or(-1),
            state.target,
            Some(&bytes),
            &self.visible_reaction_facts(conversation, facts)?,
            own,
        )
    }
    fn selected_reactions(
        &self,
        scope: Scope,
        targets: &[EventId],
        cursor: Option<&str>,
    ) -> Result<ReactionSummaryPage, NodeError> {
        if targets.is_empty() || targets.len() > 64 {
            return Err(LogError::InvalidPageBudget.into());
        }
        let _work = self.reaction_queries.lock().expect("reaction query mutex");
        let owner = self
            .log
            .lock()
            .expect("log mutex")
            .repository()?
            .receiving_owner()?;
        let store = store::Store::open(self.storage.clone(), owner)?;
        let mut targets = targets.to_vec();
        if scope.peer.is_some() {
            for target in &mut targets {
                *target = self.file_message_logical_id(scope.conversation, *target)?;
            }
        }
        targets.sort_unstable();
        targets.dedup();
        // A preparation cursor binds parameters and the raw source before a
        // complete kind/control projection exists. It never exposes partial rows.
        let preparation = {
            use sha2::{Digest, Sha256};
            let source = self
                .log
                .lock()
                .expect("log mutex")
                .repository()?
                .source_revision()?;
            let epoch = store.source()?.0;
            format!(
                "p:{}",
                hex::encode(Sha256::digest(
                    bincode::serialize(&(&scope, &targets, source, epoch))
                        .map_err(|e| LogError::Serialization(e.to_string()))?
                ))
            )
        };
        if cursor.is_some_and(|value| value.starts_with("p:") && value != preparation) {
            return Err(LogError::StaleCursor.into());
        }
        let metadata_ready = {
            let log = self.log.lock().expect("log mutex");
            let repository = log.repository()?;
            if repository.raw_policy_revisions()?.is_some() {
                true
            } else {
                if cursor.is_some_and(|value| !value.starts_with("p:")) {
                    return Err(LogError::StaleCursor.into());
                }
                repository.prepare_kind_metadata_step(PreparationBudget {
                    rows: 64,
                    edges: 64,
                    bytes: 8 * 1024 * 1024,
                })?;
                false
            }
        };
        if !metadata_ready {
            return Ok(ReactionSummaryPage {
                items: vec![],
                next: Some(preparation),
                pending: true,
            });
        }
        if scope.channel {
            let ready = self.channel_checkpoints.source_binding(scope.conversation);
            if matches!(ready, Err(LogError::StaleCursor) | Ok(None)) {
                if cursor.is_some_and(|value| !value.starts_with("p:")) {
                    return Err(LogError::StaleCursor.into());
                }
                let log = self.log.lock().expect("log mutex");
                self.channel_checkpoints.project_step(
                    log.repository()?,
                    &self.identity,
                    scope.conversation,
                )?;
                return Ok(ReactionSummaryPage {
                    items: vec![],
                    next: Some(preparation),
                    pending: true,
                });
            }
            ready?;
        }
        let snapshot = self.reaction_snapshot(&store, &scope, &targets)?;
        let cursor = cursor.filter(|value| !value.starts_with("p:"));
        let (state, after) = store.begin(snapshot.query, &targets, cursor)?;
        if state.phase == 5 {
            let page = store.summaries(&state, after)?;
            if self.reaction_snapshot(&store, &scope, &targets)?.query != snapshot.query {
                return Err(LogError::StaleCursor.into());
            }
            return Ok(page);
        }
        let own = [self.user_id(), self.account_id()];
        match state.phase {
            0 => {
                let discovery_scope = scope.peer.is_none().then_some(scope.conversation);
                let (candidates, progress) =
                    store.discovery(discovery_scope, snapshot.decode, snapshot.through)?;
                if let Some(progress) = progress {
                    let mut rows = Vec::new();
                    for candidate in &candidates {
                        if !candidate.cached {
                            rows.push(self.decode_selected_reaction(
                                candidate,
                                scope.channel,
                                &snapshot.evidence,
                            )?);
                        }
                    }
                    store.decoded(discovery_scope, snapshot.decode, progress, &rows)?;
                } else {
                    store.advance(&state, 1, -1, 0, None, &[], &own)?;
                }
            }
            1 => {
                if scope.channel || state.target >= targets.len() {
                    store.advance(&state, 2, -1, 0, None, &[], &own)?;
                } else if let Some((logical, physical, position)) =
                    self.selected_reaction_target(&scope, &targets, &state)?
                {
                    let principal = if scope.peer.is_some() {
                        &own[1]
                    } else {
                        &own[0]
                    };
                    let (facts, next) =
                        store.own_page(&state, scope.conversation, physical, logical, principal)?;
                    self.advance_reaction_target(
                        &store,
                        &state,
                        next,
                        position,
                        facts,
                        scope.conversation,
                        &own,
                    )?;
                } else {
                    store.advance(&state, 1, -1, state.target + 1, None, &[], &own)?;
                }
            }
            2 => {
                if scope.peer.is_some() {
                    store.advance(&state, 3, -1, 0, None, &[], &own)?;
                } else {
                    let page = self
                        .my_dm_reactions
                        .lock()
                        .expect("reactions mutex")
                        .channel_selected_page(
                            scope.conversation,
                            Some(state.id),
                            (state.after >= 0).then_some(state.after),
                            64,
                            8 * 1024 * 1024,
                        )?;
                    let facts = page
                        .rows
                        .into_iter()
                        .map(|(source, _, wall, payload, bound)| store::Fact {
                            target: payload.target,
                            emoji: payload.emoji,
                            principal: snapshot.evidence.principal(
                                &source,
                                bound.map(|ed25519_pub| {
                                    crate::identity::account::AccountPublic { ed25519_pub }
                                        .account_id()
                                }),
                            ),
                            wall,
                            removed: payload.remove,
                        })
                        .collect::<Vec<_>>();
                    store.advance(
                        &state,
                        if page.next.is_some() { 2 } else { 3 },
                        page.next.unwrap_or(-1),
                        0,
                        None,
                        &self.visible_reaction_facts(scope.conversation, facts)?,
                        &own,
                    )?;
                }
            }
            3 => {
                if state.target >= targets.len() {
                    store.advance(&state, 4, -1, 0, None, &[], &own)?;
                } else if let Some((logical, physical, position)) =
                    self.selected_reaction_target(&scope, &targets, &state)?
                {
                    let (rows, next) = store.raw_page(
                        &state,
                        scope.conversation,
                        scope.peer.as_deref(),
                        physical,
                        logical,
                        snapshot.decode,
                        snapshot.through,
                    )?;
                    let mut facts = Vec::new();
                    for row in rows {
                        if scope.channel
                            && !self.channel_content_visible(scope.conversation, row.id)?
                        {
                            continue;
                        }
                        let principal = if scope.peer.is_some() {
                            row.principal.ok_or_else(|| {
                                LogError::CorruptFile(
                                    "account reaction has no authenticated route".into(),
                                )
                            })?
                        } else if scope.channel {
                            snapshot.evidence.principal(
                                &row.source,
                                store.bound_account(row.id, &row.source)?.or(row.principal),
                            )
                        } else {
                            row.source.user_id()
                        };
                        facts.push(store::Fact {
                            target: row.payload.target,
                            emoji: row.payload.emoji,
                            principal,
                            wall: row.wall,
                            removed: row.payload.remove,
                        });
                    }
                    self.advance_reaction_target(
                        &store,
                        &state,
                        next,
                        position,
                        facts,
                        scope.conversation,
                        &own,
                    )?;
                } else {
                    store.advance(&state, 3, -1, state.target + 1, None, &[], &own)?;
                }
            }
            4 => {
                if let Some(peer) = &scope.peer
                    && let Some(target) = targets.get(state.target)
                {
                    let mut aliases = self
                        .file_message_identities
                        .lock()
                        .expect("identities mutex")
                        .aliases(scope.conversation, *target)?;
                    aliases.push(*target);
                    aliases.sort_unstable();
                    aliases.dedup();
                    let position: DocumentPosition = state
                        .document_cursor
                        .as_ref()
                        .map(|bytes| {
                            serde_json::from_slice(bytes).map_err(|_| LogError::StaleCursor)
                        })
                        .transpose()?
                        .unwrap_or_default();
                    if let Some(alias) = aliases.get(position.alias) {
                        let (rows, next) = self.selected_reaction_documents(
                            peer,
                            *alias,
                            position.after.as_ref(),
                        )?;
                        let facts = rows
                            .into_iter()
                            .map(|(principal, wall, payload)| store::Fact {
                                target: *target,
                                emoji: payload.emoji,
                                principal,
                                wall,
                                removed: payload.remove,
                            })
                            .collect::<Vec<_>>();
                        let position = if next.is_some() {
                            DocumentPosition {
                                alias: position.alias,
                                after: next,
                            }
                        } else {
                            DocumentPosition {
                                alias: position.alias + 1,
                                after: None,
                            }
                        };
                        let bytes = serde_json::to_vec(&position)
                            .map_err(|e| LogError::Serialization(e.to_string()))?;
                        store.advance(
                            &state,
                            4,
                            -1,
                            state.target,
                            Some(&bytes),
                            &self.visible_reaction_facts(scope.conversation, facts)?,
                            &own,
                        )?;
                    } else {
                        store.advance(&state, 4, -1, state.target + 1, None, &[], &own)?;
                    }
                } else {
                    store.advance(&state, 5, -1, 0, None, &[], &own)?;
                }
            }
            _ => return Err(LogError::StaleCursor.into()),
        }
        if self.reaction_snapshot(&store, &scope, &targets)?.query != snapshot.query {
            return Err(LogError::StaleCursor.into());
        }
        Ok(ReactionSummaryPage {
            items: vec![],
            next: Some(store::Store::cursor(state.id, 0)),
            pending: true,
        })
    }
}
#[cfg(test)]
mod tests;
