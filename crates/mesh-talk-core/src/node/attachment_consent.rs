//! Local, encrypted receive preferences and exact attachment decisions.
//! A manifest is an offer; chunk transfer is permitted only after this store says yes.

use crate::eventlog::EventId;
use crate::eventlog::{ConversationId, LogError};
use crate::identity::device::PublicIdentity;
use crate::node::{Node, NodeError};
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTACPT";
const COMPACT_AFTER: usize = 8192;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum ReceiveDecision {
    Pending,
    Accepted,
    Declined,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum Record {
    Global(bool),
    Contact { account: String, auto: Option<bool> },
    File { event: EventId, accept: bool },
}

/// Preferences apply to offers without exact decisions. Media is handled by the caller
/// before consulting this store. Every successful change is synced before publication.
pub struct AttachmentConsent {
    log: EncryptedRecordLog<Record>,
    global_auto: bool,
    contacts: HashMap<String, bool>,
    decisions: HashMap<EventId, (bool, u64)>,
    next_sequence: u64,
    writes: usize,
    generation: u64,
}

impl AttachmentConsent {
    /// Load the account-local encrypted preferences and exact offer decisions.
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (log, records) = EncryptedRecordLog::open(path, password, MAGIC)?;
        let record_count = records.len();
        let mut store = Self {
            log,
            global_auto: false,
            contacts: HashMap::new(),
            decisions: HashMap::new(),
            next_sequence: 0,
            writes: 0,
            generation: 0,
        };
        for record in records {
            store.apply(record);
        }
        let live_records = 1 + store.contacts.len() + store.decisions.len();
        if record_count >= COMPACT_AFTER && record_count >= live_records.saturating_mul(2) {
            store.compact()?;
        }
        Ok(store)
    }

    fn apply(&mut self, record: Record) {
        self.generation = self.generation.wrapping_add(1);
        match record {
            Record::Global(auto) => self.global_auto = auto,
            Record::Contact { account, auto } => match auto {
                Some(auto) => {
                    self.contacts.insert(account, auto);
                }
                None => {
                    self.contacts.remove(&account);
                }
            },
            Record::File { event, accept } => {
                self.next_sequence += 1;
                self.decisions.insert(event, (accept, self.next_sequence));
            }
        }
    }

    fn write(&mut self, record: Record) -> Result<(), LogError> {
        self.log.append_durable(&record)?;
        self.apply(record);
        self.writes += 1;
        if self.writes >= COMPACT_AFTER {
            // The new decision is already durable. Compaction is maintenance; a
            // transient rewrite failure must not report the accepted action as failed.
            let _ = self.compact();
        }
        Ok(())
    }

    fn compact(&mut self) -> Result<(), LogError> {
        let mut records = vec![Record::Global(self.global_auto)];
        for (account, auto) in &self.contacts {
            records.push(Record::Contact {
                account: account.clone(),
                auto: Some(*auto),
            });
        }
        let mut decisions = self.decisions.iter().collect::<Vec<_>>();
        decisions.sort_unstable_by_key(|(_, (_, sequence))| *sequence);
        for (event, (accept, _)) in decisions {
            records.push(Record::File {
                event: *event,
                accept: *accept,
            });
        }
        self.log.rewrite(&records)?;
        // Count writes since the rewrite. The set of unique offers may itself
        // exceed COMPACT_AFTER; using its size here would rewrite on every send.
        self.writes = 0;
        Ok(())
    }

    pub(super) fn generation(&self) -> u64 {
        self.generation
    }

    /// Whether generic attachments are received automatically by default.
    pub fn global_auto(&self) -> bool {
        self.global_auto
    }

    /// Return a contact override, if one was set.
    pub fn contact_auto(&self, account: &str) -> Option<bool> {
        self.contacts.get(account).copied()
    }

    /// Durably set the account-wide default for generic attachments.
    pub fn set_global_auto(&mut self, auto: bool) -> Result<(), LogError> {
        self.write(Record::Global(auto))
    }

    /// Durably set or remove one contact's automatic receive override.
    pub fn set_contact_auto(&mut self, account: &str, auto: Option<bool>) -> Result<(), LogError> {
        if account.len() != 32 || !account.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(LogError::Serialization("invalid account id".into()));
        }
        self.write(Record::Contact {
            account: account.to_owned(),
            auto,
        })
    }

    /// Durably accept or decline one signed manifest event.
    pub fn decide(&mut self, event: EventId, accept: bool) -> Result<(), LogError> {
        self.write(Record::File { event, accept })
    }

    #[cfg(test)]
    pub fn status(&self, event: EventId, account: &str) -> ReceiveDecision {
        self.status_for_events(&[event], account)
    }

    /// Resolve exact decisions across equivalent signed offers before preferences.
    pub fn status_for_events(&self, events: &[EventId], account: &str) -> ReceiveDecision {
        let decision = events
            .iter()
            .filter_map(|event| self.decisions.get(event))
            .max_by_key(|(_, sequence)| sequence)
            .map(|(accept, _)| *accept);
        match decision {
            Some(true) => ReceiveDecision::Accepted,
            Some(false) => ReceiveDecision::Declined,
            None if self
                .contacts
                .get(account)
                .copied()
                .unwrap_or(self.global_auto) =>
            {
                ReceiveDecision::Accepted
            }
            None => ReceiveDecision::Pending,
        }
    }
}

