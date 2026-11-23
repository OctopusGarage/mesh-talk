//! Bounded delta selection over a finite mailbox prefix.

use crate::rpc::{MailboxCursor, MailboxDeltaPage};
use mesh_replication::legacy_event::EventId;

/// A page's authorization and SQL transaction are owned by the caller.
pub enum DeltaSelection {
    Unchanged(MailboxDeltaPage),
    Scan(DeltaScan),
}

pub struct DeltaScan {
    cursor: MailboxCursor,
    upper: i64,
    revision: i64,
    reset: bool,
    ids: Vec<EventId>,
    limit: usize,
    complete: bool,
}

#[derive(Debug, Eq, PartialEq)]
pub enum DeltaError {
    InvalidLimit,
    OrdinalExhausted,
}

impl DeltaSelection {
    pub fn open(
        current: &MailboxCursor,
        prior: Option<&MailboxCursor>,
        limit: usize,
    ) -> Result<Self, DeltaError> {
        if limit == 0 || limit > 1500 {
            return Err(DeltaError::InvalidLimit);
        }
        let prior_valid = prior.filter(|cursor| cursor.accepts(current));
        let reset = prior.is_some() && prior_valid.is_none();
        if let Some(cursor) =
            prior_valid.filter(|p| p.window.is_none() && p.revision == current.revision)
        {
            return Ok(Self::Unchanged(MailboxDeltaPage {
                cursor: cursor.clone(),
                ids: vec![],
                complete: true,
                reset: false,
            }));
        }
        let cursor = prior_valid.cloned().unwrap_or_else(|| MailboxCursor {
            after: 0,
            revision: 0,
            ..current.clone()
        });
        let (upper, revision) = cursor.window.unwrap_or((current.after, current.revision));
        Ok(Self::Scan(DeltaScan {
            cursor,
            upper,
            revision,
            reset,
            ids: Vec::with_capacity(limit),
            limit,
            complete: true,
        }))
    }
}

impl DeltaScan {
    pub fn lower(&self) -> i64 {
        self.cursor.after
    }

    pub fn upper(&self) -> i64 {
        self.upper
    }

    pub fn query_limit(&self) -> i64 {
        (self.limit + 1) as i64
    }

    /// Test the next row before decoding its ID. A lookahead row proves that
    /// another page exists and remains unconsumed.
    pub fn accept_row(&mut self) -> bool {
        if self.ids.len() == self.limit {
            self.complete = false;
        }
        self.complete
    }

    pub fn push(&mut self, ordinal: i64, id: EventId) -> Result<(), DeltaError> {
        if !self.accept_row() {
            return Err(DeltaError::InvalidLimit);
        }
        self.cursor.after = ordinal.checked_add(1).ok_or(DeltaError::OrdinalExhausted)?;
        self.ids.push(id);
        Ok(())
    }

    pub fn finish(mut self) -> MailboxDeltaPage {
        if self.complete {
            self.cursor.after = self.upper;
            self.cursor.revision = self.revision;
            self.cursor.window = None;
        } else {
            self.cursor.window = Some((self.upper, self.revision));
        }
        MailboxDeltaPage {
            cursor: self.cursor,
            ids: self.ids,
            complete: self.complete,
            reset: self.reset,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mesh_replication::legacy_event::ConversationId;

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
}
