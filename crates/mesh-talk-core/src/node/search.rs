//! Full-text search over retained DM and channel plaintext. Candidate selection
//! uses verified peer proofs and durable account history; routing remains separate.

use super::conversation::{account_conversation_id, dm_conversation_id};
use super::queries::decode_account_entry;
use super::{MessageBody, Node, SearchHit};
use crate::eventlog::event::{ConversationId, EventId};
use crate::identity::device::PublicIdentity;

const RECENT_HISTORY_LIMIT: usize = 2000;

/// Exact history identity, kept distinct from a result's display/account binding.
/// A missing ID is explicitly unavailable; it must never be resolved by text/time.
/// References do not authorize a later read; hosts must recheck owner and policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchReference {
    Account {
        account_id: String,
        message_id: Option<EventId>,
    },
    Channel {
        channel_id: ConversationId,
        message_id: Option<EventId>,
    },
    LegacyDevice {
        device_id: String,
        message_id: Option<EventId>,
    },
}

/// A presentation match and its canonical, conversation-scoped identity.
#[derive(Debug, Clone)]
pub struct ReferencedSearchHit {
    pub hit: SearchHit,
    pub reference: SearchReference,
    pub expires_at_ms: Option<u64>,
}

/// Exact context and the canonical position of its target in retained history.
/// Position metadata is descriptive and does not grant later read authority.
pub struct SearchContext {
    pub messages: Vec<super::HistoryEntry>,
    pub newer_entries: usize,
}

/// Matches from a recent canonical-history window in each eligible conversation.
/// This does not establish completeness over all retained or remote history.
#[derive(Debug, Clone)]
pub struct RecentSearchResults {
    pub hits: Vec<ReferencedSearchHit>,
    /// Window entries include files and recall tombstones, not just text matches.
    /// Canonical scans retain O(history) metadata and decode only the window.
    pub per_conversation_limit: usize,
}

impl Node {
    /// Search decrypted history across all known DMs + channels for `query`
    /// (case-insensitive substring). Empty/whitespace query → no hits.
    ///
    /// Scans the sent + received stores by reference without cloning their
    /// entry vectors. It resolves channel sent event IDs only when a matching
    /// channel has recalls; other searches skip the history log scan.
    pub fn search(&self, query: &str) -> Vec<SearchHit> {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return Vec::new();
        }
        const PER_CONV: usize = 2000;
        let mut hits: Vec<SearchHit> = Vec::new();

