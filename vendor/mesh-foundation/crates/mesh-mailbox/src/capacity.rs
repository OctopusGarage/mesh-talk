//! Count and byte policy shared by spool admission and retention.
//! Callers obtain authoritative charge and apply the result in the same transaction.

#[derive(Clone, Copy)]
pub struct CapacityLimits {
    count: u64,
    bytes: u64,
    envelope_count: u64,
    envelope_bytes: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CapacityOverflow;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Admission {
    Accept,
    Backpressure,
    Impossible,
}

impl CapacityLimits {
    pub fn new(
        count: u64,
        count_slack: u64,
        bytes: u64,
        byte_slack: u64,
    ) -> Result<Self, CapacityOverflow> {
        Ok(Self {
            count,
            bytes,
            envelope_count: count.checked_add(count_slack).ok_or(CapacityOverflow)?,
            envelope_bytes: bytes.checked_add(byte_slack).ok_or(CapacityOverflow)?,
        })
    }

    pub fn at_base(self, count: u64, bytes: u64) -> bool {
        count <= self.count && bytes <= self.bytes
    }

    pub fn above_envelope(self, count: u64, bytes: u64) -> bool {
        count > self.envelope_count || bytes > self.envelope_bytes
    }

    pub fn allows_event(self, cost: u64) -> bool {
        self.envelope_count > 0 && cost <= self.envelope_bytes
    }

    pub fn admit(self, count: u64, bytes: u64, cost: u64) -> Result<Admission, CapacityOverflow> {
        if !self.allows_event(cost) {
            return Ok(Admission::Impossible);
        }
        let next_count = count.checked_add(1).ok_or(CapacityOverflow)?;
        let next_bytes = bytes.checked_add(cost).ok_or(CapacityOverflow)?;
        if self.above_envelope(next_count, next_bytes) {
            Ok(Admission::Backpressure)
        } else {
            Ok(Admission::Accept)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admits_through_slack_and_distinguishes_drain_from_impossible() {
        let limits = CapacityLimits::new(1, 1, 10, 5).unwrap();
        assert!(limits.at_base(1, 10));
        assert!(!limits.at_base(2, 10));
        assert!(!limits.at_base(1, 11));
        assert!(!limits.above_envelope(2, 15));
        assert_eq!(limits.admit(1, 10, 5), Ok(Admission::Accept));
        assert_eq!(limits.admit(1, 10, 6), Ok(Admission::Backpressure));
        assert_eq!(limits.admit(2, 0, 1), Ok(Admission::Backpressure));
        assert_eq!(limits.admit(0, 0, 16), Ok(Admission::Impossible));
        assert_eq!(
            CapacityLimits::new(0, 0, 10, 5).unwrap().admit(0, 0, 1),
            Ok(Admission::Impossible)
        );
    }

    #[test]
    fn admits_the_only_event_at_the_exact_byte_limit() {
        let limits = CapacityLimits::new(1, 0, 10, 0).unwrap();
        assert_eq!(limits.admit(0, 0, 10), Ok(Admission::Accept));
        assert_eq!(limits.admit(1, 0, 10), Ok(Admission::Backpressure));
    }

    #[test]
    fn rejects_arithmetic_overflow_without_a_capacity_decision() {
        assert!(CapacityLimits::new(u64::MAX, 1, 0, 0).is_err());
        let limits = CapacityLimits::new(u64::MAX, 0, u64::MAX, 0).unwrap();
        assert_eq!(limits.admit(u64::MAX, 0, 0), Err(CapacityOverflow));
        assert_eq!(limits.admit(0, u64::MAX, 1), Err(CapacityOverflow));
    }
}
