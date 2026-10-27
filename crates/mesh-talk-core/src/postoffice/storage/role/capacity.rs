//! Shared post-office threshold meaning; repository physical charge is authority.
use super::*;
#[derive(Clone, Copy)]
pub(super) struct RelayLimits {
    count: u64,
    bytes: u64,
    envelope_count: u64,
    envelope_bytes: u64,
}
impl RelayLimits {
    pub(super) fn new(
        count: u64,
        slack: u64,
        bytes: u64,
        byte_slack: u64,
    ) -> Result<Self, LogError> {
        Ok(Self {
            count,
            bytes,
            envelope_count: count
                .checked_add(slack)
                .ok_or(LogError::RepositoryCounterExhausted)?,
            envelope_bytes: bytes
                .checked_add(byte_slack)
                .ok_or(LogError::RepositoryCounterExhausted)?,
        })
    }
    pub(super) fn at_base(self, charge: DomainChargeMetadata) -> bool {
        charge.event_count <= self.count && charge.estimated_bytes <= self.bytes
    }
    pub(super) fn above_envelope(self, charge: DomainChargeMetadata) -> bool {
        charge.event_count > self.envelope_count || charge.estimated_bytes > self.envelope_bytes
    }
}
impl Default for RelayLimits {
    fn default() -> Self {
        Self::new(
            super::super::super::MAX_RELAY_EVENTS as u64,
            super::super::super::RETENTION_SLACK as u64,
            super::super::super::MAX_RELAY_BYTES as u64,
            super::super::super::RETENTION_BYTE_SLACK as u64,
        )
        .expect("fixed post-office limits fit u64")
    }
}
#[derive(Clone, Copy)]
pub(super) enum RelayBlock {
    Unavailable,
    Backpressure,
    Impossible,
}
impl RelayBlock {
    pub(super) fn error(self) -> LogError {
        match self {
            Self::Unavailable => LogError::RelayMailboxUnavailable,
            Self::Backpressure => LogError::RelayCapacityUnavailable,
            Self::Impossible => LogError::RelayEventTooLarge,
        }
    }
}
impl IndexedRelayState {
    #[cfg(test)]
    pub(crate) fn set_limits_for_test(
        &mut self,
        count: u64,
        slack: u64,
        bytes: u64,
        byte_slack: u64,
    ) -> Result<(), LogError> {
        self.limits = RelayLimits::new(count, slack, bytes, byte_slack)?;
        Ok(())
    }
    pub(super) fn drain_on(&self, c: &Connection) -> Result<bool, LogError> {
        let value: i64 = c.query_row(
            "SELECT drain_requested FROM relay_runtime WHERE singleton=1",
            [],
            |r| r.get(0),
        )?;
        match value {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(invalid("invalid relay drain authority")),
        }
    }
    pub(super) fn request_drain_on(&self, c: &Connection) -> Result<(), LogError> {
        c.execute(
            "UPDATE relay_runtime SET drain_requested=1 WHERE singleton=1 AND drain_requested=0",
            [],
        )?;
        Ok(())
    }
    pub(super) fn capacity_gate_on(
        &self,
        c: &Connection,
        cost: u64,
    ) -> Result<crate::eventlog::repository::AppendGate<RelayBlock>, LogError> {
        use crate::eventlog::repository::AppendGate;
        if self.limits.envelope_count == 0 || cost > self.limits.envelope_bytes {
            return Ok(AppendGate::Block(RelayBlock::Impossible));
        }
        let charge = self.repository.domain_charge_on(c)?;
        let count = charge
            .event_count
            .checked_add(1)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let bytes = charge
            .estimated_bytes
            .checked_add(cost)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        if count > self.limits.envelope_count || bytes > self.limits.envelope_bytes {
            self.request_drain_on(c)?;
            fault_cut(c, "drain")?;
            return Ok(AppendGate::Block(RelayBlock::Backpressure));
        }
        Ok(AppendGate::Accept)
    }
}
