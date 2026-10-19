//! Deterministic traversal of the validated event DAG. Lamport values are signed
//! input, not proof of ancestry: only retained parent edges constrain this order.
use crate::eventlog::event::{Event, EventId};
use std::collections::{BTreeSet, HashMap};

/// `events` must come from a validated conversation log. Plaintext-only archive
/// rows have no signed ancestry and are independent roots with a time/id fallback.
/// Including control events preserves transitive message ancestry through them.
pub(crate) fn ordered_ids(
    events: &[&Event],
    archive: &[(EventId, u64)],
    logical: bool,
) -> Vec<EventId> {
    let mut clocks: HashMap<EventId, u64> = archive.iter().copied().collect();
    for event in events {
        clocks.insert(
            event.id,
            if logical {
                event.lamport
            } else {
                event.wall_clock
            },
        );
    }
    let mut degrees: HashMap<EventId, usize> = clocks.keys().map(|id| (*id, 0)).collect();
    let mut children: HashMap<EventId, Vec<EventId>> = HashMap::new();
    for event in events {
        for parent in &event.parents {
            if clocks.contains_key(parent) {
                *degrees.get_mut(&event.id).expect("event indexed") += 1;
                children.entry(*parent).or_default().push(event.id);
            }
        }
    }
    let mut ready: BTreeSet<_> = degrees
        .iter()
        .filter(|(_, degree)| **degree == 0)
        .map(|(id, _)| (clocks[id], *id))
        .collect();
    let mut ordered = Vec::with_capacity(clocks.len());
    while let Some((_, id)) = ready.pop_first() {
        ordered.push(id);
        if let Some(next) = children.get(&id) {
            for child in next {
                let degree = degrees.get_mut(child).expect("child indexed");
                *degree -= 1;
                if *degree == 0 {
                    ready.insert((clocks[child], *child));
                }
            }
        }
    }
    // A validated content-addressed DAG cannot cycle. Fail closed for corrupted
    // in-memory input rather than inventing dependencies for archive reply IDs.
    ordered
}
