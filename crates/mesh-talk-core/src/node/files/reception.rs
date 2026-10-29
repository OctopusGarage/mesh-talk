//! Selected receipt presentation composes the file authority owner. A durable
//! cache row alone cannot authorize a file card after erase, recall or expiry.
use super::*;
use crate::eventlog::LogError;
use crate::node::reception_effects::{ArrivalKind, ReceptionMetadata};

impl Node {
    pub(in crate::node) fn received_file_for_reception(
        &self,
        entry: &ReceivedEntry,
        metadata: &ReceptionMetadata,
    ) -> Result<Option<ReceivedFile>, LogError> {
        {
            let delivery = self.delivery.lock().expect("delivery lock not poisoned");
            if delivery.file_erased(entry.conversation, entry.event_id)
                || delivery.manifest_event_erased(entry.event_id)
            {
                return Ok(None);
            }
        }
        if self.file_message_row_suppressed(entry)?
            || self.recipient_expired(&entry.plaintext, entry.event_id, entry.wall_clock)
        {
            return Ok(None);
        }
        let original = self
            .log
            .lock()
            .expect("log lock not poisoned")
            .get(&entry.event_id)?;
        let original = match original {
            Some(event) => Some(event),
            None => self
                .account_file_proofs
                .lock()
                .expect("file proofs lock not poisoned")
                .get(&entry.event_id)?,
        };
        let Some(original) = original else {
            return Ok(None);
        };
        let decoded = validated_manifest(&entry.plaintext);
        let existing = decoded.as_ref().and_then(|manifest| {
            self.files
                .lock()
                .expect("file book lock not poisoned")
                .manifest(&manifest.file_conv())
                .cloned()
        });
        let Some(manifest) = eligible_manifest_row(entry, Some(&original), existing.as_ref())
        else {
            return Ok(None);
        };
        let is_channel = self
            .channels
            .lock()
            .expect("channels lock not poisoned")
            .state(&entry.conversation)
            .is_some();
        let (event_id, author_account) = {
            let identities = self
                .file_message_identities
                .lock()
                .expect("file identity lock not poisoned");
            (
                identities.row_logical(entry)?,
                identities.row_author_account(entry)?,
            )
        };
        // Existing DM reception suppresses copies from certified sibling
        // devices; channel/history rows retain explicit own-device metadata.
        if !is_channel && metadata.own_device {
            return Ok(None);
        }
        let account = (!is_channel).then_some(author_account.as_deref()).flatten();
        if self
            .message_recalled_selected(
                entry.conversation,
                event_id,
                account,
                author_account.as_deref(),
            )
            .map_err(|error| match error {
                NodeError::Log(error) => error,
                error => LogError::CorruptFile(format!("file recall observation: {error}")),
            })?
        {
            return Ok(None);
        }
        Ok(Some(ReceivedFile {
            protection: manifest.protection(),
            expires_at_ms: crate::expiry::deadline(&entry.plaintext),
            conv: if is_channel {
                entry.conversation
            } else {
                original.conversation_id
            },
            is_channel,
            from: metadata.from_device.clone(),
            name: if manifest.protection().is_some() {
                "Protected image".into()
            } else {
                manifest.name().to_owned()
            },
            size: manifest.size(),
            mime: manifest.mime().to_owned(),
            file_conv: manifest.file_conv(),
            media: super::super::media_store::manifest_is_media(&manifest),
            event_id,
            authored_at: entry.wall_clock,
            received_at: metadata.received_at,
            arrival_kind: match metadata.arrival_kind {
                ArrivalKind::Live => "live",
                ArrivalKind::CatchUp => "catch_up",
                ArrivalKind::HistorySync => "history_sync",
            },
            own_device: metadata.own_device,
            batch_id: metadata.batch_id.clone(),
            batch_complete: metadata.batch_complete,
            batch_size: metadata.batch_size,
        }))
    }
}
