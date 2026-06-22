//! Reaction model: the `React` event payload + aggregation. Pure — the `Node` seals
//! the payload with the conversation crypto and folds opened reactions on read.

use crate::eventlog::event::EventId;
use bincode::Options;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// The plaintext payload of a `React` event: which message, which emoji, and whether
/// this is an add or a remove (toggle off). Sealed like a message before it ships.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReactionPayload {
    pub target: EventId,
    pub emoji: String,
    pub remove: bool,
}

impl ReactionPayload {
    pub fn encode(&self) -> Vec<u8> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .serialize(self)
            .expect("reaction payload serializes")
    }
    pub fn decode(bytes: &[u8]) -> Option<Self> {
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()
    }
}

/// Aggregated reactions for one `(target message, emoji)`: the set of author user-ids
/// that currently react. The UI derives count + whether the local user is among them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReactionView {
    pub target: String, // hex EventId
    pub emoji: String,
    pub who: Vec<String>, // author user-ids, sorted
}

/// Aggregate reactions into per-`(target, emoji)` author sets. Each author's state is
/// resolved INDEPENDENTLY of input order by their latest action: the entry with the
/// greatest `wall_clock` wins, and on an exact tie a `remove` beats an `add` (so a
/// toggle never sticks "on" by a hash coin-flip). This makes aggregation a pure
/// max-reduction — it doesn't matter whether a reaction arrived via the log (in
/// `(lamport, id)` order) or was merged in from our own out-of-band record. Input is
/// `(author_user_id, wall_clock_ms, payload)`; output is sorted by `(target, emoji)`,
/// each `who` sorted by author.
pub fn aggregate(reactions: &[(String, u64, ReactionPayload)]) -> Vec<ReactionView> {
    // (target hex, emoji, author) -> (latest wall_clock, currently-reacting?)
    let mut latest: BTreeMap<(String, String, String), (u64, bool)> = BTreeMap::new();
    for (author, wall, p) in reactions {
        let key = (
            hex::encode(p.target.as_bytes()),
            p.emoji.clone(),
            author.clone(),
        );
        let reacting = !p.remove;
        match latest.get_mut(&key) {
            // Newer wins; on an exact wall-clock tie a remove (reacting == false) wins.
            Some(cur) if *wall > cur.0 || (*wall == cur.0 && !reacting) => *cur = (*wall, reacting),
            Some(_) => {}
            None => {
                latest.insert(key, (*wall, reacting));
            }
        }
    }
    // Collect authors still reacting. Iterating the BTreeMap yields (target, emoji, author)
    // in sorted order, so each `who` comes out author-sorted and views target/emoji-sorted.
    let mut sets: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    for ((target, emoji, author), (_, reacting)) in latest {
        if reacting {
            sets.entry((target, emoji)).or_default().push(author);
        }
    }
    sets.into_iter()
        .map(|((target, emoji), who)| ReactionView { target, emoji, who })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target(n: u8) -> EventId {
        EventId::new([n; 32])
    }
    fn react(
        author: &str,
        ts: u64,
        t: u8,
        emoji: &str,
        remove: bool,
    ) -> (String, u64, ReactionPayload) {
        (
            author.to_string(),
            ts,
            ReactionPayload {
                target: target(t),
                emoji: emoji.into(),
                remove,
            },
        )
    }

    #[test]
    fn payload_round_trips_and_rejects_trailing() {
        let p = ReactionPayload {
            target: target(1),
            emoji: "👍".into(),
            remove: false,
        };
        assert_eq!(ReactionPayload::decode(&p.encode()), Some(p.clone()));
        let mut junk = p.encode();
        junk.push(0xAB);
        assert_eq!(ReactionPayload::decode(&junk), None);
    }

    #[test]
    fn aggregates_distinct_authors_and_emojis() {
        let views = aggregate(&[
            react("alice", 1, 1, "👍", false),
            react("bob", 1, 1, "👍", false),
            react("bob", 1, 1, "🎉", false),
        ]);
        let thumbs = views.iter().find(|v| v.emoji == "👍").unwrap();
        assert_eq!(thumbs.who, vec!["alice".to_string(), "bob".to_string()]);
        assert!(views
            .iter()
            .any(|v| v.emoji == "🎉" && v.who == vec!["bob".to_string()]));
    }

    #[test]
    fn remove_toggles_a_reaction_off_and_drops_empty() {
        let views = aggregate(&[
            react("alice", 1, 1, "👍", false),
            react("alice", 2, 1, "👍", true), // alice un-reacts (later)
        ]);
        assert!(views.is_empty());
    }

    #[test]
    fn remove_only_affects_that_author() {
        let views = aggregate(&[
            react("alice", 1, 1, "👍", false),
            react("bob", 1, 1, "👍", false),
            react("alice", 2, 1, "👍", true),
        ]);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].who, vec!["bob".to_string()]);
    }

    #[test]
    fn latest_action_wins_regardless_of_input_order() {
        // The remove is newer (ts 2 > 1); the result is the same whatever order the
        // slice is folded in — aggregation is a max-reduction, not an ordered fold.
        let add = react("alice", 1, 1, "👍", false);
        let rem = react("alice", 2, 1, "👍", true);
        assert!(aggregate(&[add.clone(), rem.clone()]).is_empty()); // add then remove
        assert!(aggregate(&[rem, add]).is_empty()); // reordered — still un-reacted
    }

    #[test]
    fn equal_timestamp_tie_breaks_to_remove() {
        // An add and a remove at the SAME wall-clock resolve deterministically to remove.
        let add = react("alice", 5, 1, "👍", false);
        let rem = react("alice", 5, 1, "👍", true);
        assert!(aggregate(&[add.clone(), rem.clone()]).is_empty());
        assert!(aggregate(&[rem, add]).is_empty());
    }

    #[test]
    fn a_newer_add_re_reacts_after_a_remove() {
        let views = aggregate(&[
            react("alice", 1, 1, "👍", true),  // earlier remove
            react("alice", 2, 1, "👍", false), // later add → reacting
        ]);
        assert_eq!(views.len(), 1);
        assert_eq!(views[0].who, vec!["alice".to_string()]);
    }
}
