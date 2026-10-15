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
    /// Scans the sent + received stores BY REFERENCE — it never clones a full
    /// conversation Vec, only the plaintext of the lines that actually match — and skips
    /// the seq->id log scan `history` does (search results carry no event id), so a
    /// keystroke no longer copies every plaintext in every conversation.
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
        let mut seen_devices: std::collections::HashSet<String> =
            peers.iter().map(|peer| peer.public.user_id()).collect();
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
            for m in self.scan_conversation(conv, &q, PER_CONV, false) {
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
            for m in self.scan_conversation(conv, &q, PER_CONV, true) {
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
            for m in self.scan_conversation(ch.id, &q, PER_CONV, false) {
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
    /// derivation as [`Node::history`] / [`Node::account_history`], but borrowing the
    /// stores (no full-Vec clone) and cloning only the matching lines. `account` selects
    /// the account-conversation envelope decode + sender derivation.
    fn scan_conversation(
        &self,
        conv: ConversationId,
        q: &str,
        limit: usize,
        account: bool,
    ) -> Vec<ScanMatch> {
        let my_account = self.account.account_id();
        let own_devices: std::collections::HashSet<_> = self
            .linked_devices()
            .into_iter()
            .map(|device| device.public.user_id())
            .collect();
        // Materialize (wall_clock, from_me, who, text) for the window, borrowing the
        // stores; only the decoded body bytes of kept lines are owned. Mirror `history`'s
        // ordering (wall_clock, then a sent-before-received tiebreak is irrelevant to a
        // substring scan) and most-recent-`limit` truncation.
        let mut lines: Vec<(u64, bool, String, Vec<u8>)> = Vec::new();
        {
            let sentlog = self.sentlog.lock().expect("sentlog mutex not poisoned");
            for e in sentlog.entries_ref(&conv) {
                let (from_me, who, body) = if account {
                    let (_, body) = decode_account_entry(&e.plaintext);
                    (true, "you".to_string(), body.text)
                } else {
                    (
                        true,
                        "you".to_string(),
                        MessageBody::decode(&e.plaintext).text,
                    )
                };
                lines.push((e.wall_clock, from_me, who, body));
            }
        }
        {
            let received = self.received.lock().expect("received mutex not poisoned");
            for e in received.entries_ref(&conv) {
                let (from_me, who, body) = if account {
                    let (_, body) = decode_account_entry(&e.plaintext);
                    let from_me = e.from == my_account;
                    let who = if from_me {
                        "you".to_string()
                    } else {
                        e.from.clone()
                    };
                    (from_me, who, body.text)
                } else {
                    let from_me = own_devices.contains(&e.from);
                    let who = if from_me {
                        "you".to_string()
                    } else {
                        e.from.clone()
                    };
                    (from_me, who, MessageBody::decode(&e.plaintext).text)
                };
                lines.push((e.wall_clock, from_me, who, body));
            }
        }
        lines.sort_by_key(|l| l.0);
        if lines.len() > limit {
            lines.drain(0..lines.len() - limit);
        }
        lines
            .into_iter()
            .filter(|(_, _, _, text)| String::from_utf8_lossy(text).to_lowercase().contains(q))
            .map(|(wall_clock, from_me, who, text)| ScanMatch {
                from_me,
                who,
                text,
                wall_clock,
            })
            .collect()
    }
}

/// One matching line from [`Node::scan_conversation`] (the fields `search` needs).
struct ScanMatch {
    from_me: bool,
    who: String,
    text: Vec<u8>,
    wall_clock: u64,
}

#[cfg(test)]
#[path = "search_tests.rs"]
mod tests;
