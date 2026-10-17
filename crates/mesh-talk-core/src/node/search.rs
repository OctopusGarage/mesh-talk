//! Full-text search over retained DM and channel plaintext. Candidate selection
//! uses verified peer proofs and durable account history; routing remains separate.

use super::conversation::{account_conversation_id, dm_conversation_id};
use super::queries::decode_account_entry;
use super::{MessageBody, Node, SearchHit};
use crate::eventlog::event::ConversationId;

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
            for m in self.scan_conversation(conv, &q, PER_CONV, SearchScope::LegacyDm) {
                hits.push(SearchHit {
                    is_channel: false,
                    target: public.user_id(),
                    account_id: account_id.clone(),
                    label: name.clone(),
                    from_me: m.from_me,
                    who: m.who,
                    text: m.text,
                    wall_clock: m.wall_clock,
                });
            }
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
            for m in self.scan_conversation(conv, &q, PER_CONV, SearchScope::Account(&acct)) {
                hits.push(SearchHit {
                    is_channel: false,
                    target: acct.clone(),
                    account_id: Some(acct.clone()),
                    label: label.clone(),
                    from_me: m.from_me,
                    who: m.who,
                    text: m.text,
                    wall_clock: m.wall_clock,
                });
            }
        }
        for ch in self.list_channels() {
            for m in self.scan_conversation(ch.id, &q, PER_CONV, SearchScope::Channel) {
                hits.push(SearchHit {
                    is_channel: true,
                    account_id: None,
                    target: hex::encode(ch.id.as_bytes()),
                    label: ch.name.clone(),
                    from_me: m.from_me,
                    who: m.who,
                    text: m.text,
                    wall_clock: m.wall_clock,
                });
            }
        }
        hits.sort_by_key(|b| std::cmp::Reverse(b.wall_clock)); // most recent first
        hits
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
                if e.from != my_account
                    && !own_devices.contains(&e.from)
                    && self.recipient_expired(inner, e.event_id, e.wall_clock)
                {
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
