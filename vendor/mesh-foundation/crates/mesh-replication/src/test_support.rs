//! Opt-in adapter probes for development, under `test-observability`.
//!
//! Checks only the supplied finite snapshot samples. It is not an authorization,
//! concurrency, durability or exhaustive integrity proof. Hosts must additionally
//! test transactional failure/reopen and view retirement in their own stores.
use crate::{
    EventId,
    range::{Prefix, RangeIndex, RangeRead, Summary},
};
#[derive(Debug)]
pub enum ReaderProbeError<E> {
    Read(E),
    Summary {
        prefix: Prefix,
        expected: Summary,
        actual: Summary,
    },
    Membership {
        id: EventId,
        expected: bool,
        actual: bool,
    },
    Revision {
        before: u64,
        after: u64,
    },
}
/// Compare a pinned adapter with the canonical mathematical oracle, using only
/// caller-selected prefixes and ID probes. Work is bounded by those slices.
/// Reader revision is a host token, not necessarily the oracle's insert count.
pub fn check_reader<R: RangeRead + ?Sized>(
    reader: &R,
    expected: &RangeIndex,
    prefixes: &[Prefix],
    ids: &[EventId],
) -> Result<(), ReaderProbeError<R::Error>> {
    let before = reader.revision();
    for prefix in prefixes {
        let actual = reader.summary(*prefix).map_err(ReaderProbeError::Read)?;
        let expected = expected.summary(*prefix);
        if actual != expected {
            return Err(ReaderProbeError::Summary {
                prefix: *prefix,
                expected,
                actual,
            });
        }
    }
    for id in ids {
        let actual = reader.contains(id).map_err(ReaderProbeError::Read)?;
        let expected = expected.contains(id);
        if actual != expected {
            return Err(ReaderProbeError::Membership {
                id: *id,
                expected,
                actual,
            });
        }
    }
    let after = reader.revision();
    if before != after {
        return Err(ReaderProbeError::Revision { before, after });
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    struct Broken {
        mode: u8,
        revision: std::cell::Cell<u64>,
    }
    impl RangeRead for Broken {
        type Error = &'static str;
        fn revision(&self) -> u64 {
            let n = self.revision.get();
            if self.mode == 3 {
                self.revision.set(n + 1);
            }
            n
        }
        fn summary(&self, _: Prefix) -> Result<Summary, Self::Error> {
            match self.mode {
                0 => Ok(Summary::leaf([7; 32])),
                2 => Err("read failed"),
                _ => Ok(Summary::empty()),
            }
        }
        fn contains(&self, _: &EventId) -> Result<bool, Self::Error> {
            Ok(self.mode == 1)
        }
    }
    #[test]
    fn probes_detect_false_summaries_membership_read_errors_and_revision_drift() {
        let oracle = RangeIndex::default();
        for mode in 0..4 {
            let reader = Broken {
                mode,
                revision: std::cell::Cell::new(0),
            };
            let error = check_reader(&reader, &oracle, &[Prefix::root()], &[[7; 32]]).unwrap_err();
            assert!(matches!(
                (mode, error),
                (0, ReaderProbeError::Summary { .. })
                    | (1, ReaderProbeError::Membership { .. })
                    | (2, ReaderProbeError::Read("read failed"))
                    | (3, ReaderProbeError::Revision { .. })
            ));
        }
        check_reader(&oracle, &oracle, &[Prefix::root()], &[[7; 32]]).unwrap();
    }
}
