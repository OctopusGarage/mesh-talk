//! Shared causal traversal for channel projections and retained history.
pub(super) use crate::eventlog::order::ordered_ids;

#[cfg(test)]
use crate::eventlog::event::{Event, EventId};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::eventlog::event::{ConversationId, EventKind};
    use crate::identity::device::DeviceIdentity;

    #[test]
    fn group_reception_permutations_keep_transitive_ancestry_and_concurrent_ties() {
        let author = DeviceIdentity::generate();
        let conv = ConversationId::new([8; 32]);
        let first = Event::new(
            &author,
            conv,
            1,
            vec![],
            100,
            900,
            EventKind::Message,
            vec![],
        );
        let control = Event::new(
            &author,
            conv,
            2,
            vec![first.id],
            0,
            0,
            EventKind::KeyRotation,
            vec![],
        );
        let reply = Event::new(
            &author,
            conv,
            3,
            vec![control.id],
            0,
            1,
            EventKind::Message,
            vec![],
        );
        let concurrent = Event::new(&author, conv, 4, vec![], 0, 900, EventKind::Message, vec![]);
        let expected = ordered_ids(&[&first, &control, &reply, &concurrent], &[], false);
        for events in [
            vec![&reply, &concurrent, &control, &first],
            vec![&control, &first, &concurrent, &reply],
        ] {
            assert_eq!(ordered_ids(&events, &[], false), expected);
        }
        let position = |id| expected.iter().position(|entry| *entry == id).unwrap();
        assert!(position(first.id) < position(control.id));
        assert!(position(control.id) < position(reply.id));
    }

    #[test]
    fn group_reception_plaintext_archive_has_explicit_stable_fallback() {
        let older = EventId::new([2; 32]);
        let newer = EventId::new([1; 32]);
        let same_time = EventId::new([3; 32]);
        assert_eq!(
            ordered_ids(&[], &[(newer, 5), (same_time, 5), (older, 1)], false),
            vec![older, newer, same_time]
        );
    }
}
