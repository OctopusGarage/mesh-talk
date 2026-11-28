//! Shared post-office threshold meaning; repository physical charge is authority.
use super::*;
use mesh_talk_protocol::mailbox::capacity::{Admission, CapacityLimits};

#[derive(Clone, Copy)]
pub(super) struct RelayLimits(CapacityLimits);

impl RelayLimits {
    fn new(count: u64, slack: u64, bytes: u64, byte_slack: u64) -> Result<Self, LogError> {
        CapacityLimits::new(count, slack, bytes, byte_slack)
            .map(Self)
            .map_err(|_| LogError::RepositoryCounterExhausted)
    }

    pub(super) fn at_base(self, charge: DomainChargeMetadata) -> bool {
        self.0.at_base(charge.event_count, charge.estimated_bytes)
    }

    pub(super) fn above_envelope(self, charge: DomainChargeMetadata) -> bool {
        self.0
            .above_envelope(charge.event_count, charge.estimated_bytes)
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
        let value: i64 = c
            .prepare_cached("SELECT drain_requested FROM relay_runtime WHERE singleton=1")?
            .query_row([], |r| r.get(0))?;
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
        if !self.limits.0.allows_event(cost) {
            return Ok(AppendGate::Block(RelayBlock::Impossible));
        }
        let charge = self.repository.domain_charge_on(c)?;
        match self
            .limits
            .0
            .admit(charge.event_count, charge.estimated_bytes, cost)
            .map_err(|_| LogError::RepositoryCounterExhausted)?
        {
            Admission::Accept => Ok(AppendGate::Accept),
            Admission::Backpressure => {
                self.request_drain_on(c)?;
                fault_cut(c, "drain")?;
                Ok(AppendGate::Block(RelayBlock::Backpressure))
            }
            Admission::Impossible => Ok(AppendGate::Block(RelayBlock::Impossible)),
        }
    }
}
