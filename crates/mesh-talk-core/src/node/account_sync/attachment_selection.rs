//! Bounded receiver-local missing-byte handoff; it never requests source history.
use super::*;
use std::collections::{BTreeMap, VecDeque};

const PAGE: usize = MAX_UNAVAILABLE_ATTACHMENTS;
pub(super) struct AttachmentPage {
    pub files: VecDeque<(ConversationId, bool)>,
    pub total: usize,
    pub after: Option<ConversationId>,
}
pub(super) struct AttachmentSelection {
    pub peer: PeerRecord,
    pub page: AttachmentPage,
    pub version: AccountSyncVersion,
    pub favorites_supported: bool,
    pub generation: u64,
    pub refill: bool,
    pub retry_at: Option<std::time::Instant>,
    pub read_failures: u8,
}
impl Node {
    pub(super) fn select_sibling_attachment_page(
        &self,
        after: Option<ConversationId>,
        excluded: HashSet<ConversationId>,
        favorites: bool,
    ) -> Result<AttachmentPage, NodeError> {
        #[cfg(test)]
        self.trace_account_client(&self.identity.public(), || {
            "attachment_selection_begin".into()
        });
        let mut forward = BTreeMap::new();
        let mut wrapped = BTreeMap::new();
        let mut total = 0;
        let mut offer = |file: ConversationId, favorite| {
            total += 1;
            if excluded.contains(&file) {
                return;
            }
            let page = if after.is_none_or(|after| file.as_bytes() > after.as_bytes()) {
                &mut forward
            } else {
                &mut wrapped
            };
            page.insert(*file.as_bytes(), (file, favorite));
            if page.len() > PAGE {
                page.pop_last();
            }
        };
        let files = self.files.lock().expect("files mutex not poisoned").files();
        for file in files {
            let file = file?;
            if !self.media.contains(file) && !self.account_file_recalled(file)? {
                offer(file, false);
            }
        }
        if favorites && self.device_sync_policy().history {
            for file in self.missing_favorite_files() {
                offer(file, true);
            }
        }
        if forward.is_empty() {
            forward = wrapped;
        }
        let files: VecDeque<_> = forward.into_values().collect();
        let after = files.back().map(|(file, _)| *file);
        Ok(AttachmentPage {
            files,
            total,
            after,
        })
    }
}
