//! Bounded finite-window paging with host-owned cursor binding and record IDs.
use crate::{RecordId, model::MailboxCursor};

/// Source-local positions. Identity, authorization and lifecycle stay in the host cursor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeltaPosition {
    pub after: i64,
    pub revision: i64,
    pub window: Option<(i64, i64)>,
}
/// A host qualifies its complete binding before the shared window engine resumes.
/// `set_position` must preserve all identity, epoch and application metadata.
pub trait DeltaCursor: Clone {
    fn accepts(&self, current: &Self) -> bool;
    fn position(&self) -> DeltaPosition;
    fn set_position(&mut self, position: DeltaPosition);
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WindowPage<C, I> {
    pub cursor: C,
    pub ids: Vec<I>,
    pub complete: bool,
    pub reset: bool,
}
pub enum WindowSelection<C, I> {
    Unchanged(WindowPage<C, I>),
    Scan(WindowScan<C, I>),
}
pub struct WindowScan<C, I> {
    cursor: C,
    upper: i64,
    revision: i64,
    reset: bool,
    ids: Vec<I>,
    limit: usize,
    complete: bool,
}
/// Existing neutral profile names remain concrete convenience aliases.
pub type DeltaSelection = WindowSelection<MailboxCursor, RecordId>;
pub type DeltaScan = WindowScan<MailboxCursor, RecordId>;
#[derive(Debug, Eq, PartialEq)]
pub enum DeltaError {
    InvalidLimit,
    InvalidOrdinal,
    OrdinalExhausted,
}

impl<C: DeltaCursor, I> WindowSelection<C, I> {
    pub fn open(current: &C, prior: Option<&C>, limit: usize) -> Result<Self, DeltaError> {
        if limit == 0
            || i64::try_from(limit)
                .ok()
                .and_then(|n| n.checked_add(1))
                .is_none()
        {
            return Err(DeltaError::InvalidLimit);
        }
        let prior_valid = prior.filter(|cursor| cursor.accepts(current));
        let reset = prior.is_some() && prior_valid.is_none();
        let now = current.position();
        if let Some(cursor) = prior_valid.filter(|p| {
            let p = p.position();
            p.window.is_none() && p.revision == now.revision && p.after == now.after
        }) {
            return Ok(Self::Unchanged(WindowPage {
                cursor: cursor.clone(),
                ids: vec![],
                complete: true,
                reset: false,
            }));
        }
        let cursor = prior_valid.cloned().unwrap_or_else(|| {
            let mut cursor = current.clone();
            cursor.set_position(DeltaPosition {
                after: 0,
                revision: 0,
                ..now
            });
            cursor
        });
        let position = cursor.position();
        let (upper, revision) = position.window.unwrap_or((now.after, now.revision));
        let mut ids = Vec::new();
        ids.try_reserve_exact(limit)
            .map_err(|_| DeltaError::InvalidLimit)?;
        Ok(Self::Scan(WindowScan {
            cursor,
            upper,
            revision,
            reset,
            ids,
            limit,
            complete: true,
        }))
    }
}
impl<C: DeltaCursor, I> WindowScan<C, I> {
    pub fn lower(&self) -> i64 {
        self.cursor.position().after
    }
    pub fn upper(&self) -> i64 {
        self.upper
    }
    pub fn query_limit(&self) -> i64 {
        (self.limit + 1) as i64
    }
    /// Lookahead proves another page exists without acknowledging its row.
    pub fn accept_row(&mut self) -> bool {
        if self.ids.len() == self.limit {
            self.complete = false;
        }
        self.complete
    }
    pub fn push(&mut self, ordinal: i64, id: I) -> Result<(), DeltaError> {
        let position = self.cursor.position();
        if ordinal < position.after || ordinal < 0 || ordinal >= self.upper {
            return Err(DeltaError::InvalidOrdinal);
        }
        if !self.accept_row() {
            return Err(DeltaError::InvalidLimit);
        }
        self.cursor.set_position(DeltaPosition {
            after: ordinal.checked_add(1).ok_or(DeltaError::OrdinalExhausted)?,
            ..position
        });
        self.ids.push(id);
        Ok(())
    }
    pub fn defer_remaining(&mut self) {
        self.complete = false;
    }
    pub fn finish(mut self) -> WindowPage<C, I> {
        let mut position = self.cursor.position();
        if self.complete {
            position.after = self.upper;
            position.revision = self.revision;
            position.window = None;
        } else {
            position.window = Some((self.upper, self.revision));
        }
        self.cursor.set_position(position);
        WindowPage {
            cursor: self.cursor,
            ids: self.ids,
            complete: self.complete,
            reset: self.reset,
        }
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
    use crate::ScopeId;

    fn cursor(after: i64, revision: i64) -> MailboxCursor {
        MailboxCursor {
            store_uuid: [1; 16],
            scope: ScopeId::new([5; 32]),
            epoch: 2,
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
        scan.push(10, RecordId::new([1; 32])).unwrap();
        assert!(!scan.accept_row());
        let first = scan.finish();
        assert_eq!(first.cursor.window, Some((20, 4)));
        let DeltaSelection::Scan(mut resumed) =
            DeltaSelection::open(&cursor(30, 5), Some(&first.cursor), 1).unwrap()
        else {
            panic!("unfinished mailbox needs a scan")
        };
        assert_eq!((resumed.lower(), resumed.upper()), (11, 20));
        resumed.push(12, RecordId::new([2; 32])).unwrap();
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
        assert!(scan.push(-1, RecordId::new([1; 32])).is_err());
        assert_eq!(scan.lower(), 0);
        assert!(scan.push(20, RecordId::new([1; 32])).is_err());
        assert_eq!(scan.lower(), 0);
        scan.push(3, RecordId::new([1; 32])).unwrap();
        assert!(scan.push(2, RecordId::new([2; 32])).is_err());
        assert_eq!(scan.lower(), 4);
    }
}
