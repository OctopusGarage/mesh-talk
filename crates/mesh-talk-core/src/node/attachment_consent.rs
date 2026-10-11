//! Local, encrypted receive preferences and exact attachment decisions.
//! A manifest is an offer; chunk transfer is permitted only after this store says yes.

use crate::eventlog::EventId;
use crate::eventlog::{ConversationId, LogError};
use crate::identity::device::PublicIdentity;
use crate::node::{Node, NodeError};
use crate::storage::record_log::EncryptedRecordLog;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::path::Path;

const MAGIC: &[u8; 6] = b"MTACPT";
const MAX_DECISIONS: usize = 4096;
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
    decisions: HashMap<EventId, bool>,
    order: VecDeque<EventId>,
    writes: usize,
}

impl AttachmentConsent {
    pub fn open(path: &Path, password: &str) -> Result<Self, LogError> {
        let (log, records) = EncryptedRecordLog::open(path, password, MAGIC)?;
        let writes = records.len();
        let mut store = Self {
            log,
            global_auto: false,
            contacts: HashMap::new(),
            decisions: HashMap::new(),
            order: VecDeque::new(),
            writes,
        };
        for record in records {
            store.apply(record);
        }
        store.trim();
        if store.writes >= COMPACT_AFTER {
            store.compact()?;
        }
        Ok(store)
    }

    fn apply(&mut self, record: Record) {
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
                self.order.retain(|id| *id != event);
                self.order.push_back(event);
                self.decisions.insert(event, accept);
            }
        }
    }

    fn trim(&mut self) {
        while self.order.len() > MAX_DECISIONS {
            if let Some(old) = self.order.pop_front() {
                self.decisions.remove(&old);
            }
        }
    }

    fn write(&mut self, record: Record) -> Result<(), LogError> {
        self.log.append_durable(&record)?;
        self.apply(record);
        self.trim();
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
        for event in &self.order {
            if let Some(accept) = self.decisions.get(event) {
                records.push(Record::File {
                    event: *event,
                    accept: *accept,
                });
            }
        }
        self.log.rewrite(&records)?;
        self.writes = records.len();
        Ok(())
    }

    pub fn global_auto(&self) -> bool {
        self.global_auto
    }

    pub fn contact_auto(&self, account: &str) -> Option<bool> {
        self.contacts.get(account).copied()
    }

    pub fn set_global_auto(&mut self, auto: bool) -> Result<(), LogError> {
        self.write(Record::Global(auto))
    }

    pub fn set_contact_auto(&mut self, account: &str, auto: Option<bool>) -> Result<(), LogError> {
        if account.len() != 32 || !account.bytes().all(|c| c.is_ascii_hexdigit()) {
            return Err(LogError::Serialization("invalid account id".into()));
        }
        self.write(Record::Contact {
            account: account.to_owned(),
            auto,
        })
    }

    pub fn decide(&mut self, event: EventId, accept: bool) -> Result<(), LogError> {
        self.write(Record::File { event, accept })
    }

    #[cfg(test)]
    pub fn status(&self, event: EventId, account: &str) -> ReceiveDecision {
        self.status_for_events(&[event], account)
    }

    pub fn status_for_events(&self, events: &[EventId], account: &str) -> ReceiveDecision {
        let decision = self.order.iter().rev().find_map(|event| {
            events
                .contains(event)
                .then(|| self.decisions.get(event).copied())
                .flatten()
        });
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
        let files = self
            .files
            .lock()
            .expect("files lock not poisoned")
            .file_convs();
        for file in files {
            if self.attachment_consent_status(file) == Some(ReceiveDecision::Accepted)
                && matches!(self.file_progress(file), Some(progress) if progress.done < progress.total)
            {
                self.pending_files
                    .lock()
                    .expect("pending files lock not poisoned")
                    .insert(file);
            }
        }
    }

    fn incoming_offer(
        &self,
        file: ConversationId,
        event_id: Option<EventId>,
        sender: Option<&PublicIdentity>,
    ) -> Option<(bool, String, EventId, PublicIdentity)> {
        let (manifest, mut ids) = {
            let files = self.files.lock().expect("files lock not poisoned");
            (files.manifest(&file)?.clone(), files.manifest_events(file))
        };
        if manifest.file_conv() != file {
            return None;
        }
        ids.sort_unstable();
        let own = self.identity.public();
        for id in ids {
            if event_id.is_some_and(|expected| expected != id) {
                continue;
            }
            let Some(event) = ({
                self.log
                    .lock()
                    .expect("log lock not poisoned")
                    .get(&id)
                    .cloned()
            }) else {
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
            // SDK/legacy device-only peers have no account certificate. Their
            // stable device fingerprint serves as the preference key.
            let account = proof
                .account_id()
                .unwrap_or_else(|| proof.public().user_id());
            return Some((
                crate::node::media_store::automatic_inline_image(&manifest)
                    || account == self.account_id(),
                account,
                id,
                proof.public(),
            ));
        }
        None
    }

    /// A sender may query only its own exact manifest after the recipient has opened it.
    /// Unknown or not-yet-opened offers remain pending.
    pub(in crate::node) fn attachment_consent_for_peer(
        &self,
        file: ConversationId,
        manifest_event: EventId,
        peer: &PublicIdentity,
    ) -> ReceiveDecision {
        let Some((automatic, account, _, _)) =
            self.incoming_offer(file, Some(manifest_event), Some(peer))
        else {
            return ReceiveDecision::Pending;
        };
        if automatic {
            return ReceiveDecision::Accepted;
        }
        let events = self.equivalent_offer_events(file);
        self.attachment_consent
            .lock()
            .expect("consent lock not poisoned")
            .status_for_events(&events, &account)
    }

    /// Status for an attachment surfaced to this account. `None` means there is
    /// no verified incoming offer for the requested file conversation.
    pub fn attachment_consent_status(&self, file: ConversationId) -> Option<ReceiveDecision> {
        let (automatic, account, _, _) = self.incoming_offer(file, None, None)?;
        Some(if automatic {
            ReceiveDecision::Accepted
        } else {
            let events = self.equivalent_offer_events(file);
            self.attachment_consent
                .lock()
                .expect("consent lock not poisoned")
                .status_for_events(&events, &account)
        })
    }

    pub(in crate::node) fn attachment_chunk_author_allowed(
        &self,
        file: ConversationId,
        author: &[u8; 32],
    ) -> bool {
        let Some((automatic, account, _, sender)) = self.incoming_offer(file, None, None) else {
            return false;
        };
        let events = self.equivalent_offer_events(file);
        &sender.ed25519_pub == author
            && (automatic
                || self
                    .attachment_consent
                    .lock()
                    .expect("consent lock not poisoned")
                    .status_for_events(&events, &account)
                    == ReceiveDecision::Accepted)
    }

    fn equivalent_offer_events(&self, file: ConversationId) -> Vec<EventId> {
        self.files
            .lock()
            .expect("files lock not poisoned")
            .manifest_events(file)
    }

    pub fn decide_attachment(&self, file: ConversationId, accept: bool) -> Result<(), NodeError> {
        let Some((automatic, _, event, _)) = self.incoming_offer(file, None, None) else {
            return Err(NodeError::InvalidInput(
                "unknown incoming attachment".into(),
            ));
        };
        if automatic {
            return Err(NodeError::InvalidInput(
                "attachment does not require approval".into(),
            ));
        }
        // One durable decision covers all identical, same-author offer events.
        self.attachment_consent
            .lock()
            .expect("consent lock not poisoned")
            .decide(event, accept)
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