impl Node {
    pub(in crate::node) fn queue_approved_incomplete_files(&self) {
        let result = (|| -> Result<(), NodeError> {
            let files = self
                .files
                .lock()
                .expect("files lock not poisoned")
                .file_convs();
            for file in files {
                if match self.attachment_consent_status(file) {
                    Ok(status) => status,
                    Err(error) => {
                        log::warn!("attachment consent lookup deferred: {error}");
                        continue;
                    }
                } == Some(ReceiveDecision::Accepted)
                    && !(self.media.contains(file) && self.has_verified_file_completion(file))
                    && matches!(self.file_progress(file)?, Some(progress) if progress.done < progress.total)
                {
                    self.pending_files
                        .lock()
                        .expect("pending files lock not poisoned")
                        .insert(file);
                }
            }
            self.file_pull_notify.notify_one();
            self.note_file_change();

            Ok(())
        })();
        if let Err(error) = result {
            log::warn!("queue_approved_incomplete_files deferred after storage failure: {error}");
        }
    }

    pub(in crate::node) fn incoming_offer(
        &self,
        file: ConversationId,
        event_id: Option<EventId>,
        sender: Option<&PublicIdentity>,
    ) -> Result<Option<(bool, String, EventId, PublicIdentity)>, crate::eventlog::LogError> {
        let (manifest, mut ids) = {
            let files = self.files.lock().expect("files lock not poisoned");
            let Some(manifest) = files.manifest(&file).cloned() else {
                return Ok(None);
            };
            (
                manifest,
                match event_id {
                    Some(id) if files.has_manifest_event(file, id) => vec![id],
                    Some(_) => return Ok(None),
                    None => files.manifest_events(file),
                },
            )
        };
        if manifest.file_conv() != file {
            return Ok(None);
        }
        ids.sort_unstable();
        let own = self.identity.public();
        for id in ids {
            if event_id.is_some_and(|expected| expected != id) {
                continue;
            }
            let Some(event) = ({ self.log.lock().expect("log lock not poisoned").get(&id)? })
            else {
                continue;
            };
            if event.kind != crate::eventlog::EventKind::FileManifest
                || event.author.ed25519_pub() == &own.ed25519_pub
                || sender.is_some_and(|peer| event.author.ed25519_pub() != &peer.ed25519_pub)
            {
                continue;
            }
            let Some(proof) = self.historical_author(event.author.ed25519_pub()) else {
                continue;
            };
            if sender.is_some_and(|peer| proof.public() != *peer) {
                continue;
            }
            if event.conversation_id
                == super::conversation::dm_conversation_id(&own, &proof.public())
                && self.certified_sibling(&proof.public())
                && !self.device_sync_policy().attachments
            {
                continue;
            }
            // SDK/legacy device-only peers have no account certificate. Their
            // stable device fingerprint serves as the preference key.
            let account = proof
                .account_id()
                .unwrap_or_else(|| proof.public().user_id());
            return Ok(Some((
                crate::node::media_store::automatic_inline_image(&manifest)
                    || account == self.account_id(),
                account,
                id,
                proof.public(),
            )));
        }
        Ok(None)
    }

