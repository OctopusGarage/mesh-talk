//! Explicit File work shares the existing account attachment worker budget.
use super::{file_preparation::GroupFilePayload, *};
use crate::eventlog::Event;
use std::sync::Arc;

#[derive(Clone)]
pub(in crate::node) struct AccountGroupFileWork {
    pub peer_id: String,
    pub id: AccountGroupOperationId,
    pub file: ConversationId,
    pub size: u64,
    reference: journal::OperationReference,
    payload: GroupFilePayload,
    committed: Option<Event>,
}

impl Node {
    /// Project one bounded journal page after releasing its lock. No independent scan.
    pub(super) fn account_group_file_work(
        &self,
        candidates: Vec<journal::Entry>,
    ) -> Result<Vec<AccountGroupFileWork>, crate::eventlog::LogError> {
        let (pending, copies): (Vec<_>, Vec<_>) =
            candidates.into_iter().partition(|entry| !entry.terminal());
        let mut work = Vec::new();
        for entry in pending {
            let Some(AccountGroupOperation::Send { message }) = entry.body else {
                continue;
            };
            let Some(payload) = GroupFilePayload::decode(message.encoded_body()) else {
                continue;
            };
            if self.account_group_file_assets_ready(&entry.reference, &payload)? {
                continue;
            }
            let Some(descriptor) = payload
                .descriptor
                .as_ref()
                .filter(|descriptor| descriptor.valid_for(&entry.reference, &payload.logical))
            else {
                continue;
            };
            let Some(manifest) = crate::node::files::validated_manifest(&descriptor.manifest)
            else {
                continue;
            };
            work.push(AccountGroupFileWork {
                peer_id: entry.reference.origin.public().user_id(),
                id: entry.reference.id,
                file: manifest.file_conv(),
                size: payload.logical.size,
                reference: entry.reference,
                payload,
                committed: None,
            });
        }
        // The original result descriptor lives with the immutable terminal
        // intent. A rotating signed source head cannot erase unfinished work,
        // and deliberate reclamation cannot restart an already completed copy.
        for entry in copies {
            let Some(copy) = entry.file_copy else {
                continue;
            };
            let descriptor = copy.descriptor;
            let Some(logical) =
                file_preparation::GroupFileLogical::from_manifest(&descriptor.manifest)
            else {
                continue;
            };
            let payload = GroupFilePayload {
                logical,
                descriptor: Some(descriptor),
            };
            let Some(event_id) = entry.status.primary_event else {
                continue;
            };
            let private = self
                .account_file_proofs
                .lock()
                .expect("file proofs mutex")
                .get(&event_id)?;
            let Some(event) = (match private {
                Some(event) => Some(event),
                None => self.log.lock().expect("log mutex").get(&event_id)?,
            }) else {
                continue;
            };
            let Some(descriptor) = payload
                .descriptor
                .as_ref()
                .filter(|descriptor| descriptor.valid_for(&entry.reference, &payload.logical))
            else {
                continue;
            };
            if self.recipient_expired(&descriptor.manifest, event.id, event.wall_clock)
                || self
                    .recalls
                    .lock()
                    .expect("recalls mutex")
                    .contains(entry.reference.group, event.id)?
            {
                continue;
            }
            let has_card = self
                .received_files
                .lock()
                .expect("received files mutex")
                .entry(event.id)?
                .is_some_and(|row| {
                    row.conversation == entry.reference.group
                        && row.from == entry.reference.executor.device.user_id()
                        && row.wall_clock == event.wall_clock
                        && row.plaintext == descriptor.manifest
                });
            if !has_card {
                continue;
            }
            let Some(manifest) = crate::node::files::validated_manifest(&descriptor.manifest)
            else {
                continue;
            };
            work.push(AccountGroupFileWork {
                peer_id: entry.reference.executor.device.user_id(),
                id: entry.reference.id,
                file: manifest.file_conv(),
                size: payload.logical.size,
                reference: entry.reference,
                payload,
                committed: Some(event),
            });
        }
        work.sort_by_key(|job| job.id);
        work.dedup_by_key(|job| job.id);
        Ok(work)
    }

    pub(in crate::node) async fn execute_account_group_file_work(
        self: &Arc<Self>,
        work: AccountGroupFileWork,
    ) -> Result<(), NodeError> {
        if let Some(event) = work.committed {
            self.fetch_account_group_file_result_assets(work.reference, work.payload, event)
                .await?;
            self.note_account_history_change();
        } else {
            self.fetch_account_group_file_assets(work.reference, work.payload)
                .await?;
        }
        self.account_sync_notify.notify_one();
        Ok(())
    }
}
