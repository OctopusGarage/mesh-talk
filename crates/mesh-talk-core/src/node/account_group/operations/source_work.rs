//! Explicit account work uses the existing bounded account data workers.
use super::{file_work::AccountGroupFileWork, *};
use std::sync::Arc;

pub(in crate::node) enum AccountGroupDataWork {
    File(Box<AccountGroupFileWork>),
    Invitation {
        id: AccountGroupOperationId,
        group: ConversationId,
        target: String,
    },
}

impl AccountGroupDataWork {
    pub(in crate::node) fn id(&self) -> AccountGroupOperationId {
        match self {
            Self::File(work) => work.id,
            Self::Invitation { id, .. } => *id,
        }
    }
    pub(in crate::node) fn worker_key(&self) -> String {
        match self {
            Self::File(work) => work.peer_id.clone(),
            Self::Invitation { id, .. } => format!("invitation:{}", id.to_hex()),
        }
    }
    pub(in crate::node) fn conversation(&self) -> ConversationId {
        match self {
            Self::File(work) => work.file,
            Self::Invitation { group, .. } => *group,
        }
    }
    pub(in crate::node) fn size(&self) -> u64 {
        match self {
            Self::File(work) => work.size,
            Self::Invitation { .. } => 0,
        }
    }
}

/// A bounded journal scan turn, including progress over candidates whose data is retired.
pub(in crate::node) struct AccountGroupDataWorkTurn {
    pub work: Vec<AccountGroupDataWork>,
    pub after: Option<AccountGroupOperationId>,
    pub more: bool,
}

const DATA_WORK_CANDIDATES: usize = 128;

impl Node {
    fn account_group_data_candidates(
        &self,
        after: Option<AccountGroupOperationId>,
    ) -> (Vec<journal::Entry>, Option<AccountGroupOperationId>, bool) {
        use std::ops::Bound::{Excluded, Unbounded};
        let device = self.identity.public();
        let journal = self
            .group_operations
            .lock()
            .expect("operation journal mutex");
        let eligible = |entry: &journal::Entry| match entry.reference.kind {
            AccountGroupOperationKind::File if !entry.source => {
                (!entry.cancel && !entry.terminal() && entry.reference.executor.device == device)
                    || (entry.status.phase == AccountGroupOperationPhase::Committed
                        && entry.file_copy.as_ref().is_some_and(|copy| !copy.complete))
            }
            AccountGroupOperationKind::Invite => {
                entry.source
                    && !entry.terminal()
                    && !entry.cancel
                    && entry.reference.executor.device == device
                    && matches!(
                        entry.body.as_ref(),
                        Some(AccountGroupOperation::Invite { .. })
                    )
            }
            _ => false,
        };
        let scan = |start| {
            journal
                .entries
                .range((start, Unbounded))
                .filter(|(_, entry)| eligible(entry))
        };
        let mut candidates = scan(after.map_or(Unbounded, Excluded)).peekable();
        // A completed cycle wraps on the next ordinary turn, never by self-notifying.
        let wrapped = after.is_some() && candidates.peek().is_none();
        if wrapped {
            candidates = scan(Unbounded).peekable();
        }
        let entries: Vec<_> = candidates
            .by_ref()
            .take(DATA_WORK_CANDIDATES)
            .map(|(_, entry)| entry.clone())
            .collect();
        let more = !wrapped && candidates.next().is_some();
        let next = entries.last().map(|entry| entry.reference.id).or(after);
        (entries, next, more)
    }

    pub(in crate::node) fn account_group_data_work(
        &self,
        after: Option<AccountGroupOperationId>,
    ) -> AccountGroupDataWorkTurn {
        let (candidates, after, more) = self.account_group_data_candidates(after);
        let mut files = Vec::new();
        let mut work = Vec::new();
        for entry in candidates {
            if entry.reference.kind == AccountGroupOperationKind::File {
                files.push(entry);
            } else if let Some(AccountGroupOperation::Invite { target_account_id }) = entry.body {
                work.push(AccountGroupDataWork::Invitation {
                    id: entry.reference.id,
                    group: entry.reference.group,
                    target: target_account_id,
                });
            }
        }
        work.extend(
            self.account_group_file_work(files)
                .into_iter()
                .map(|work| AccountGroupDataWork::File(Box::new(work))),
        );
        work.sort_by_key(AccountGroupDataWork::id);
        AccountGroupDataWorkTurn { work, after, more }
    }