        for conversation in self.search_conversations() {
            let scope = match &conversation.source {
                SearchSource::Account(account) => SearchScope::Account(account),
                SearchSource::Channel => SearchScope::Channel,
                SearchSource::LegacyDevice(_) => SearchScope::LegacyDm,
            };
            for line in self.scan_conversation(conversation.id, &q, PER_CONV, scope) {
                hits.push(conversation.hit(line.from_me, line.who, line.text, line.wall_clock));
            }
        }
        hits.sort_by_key(|b| std::cmp::Reverse(b.wall_clock));
        hits
    }

    /// Search the latest 2,000 canonical history entries per eligible conversation.
    /// Matching is case-insensitive substring search. Unlike [`Node::search`], this
    /// opt-in API uses history's logical deduplication, pending accepted records,
    /// recovered IDs and channel causal ordering. Files and recall tombstones use
    /// window slots; only visible text is matched. A zero sentinel becomes no ID.
    ///
    /// Canonical projection scans retained metadata and decodes only the selected
    /// window. Scan time and identity/causal metadata remain proportional to history.
    pub fn search_recent(&self, query: &str) -> RecentSearchResults {
        self.search_recent_limited(query, usize::MAX)
    }

    /// The newest `limit` matches from the same canonical recent windows as
    /// [`Node::search_recent`]. Retains at most `limit` accumulated hits between
    /// conversations, plus the current conversation's 2,000-entry window.
    /// Metadata scans still cover retained history in every eligible conversation.
    pub fn search_recent_limited(&self, query: &str, limit: usize) -> RecentSearchResults {
        let q = query.trim().to_lowercase();
        let mut response = RecentSearchResults {
            hits: Vec::new(),
            per_conversation_limit: RECENT_HISTORY_LIMIT,
        };
        if q.is_empty() || limit == 0 {
            return response;
        }
        for conversation in self.search_conversations() {
            let history = match &conversation.source {
                SearchSource::Account(account) => {
                    self.account_history(account, RECENT_HISTORY_LIMIT)
                }
                SearchSource::Channel => {
                    self.channel_history(conversation.id, RECENT_HISTORY_LIMIT)
                }
                SearchSource::LegacyDevice(public) => self.dm_history(public, RECENT_HISTORY_LIMIT),
            };
            for entry in history.into_iter().rev() {
                if entry.recalled
                    || !String::from_utf8_lossy(&entry.text)
                        .to_lowercase()
                        .contains(&q)
                {
                    continue;
                }
                let message_id = (entry.id != EventId::new([0; 32])).then_some(entry.id);
                let reference = match &conversation.source {
                    SearchSource::Account(account) => SearchReference::Account {
                        account_id: account.clone(),
                        message_id,
                    },
                    SearchSource::Channel => SearchReference::Channel {
                        channel_id: conversation.id,
                        message_id,
                    },
                    SearchSource::LegacyDevice(public) => SearchReference::LegacyDevice {
                        device_id: public.user_id(),
                        message_id,
                    },
                };
                response.hits.push(ReferencedSearchHit {
                    hit: conversation.hit(entry.from_me, entry.who, entry.text, entry.wall_clock),
                    reference,
                    expires_at_ms: entry.expires_at_ms,
                });
            }
            if limit != usize::MAX {
                response
                    .hits
                    .sort_by_key(|hit| std::cmp::Reverse(hit.hit.wall_clock));
                response.hits.truncate(limit);
            }
        }
        response
            .hits
            .sort_by_key(|hit| std::cmp::Reverse(hit.hit.wall_clock));
        response
    }

    /// Return protected image bytes only for an exact currently eligible retained message.
    /// Hosts must authorize every reveal before calling this method.
    pub fn protected_image_protection(
        &self,
        reference: &SearchReference,
    ) -> Option<super::MessageProtection> {
        let context = self.reference_context(reference, true)?;
        let entry = context.messages.into_iter().next()?;
        let protection = entry.protection?;
        let file = entry.file?;
        if !file.media || !file.mime.starts_with("image/") || file.size > 16 * 1024 * 1024 {
            return None;
        }
        let manifest = self
            .files
            .lock()
            .expect("files lock")
            .manifest(&file.file_conv)
            .cloned()?;
        if manifest.protection() != Some(protection)
            || !crate::node::media_store::automatic_inline_image(&manifest)
        {
            return None;
        }
        Some(protection)
    }

    pub fn protected_image_content(
        &self,
        reference: &SearchReference,
    ) -> Option<(super::MessageProtection, Vec<u8>)> {
        let context = self.reference_context(reference, true)?;
        let entry = context.messages.into_iter().next()?;
        let protection = entry.protection?;
        let file = entry.file?;
        if !file.media || !file.mime.starts_with("image/") || file.size > 16 * 1024 * 1024 {
            return None;
        }
        let manifest = self
            .files
            .lock()
            .expect("files lock")
            .manifest(&file.file_conv)
            .cloned()?;
        if manifest.protection() != Some(protection)
            || !crate::node::media_store::automatic_inline_image(&manifest)
        {
            return None;
        }
        let bytes = self
            .media
            .read(file.file_conv)
            .or_else(|| self.read_file_internal(file.file_conv).ok())?;
        if bytes.len() as u64 != manifest.size()
            || crate::file::file_checksum(&bytes) != manifest.checksum()
        {
            return None;
        }
        if !crate::node::media_store::raster_signature_matches(manifest.mime(), &bytes)
            || self.protected_image_protection(reference) != Some(protection)
        {
            return None;
        }
        Some((protection, bytes))
    }

    /// Explicit retained content lookup. Hosts must authorize each reveal before calling.
    pub fn protected_message_content(
        &self,
        reference: &SearchReference,
    ) -> Option<(super::MessageProtection, Vec<u8>)> {
        let context = self.reference_context(reference, true)?;
        let id = match reference {
            SearchReference::Account { message_id, .. }
            | SearchReference::Channel { message_id, .. }
            | SearchReference::LegacyDevice { message_id, .. } => (*message_id)?,
        };
        let entry = context
            .messages
            .into_iter()
            .find(|entry| entry.id == id && !entry.recalled)?;
        if entry.file.is_some() {
            return None;
        }
        Some((entry.protection?, entry.text))
    }

    /// Resolve an exact reference against currently eligible canonical history.
    /// Returns at most 25 entries on each side, preserving canonical order. The
    /// retained projection scans identity/order metadata, then decodes only the
    /// context window. Scan time and metadata still scale with retained history.
    pub fn search_context(&self, reference: &SearchReference) -> Option<SearchContext> {
        self.reference_context(reference, false)
    }

    fn reference_context(
        &self,
        reference: &SearchReference,
        reveal: bool,
    ) -> Option<SearchContext> {
        let message_id = match reference {
            SearchReference::Account { message_id, .. }
            | SearchReference::Channel { message_id, .. }
            | SearchReference::LegacyDevice { message_id, .. } => (*message_id)?,
        };
        if message_id == EventId::new([0; 32]) {
            return None;
        }
        if let SearchReference::Account { account_id, .. } = reference
            && account_id == &self.account_id()
        {
            return self.project_history(
                account_conversation_id(account_id, account_id),
                Some(account_id),
                if reveal {
                    super::queries::HistorySelection::Protected(message_id)
                } else {
                    super::queries::HistorySelection::Context(message_id)
                },
            );
        }
        let conversation = self
            .search_conversations()
            .into_iter()
            .find(|conversation| match (reference, &conversation.source) {
                (SearchReference::Account { account_id, .. }, SearchSource::Account(account)) => {
                    account_id == account
                }
                (SearchReference::Channel { channel_id, .. }, SearchSource::Channel) => {
                    *channel_id == conversation.id
                }
                (
                    SearchReference::LegacyDevice { device_id, .. },
                    SearchSource::LegacyDevice(public),
                ) => *device_id == public.user_id(),
                _ => false,
            })?;
        let account = match &conversation.source {
            SearchSource::Account(account) => Some(account.as_str()),
            _ => None,
        };
        self.project_history(
            conversation.id,
            account,
            if reveal {
                super::queries::HistorySelection::Protected(message_id)
            } else {
                super::queries::HistorySelection::Context(message_id)
            },
        )
    }

    // Candidate eligibility is shared; canonical projection and the legacy fast
    // scan retain their separate coverage/cost contracts.
    fn search_conversations(&self) -> Vec<SearchConversation> {
        let mut conversations = Vec::new();
        let peers = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .peers();
        // Keep the current roster authoritative for each device. Signed proof
        // history lets legacy device-pair DMs remain searchable after expiry,
        // but must not revive an old human role for a live relay device.
        let live_devices: std::collections::HashSet<String> =
            peers.iter().map(|peer| peer.public.user_id()).collect();
        let mut seen_devices = live_devices.clone();
        let mut dm_peers: Vec<_> = peers
            .iter()
            .filter(|peer| !peer.post_office)
            .map(|peer| {
                (
                    peer.public.clone(),
                    peer.account_id.clone(),
                    peer.name.clone(),
                )
            })
            .collect();
        dm_peers.extend(self.historical_dm_peers().into_iter().filter_map(|proof| {
            seen_devices
                .insert(proof.user_id.clone())
                .then(|| (proof.public(), proof.account_id(), proof.name))
        }));
        for (public, account_id, name) in dm_peers {
            let conv = dm_conversation_id(&self.identity.public(), &public);
            conversations.push(SearchConversation {
                id: conv,
                target: public.user_id(),
                account_id,
                label: name,
                source: SearchSource::LegacyDevice(public),
            });
        }
        // Account-addressed DMs (the multi-device path the app uses) remain
        // searchable after every device leaves discovery. The durable message
        // index supplies account IDs; verified proofs provide names and prevent
        // relay-only accounts from becoming people-facing search results.
        let my_account = self.account.account_id();
        // None marks an account for which we only know relay devices. A human
        // device upgrades it to Some(name), even when the two share an account.
        let mut account_labels: std::collections::HashMap<String, Option<String>> =
            std::collections::HashMap::new();
        for peer in &peers {
            if let Some(acct) = &peer.account_id {
                let label = account_labels.entry(acct.clone()).or_default();
                if !peer.post_office && label.is_none() {
                    *label = Some(peer.name.clone());
                }
            }
        }
        for proof in self.historical_peer_proofs() {
            // A saved human proof must not override a newer live relay role for
            // the same device. Other offline human devices remain searchable.
            if live_devices.contains(&proof.user_id) {
                continue;
            }
            if let Some(acct) = proof.account_id() {
                let label = account_labels.entry(acct).or_default();
                if !proof.post_office && label.is_none() {
                    *label = Some(proof.name);
                }
            }
        }
        let mut accounts: std::collections::BTreeSet<String> =
            self.known_account_ids().into_iter().collect();
        accounts.extend(
            account_labels
                .iter()
                .filter_map(|(acct, label)| label.as_ref().map(|_| acct.clone())),
        );
        for acct in accounts {
            if matches!(account_labels.get(&acct), Some(None)) {
                continue;
            }
            let label = account_labels
                .get(&acct)
                .and_then(Clone::clone)
                .unwrap_or_else(|| acct.clone());
            let conv = account_conversation_id(&my_account, &acct);
            conversations.push(SearchConversation {
                id: conv,
                target: acct.clone(),
                account_id: Some(acct.clone()),
                label,
                source: SearchSource::Account(acct),
            });
        }
        for ch in self.list_channels() {
            conversations.push(SearchConversation {
                id: ch.id,
                target: hex::encode(ch.id.as_bytes()),
                account_id: None,
                label: ch.name,
                source: SearchSource::Channel,
            });
        }
        conversations
    }

    /// Scan one conversation's sent + received plaintext for a lowercased substring `q`,
    /// matching the same most-recent-`limit` window and the same `from_me`/`who`/`text`
    /// derivation as [`Node::history`] / [`Node::account_history`], while borrowing
    /// the stores rather than cloning their entry vectors. Recall
    /// tombstones keep their positions in the recent-message window while their
    /// plaintext is excluded from the returned matches.
    fn scan_conversation(
        &self,
        conv: ConversationId,
        q: &str,
        limit: usize,
        scope: SearchScope<'_>,
    ) -> Vec<ScanMatch> {
        let my_account = self.account.account_id();
        let own_devices: std::collections::HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        // Decode text from the borrowed stores, then apply the same recent-message
        // window as history before checking the query. Recalled rows occupy a
        // window slot even though search must omit their text.
        let mut lines = Vec::new();
        {
            let sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            for e in sentlog.entries_ref(&conv) {
                if super::expiry_policy::plaintext_deadline(&e.plaintext)
                    .is_some_and(|deadline| deadline <= self.expiry_now())
                {
                    continue;
                }
                let (id, body, sent_seq) = if matches!(scope, SearchScope::Account(_)) {
                    let (id, body) = decode_account_entry(&e.plaintext);
                    (Some(id), body.text, None)
                } else {
                    (None, MessageBody::decode(&e.plaintext).text, Some(e.seq))
                };
                lines.push(ScanMatch {
                    from_me: true,
                    who: "you".to_string(),
                    text: body,
                    wall_clock: e.wall_clock,
                    id,
                    sent_seq,
                });
            }
        }
        {
            let received = self.received.lock().expect("received mutex not poisoned");
            for e in received.entries_ref(&conv) {
                let envelope = super::DmEnvelope::decode(&e.plaintext);
                let inner = envelope
                    .as_ref()
                    .map_or(e.plaintext.as_slice(), |env| env.body.as_slice());
                if self.recipient_expired(inner, e.event_id, e.wall_clock) {
                    continue;
                }
                let (from_me, who, body, id) = if matches!(scope, SearchScope::Account(_)) {
                    let (id, body) = decode_account_entry(&e.plaintext);
                    let from_me = e.from == my_account;
                    let who = if from_me {
                        "you".to_string()
                    } else {
                        e.from.clone()
                    };
                    (from_me, who, body.text, id)
                } else {
                    let from_me = own_devices.contains(&e.from);
                    let who = if from_me {
                        "you".to_string()
                    } else {
                        e.from.clone()
                    };
                    (
                        from_me,
                        who,
                        MessageBody::decode(&e.plaintext).text,
                        e.event_id,
                    )
                };
                lines.push(ScanMatch {
                    from_me,
                    who,
                    text: body,
                    wall_clock: e.wall_clock,
                    id: Some(id),
                    sent_seq: None,
                });
            }
        }
        lines.sort_by_key(|line| line.wall_clock);
        if lines.len() > limit {
            lines.drain(0..lines.len() - limit);
        }
        let mut matches: Vec<_> = lines
            .into_iter()
            .filter(|line| {
                String::from_utf8_lossy(&line.text)
                    .to_lowercase()
                    .contains(q)
            })
            .collect();
        if matches.is_empty() {
            return matches;
        }
        let recalled = match scope {
            SearchScope::LegacyDm => return matches,
            SearchScope::Account(peer) => self.recalled_targets_account(peer),
            SearchScope::Channel => self.recalled_targets_channel(conv),
        };
        if recalled.is_empty() {
            return matches;
        }
        // Channel sent entries store sequence numbers, while recall markers
        // name event IDs. Reuse the history resolver only for channel matches
        // that might contain recalled text.
        let recalled_sent_sequences: std::collections::HashSet<u64> =
            if matches!(scope, SearchScope::Channel)
                && matches.iter().any(|line| line.sent_seq.is_some())
            {
                self.raw_sent_history(conv)
                    .into_iter()
                    .filter_map(|(id, entry)| recalled.contains(&id).then_some(entry.seq))
                    .collect()
            } else {
                Default::default()
            };
        matches.retain(|line| {
            !line.id.is_some_and(|id| recalled.contains(&id))
                && !line
                    .sent_seq
                    .is_some_and(|seq| recalled_sent_sequences.contains(&seq))
        });
        matches
    }
}

#[derive(Clone, Copy)]
enum SearchScope<'a> {
    LegacyDm,
    Account(&'a str),
    Channel,
}

/// One matching line from [`Node::scan_conversation`] (the fields `search` needs).
struct ScanMatch {
    from_me: bool,
    who: String,
    text: Vec<u8>,
    wall_clock: u64,
    id: Option<crate::eventlog::event::EventId>,
    sent_seq: Option<u64>,
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;

struct SearchConversation {
    id: ConversationId,
    target: String,
    account_id: Option<String>,
    label: String,
    source: SearchSource,
}

enum SearchSource {
    Account(String),
    Channel,
    LegacyDevice(PublicIdentity),
}

impl SearchConversation {
    fn hit(&self, from_me: bool, who: String, text: Vec<u8>, wall_clock: u64) -> SearchHit {
        SearchHit {
            is_channel: matches!(self.source, SearchSource::Channel),
            target: self.target.clone(),
            account_id: self.account_id.clone(),
            label: self.label.clone(),
            from_me,
            who,
            text,
            wall_clock,
        }
    }
}