    /// A sender may query only its own exact manifest after the recipient has opened it.
    /// Unknown or not-yet-opened offers remain pending.
    pub(in crate::node) fn attachment_consent_for_peer(
        &self,
        file: ConversationId,
        manifest_event: EventId,
        peer: &PublicIdentity,
    ) -> Result<ReceiveDecision, crate::eventlog::LogError> {
        let Some((automatic, account, _, _)) =
            self.incoming_offer(file, Some(manifest_event), Some(peer))?
        else {
            return Ok(ReceiveDecision::Pending);
        };
        if automatic {
            return Ok(ReceiveDecision::Accepted);
        }
        let events = self.equivalent_offer_events(file);
        Ok(self
            .attachment_consent
            .lock()
            .expect("consent lock not poisoned")
            .status_for_events(&events, &account))
    }

    /// Status for an attachment surfaced to this account. `None` means there is
    /// no verified incoming offer for the requested file conversation.
    pub fn attachment_consent_status(
        &self,
        file: ConversationId,
    ) -> Result<Option<ReceiveDecision>, crate::eventlog::LogError> {
        let events = self.equivalent_offer_events(file);
        let offers: Vec<_> = events
            .iter()
            .map(|id| self.incoming_offer(file, Some(*id), None))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        if offers.is_empty() {
            return Ok(None);
        }
        let consent = self
            .attachment_consent
            .lock()
            .expect("consent lock not poisoned");
        let mut pending = false;
        let mut accepted = false;
        for (automatic, account, _, _) in offers {
            let decision = if automatic {
                ReceiveDecision::Accepted
            } else {
                consent.status_for_events(&events, &account)
            };
            match decision {
                ReceiveDecision::Accepted => accepted = true,
                ReceiveDecision::Pending => pending = true,
                ReceiveDecision::Declined => {}
            }
        }
        Ok(Some(if pending {
            ReceiveDecision::Pending
        } else if accepted {
            ReceiveDecision::Accepted
        } else {
            ReceiveDecision::Declined
        }))
    }

    pub(in crate::node) fn attachment_chunk_author_allowed(
        &self,
        file: ConversationId,
        author: &[u8; 32],
    ) -> Result<bool, crate::eventlog::LogError> {
        if !self
            .files
            .lock()
            .expect("files lock not poisoned")
            .has_author(file, crate::eventlog::Author::from_ed25519(*author))
        {
            return Ok(false);
        }
        let events = self.equivalent_offer_events(file);
        let offers: Vec<_> = events
            .iter()
            .map(|id| self.incoming_offer(file, Some(*id), None))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .filter(|(_, _, _, sender)| &sender.ed25519_pub == author)
            .collect();
        let consent = self
            .attachment_consent
            .lock()
            .expect("consent lock not poisoned");
        Ok(offers.into_iter().any(|(automatic, account, _, _)| {
            automatic || consent.status_for_events(&events, &account) == ReceiveDecision::Accepted
        }))
    }

    fn equivalent_offer_events(&self, file: ConversationId) -> Vec<EventId> {
        self.files
            .lock()
            .expect("files lock not poisoned")
            .manifest_events(file)
    }

    pub fn decide_attachment(&self, file: ConversationId, accept: bool) -> Result<(), NodeError> {
        let offers: Vec<_> = self
            .equivalent_offer_events(file)
            .into_iter()
            .map(|id| self.incoming_offer(file, Some(id), None))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .flatten()
            .collect();
        let Some((_, _, event, _)) = offers.iter().find(|(automatic, _, _, _)| !automatic) else {
            return Err(NodeError::InvalidInput(
                "attachment does not require approval or is unknown".into(),
            ));
        };
        // One durable decision covers all identical signed offer events for this file.
        self.attachment_consent
            .lock()
            .expect("consent lock not poisoned")
            .decide(*event, accept)
            .map_err(NodeError::Log)?;
        if accept {
            self.pending_files
                .lock()
                .expect("pending files lock not poisoned")
                .insert(file);
        } else {
            self.pending_files
                .lock()
                .expect("pending files lock not poisoned")
                .remove(&file);
        }
        self.file_pull_notify.notify_one();
        self.note_file_change();
        Ok(())
    }

    pub fn attachment_global_auto(&self) -> bool {
        self.attachment_consent
            .lock()
            .expect("consent lock not poisoned")
            .global_auto()
    }

    pub fn attachment_contact_auto(&self, account: &str) -> Option<bool> {
        self.attachment_consent
            .lock()
            .expect("consent lock not poisoned")
            .contact_auto(account)
    }