    pub(in crate::node) async fn execute_account_group_data_work(
        self: &Arc<Self>,
        work: AccountGroupDataWork,
    ) -> Result<(), NodeError> {
        match work {
            AccountGroupDataWork::File(work) => self.execute_account_group_file_work(*work).await,
            AccountGroupDataWork::Invitation { id, group, target } => {
                let assets = self.prepare_invitation_assets(group, &target).await?;
                if let Some(group) =
                    self.execute_source_control_with_invitation(id, Some(assets))?
                {
                    self.publish_account_group_access_for(group);
                    self.publish_group_inventory_for(group);
                }
                self.publish_account_group_operations();
                self.account_sync_notify.notify_one();
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        discovery::Roster,
        identity::{account::Account, device::DeviceIdentity},
    };
    use sha2::Digest;
    use std::sync::Mutex;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn retired_file_copies_cannot_hide_a_later_live_result_copy() {
        let dir = tempfile::tempdir().unwrap();
        let account = Account::generate();
        let (dm, _) = mpsc::unbounded_channel();
        let (channels, _) = mpsc::unbounded_channel();
        let (files, _) = mpsc::unbounded_channel();
        let node = Node::open_with_account(
            DeviceIdentity::generate(),
            account,
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channels,
            files,
            &dir.path().join("events"),
            &dir.path().join("sent"),
            "pw",
        )
        .unwrap();
        let group = node
            .create_channel("copy scheduling", vec![])
            .await
            .unwrap();
        let path = dir.path().join("original.bin");
        std::fs::write(&path, b"original result bytes").unwrap();
        let live_id =
            AccountGroupOperationId::for_origin(&node.identity.public(), 130u128.to_be_bytes());
        node.submit_account_group_file_if(
            &node.account_id(),
            live_id,
            group,
            &path,
            crate::file::FileKind::File,
            None,
            None,
            |_| {},
            |commit| commit(),
        )
        .await
        .unwrap();
        node.maintain_account_group_operation_effects();
        {
            let mut journal = node.group_operations.lock().unwrap();
            let mut live = journal.entries.get(&live_id).unwrap().clone();
            assert_eq!(live.status.phase, AccountGroupOperationPhase::Committed);
            live.source = false;
            live.file_copy.as_mut().unwrap().complete = false;
            journal.entries.insert(live_id, live.clone());
            for index in 1u128..=129 {
                let id = AccountGroupOperationId::for_origin(
                    &node.identity.public(),
                    index.to_be_bytes(),
                );
                let mut retired = live.clone();
                retired.reference.id = id;
                retired.reference.signature =
                    node.identity.sign(&retired.reference.message()).to_vec();
                retired.status.id = id;
                // A retained terminal receipt whose local manifest/proof was erased.
                let erased = EventId::new(sha2::Sha256::digest(index.to_be_bytes()).into());
                retired.status.primary_event = Some(erased);
                retired.status.committed_events = vec![erased];
                let old = &retired.file_copy.as_ref().unwrap().descriptor;
                let descriptor = file_preparation::GroupFileDescriptor::new(
                    &node,
                    id,
                    group,
                    retired.reference.digest,
                    old.manifest.clone(),
                    old.asset_ciphertext_digest,
                );
                retired.file_copy.as_mut().unwrap().descriptor = descriptor;
                journal.entries.insert(id, retired);
            }
        }
        let before = node.group_operations.lock().unwrap().entries.len();
        let mut reached = false;
        let mut after = None;
        for turn_index in 0..4 {
            let (snapshots, next, more) = node.account_group_data_candidates(after);
            assert!(snapshots.len() <= DATA_WORK_CANDIDATES);
            assert_eq!(snapshots.len(), if turn_index % 2 == 0 { 128 } else { 2 });
            let turn = node.account_group_data_work(after);
            assert_eq!(turn.after, next);
            assert_eq!(turn.more, more);
            assert_eq!(
                turn.more,
                turn_index == 0,
                "wrapping must not restart prompt paging through retained metadata"
            );
            assert_ne!(
                turn.after, after,
                "skipped candidates must advance the cursor"
            );
            after = turn.after;
            let work = turn.work;
            assert!(work.len() <= 128);
            assert!(
                work.iter().all(|job| job.id() == live_id),
                "retired metadata must never become work"
            );
            reached |= work.iter().any(|job| job.id() == live_id);
        }
        assert!(
            reached,
            "later live copies must be reached despite 129 retained erased copies"
        );
        let journal = node.group_operations.lock().unwrap();
        assert_eq!(journal.entries.len(), before);
        assert!(
            journal
                .entries
                .values()
                .all(|entry| !entry.file_copy.as_ref().unwrap().complete)
        );
    }
}
