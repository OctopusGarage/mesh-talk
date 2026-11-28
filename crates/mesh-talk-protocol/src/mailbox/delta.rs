//! Historical cursor binding over the SDK's generic finite-window engine.
use crate::legacy_event::EventId;
use crate::mailbox::rpc::{MailboxCursor, MailboxDeltaPage};
pub use mesh_mailbox::delta::DeltaError;
use mesh_mailbox::delta::{DeltaCursor, DeltaPosition, WindowSelection};
pub type DeltaScan = mesh_mailbox::delta::WindowScan<MailboxCursor, EventId>;

pub enum DeltaSelection {
    Unchanged(MailboxDeltaPage),
    Scan(DeltaScan),
}
impl DeltaSelection {
    pub fn open(
        current: &MailboxCursor,
        prior: Option<&MailboxCursor>,
        limit: usize,
    ) -> Result<Self, DeltaError> {
        // This historical profile's allowance and first-seen binding stay here.
        if limit == 0 || limit > 1500 {
            return Err(DeltaError::InvalidLimit);
        }
        Ok(match WindowSelection::open(current, prior, limit)? {
            WindowSelection::Unchanged(page) => Self::Unchanged(page),
            WindowSelection::Scan(scan) => Self::Scan(scan),
        })
    }
}
impl DeltaCursor for MailboxCursor {
    fn accepts(&self, current: &Self) -> bool {
        MailboxCursor::accepts(self, current)
    }
    fn position(&self) -> DeltaPosition {
        DeltaPosition {
            after: self.after,
            revision: self.revision,
            window: self.window,
        }
    }
    fn set_position(&mut self, position: DeltaPosition) {
        self.after = position.after;
        self.revision = position.revision;
        self.window = position.window;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::legacy_event::ConversationId;

    fn cursor(after: i64, revision: i64) -> MailboxCursor {
        MailboxCursor {
            store_uuid: [1; 16],
            conversation: ConversationId::new([5; 32]),
            epoch: 2,
            first_seen: Some(100),
            revision,
            after,
            window: None,
        }
    }

    #[test]
    fn continuation_retains_original_upper_cutoff_after_append() {
        let current = cursor(20, 4);
        let DeltaSelection::Scan(mut scan) = DeltaSelection::open(&current, None, 1).unwrap()
        else {
            panic!("new mailbox needs a scan")
        };
        assert_eq!((scan.lower(), scan.upper(), scan.query_limit()), (0, 20, 2));
        scan.push(10, EventId::new([1; 32])).unwrap();
        assert!(!scan.accept_row());
        let first = scan.finish();
        assert_eq!(first.cursor.window, Some((20, 4)));
        let DeltaSelection::Scan(mut resumed) =
            DeltaSelection::open(&cursor(30, 5), Some(&first.cursor), 1).unwrap()
        else {
            panic!("unfinished mailbox needs a scan")
        };
        assert_eq!((resumed.lower(), resumed.upper()), (11, 20));
        resumed.push(12, EventId::new([2; 32])).unwrap();
        let final_page = resumed.finish();
        assert!(final_page.complete);
        assert_eq!(final_page.cursor, current);
    }

    #[test]
    fn unchanged_and_destructive_epoch_reset_are_distinct() {
        let prior = cursor(20, 4);
        assert!(matches!(
            DeltaSelection::open(&prior, Some(&prior), 1),
            Ok(DeltaSelection::Unchanged(_))
        ));
        let mut current = prior.clone();
        current.epoch += 1;
        let DeltaSelection::Scan(scan) = DeltaSelection::open(&current, Some(&prior), 1).unwrap()
        else {
            panic!("destructive epoch forces scan")
        };
        assert_eq!(scan.lower(), 0);
        let reset = scan.finish();
        assert!(reset.reset && reset.complete);
        assert_eq!(reset.cursor, current);
    }
    #[test]
    fn revision_match_does_not_skip_an_unread_prefix() {
        let current = cursor(20, 4);
        let prior = cursor(0, 4);
        assert!(matches!(
            DeltaSelection::open(&current, Some(&prior), 2),
            Ok(DeltaSelection::Scan(_))
        ));
    }
    #[test]
    fn rejected_ordinals_leave_the_scan_unchanged() {
        let DeltaSelection::Scan(mut scan) = DeltaSelection::open(&cursor(20, 4), None, 2).unwrap()
        else {
            panic!("scan")
        };
        assert!(scan.push(-1, EventId::new([1; 32])).is_err());
        assert_eq!(scan.lower(), 0);
        assert!(scan.push(20, EventId::new([1; 32])).is_err());
        assert_eq!(scan.lower(), 0);
        scan.push(3, EventId::new([1; 32])).unwrap();
        assert!(scan.push(2, EventId::new([2; 32])).is_err());
        assert_eq!(scan.lower(), 4);
    }
}

#[cfg(test)]
mod compatibility_binding_tests {
    use super::*;
    use crate::legacy_event::ConversationId;
    #[test]
    fn historical_first_seen_change_resets_the_shared_window() {
        let prior = MailboxCursor {
            store_uuid: [1; 16],
            conversation: ConversationId::new([2; 32]),
            epoch: 1,
            first_seen: Some(10),
            revision: 4,
            after: 20,
            window: None,
        };
        let current = MailboxCursor {
            first_seen: Some(11),
            ..prior.clone()
        };
        let DeltaSelection::Scan(scan) = DeltaSelection::open(&current, Some(&prior), 1).unwrap()
        else {
            panic!("binding change must reset")
        };
        let page = scan.finish();
        assert!(page.reset);
        assert_eq!(page.cursor.first_seen, Some(11));
        assert_eq!(page.cursor.store_uuid, current.store_uuid);
        assert_eq!(page.cursor.conversation, current.conversation);
        assert!(DeltaSelection::open(&current, None, 1501).is_err());
    }
}