    pub fn set_attachment_global_auto(&self, auto: bool) -> Result<(), NodeError> {
        self.attachment_consent
            .lock()
            .expect("consent lock not poisoned")
            .set_global_auto(auto)
            .map_err(NodeError::Log)?;
        self.queue_approved_incomplete_files();
        Ok(())
    }

    pub fn set_attachment_contact_auto(
        &self,
        account: &str,
        auto: Option<bool>,
    ) -> Result<(), NodeError> {
        self.attachment_consent
            .lock()
            .expect("consent lock not poisoned")
            .set_contact_auto(account, auto)
            .map_err(NodeError::Log)?;
        self.queue_approved_incomplete_files();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_decline_survives_decision_limit_and_compaction() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("attachment-consent.log");
        let account = "a".repeat(32);
        let declined = EventId::new([1; 32]);
        let reapproved_alias = EventId::new([2; 32]);
        {
            let mut consent = AttachmentConsent::open(&path, "pw").unwrap();
            consent.set_global_auto(true).unwrap();
            consent.decide(declined, false).unwrap();
            consent.apply(Record::File {
                event: reapproved_alias,
                accept: true,
            });
            // Cross the previous 4096-decision eviction boundary.
            for index in 0..=4096 {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&(index as u64 + 2).to_le_bytes());
                consent.apply(Record::File {
                    event: EventId::new(bytes),
                    accept: true,
                });
            }
            assert_eq!(
                consent.status(declined, &account),
                ReceiveDecision::Declined
            );
            assert_eq!(
                consent.status_for_events(&[declined, reapproved_alias], &account),
                ReceiveDecision::Accepted
            );
            consent.compact().unwrap();
        }
        let reopened = AttachmentConsent::open(&path, "pw").unwrap();
        assert_eq!(
            reopened.status(declined, &account),
            ReceiveDecision::Declined
        );
        assert_eq!(
            reopened.status_for_events(&[declined, reapproved_alias], &account),
            ReceiveDecision::Accepted
        );
    }

    #[test]
    fn reopening_many_unique_decisions_does_not_rewrite_the_encrypted_log() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("attachment-consent.log");
        {
            let mut consent = AttachmentConsent::open(&path, "pw").unwrap();
            for index in 0..COMPACT_AFTER {
                let mut bytes = [0; 32];
                bytes[..8].copy_from_slice(&(index as u64).to_le_bytes());
                consent.apply(Record::File {
                    event: EventId::new(bytes),
                    accept: true,
                });
            }
            consent.compact().unwrap();
        }
        let encrypted = std::fs::read(&path).unwrap();
        let reopened = AttachmentConsent::open(&path, "pw").unwrap();
        assert_eq!(reopened.decisions.len(), COMPACT_AFTER);
        assert_eq!(std::fs::read(&path).unwrap(), encrypted);
    }

    #[test]
    fn preferences_and_exact_decisions_survive_reopen() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("attachment-consent.log");
        let file = EventId::new([7; 32]);
        let account = "a".repeat(32);
        {
            let mut consent = AttachmentConsent::open(&path, "pw").unwrap();
            assert_eq!(consent.status(file, &account), ReceiveDecision::Pending);
            consent.set_global_auto(true).unwrap();
            assert_eq!(consent.status(file, &account), ReceiveDecision::Accepted);
            consent.set_contact_auto(&account, Some(false)).unwrap();
            assert_eq!(consent.status(file, &account), ReceiveDecision::Pending);
            consent.decide(file, true).unwrap();
            assert_eq!(consent.status(file, &account), ReceiveDecision::Accepted);
            consent.decide(file, false).unwrap();
            assert_eq!(consent.status(file, &account), ReceiveDecision::Declined);
        }
        let mut reopened = AttachmentConsent::open(&path, "pw").unwrap();
        assert!(reopened.global_auto());
        assert_eq!(reopened.contact_auto(&account), Some(false));
        assert_eq!(reopened.status(file, &account), ReceiveDecision::Declined);
        reopened.set_contact_auto(&account, None).unwrap();
        assert_eq!(reopened.contact_auto(&account), None);
        let later = EventId::new([8; 32]);
        assert_eq!(
            reopened.status_for_events(&[file, later], &account),
            ReceiveDecision::Declined
        );
        reopened.decide(later, true).unwrap();
        assert_eq!(
            reopened.status_for_events(&[file, later], &account),
            ReceiveDecision::Accepted
        );
    }
}
