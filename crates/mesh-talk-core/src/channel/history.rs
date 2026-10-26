//! Owner-controlled disclosure of a bounded copy of pre-invitation text history.
use serde::{Deserialize, Serialize};

/// Missing policy on existing channels means no pre-join disclosure. Policies are
/// fixed presets so every holder enforces the same limits.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HistoryPolicy {
    #[default]
    Off,
    LastTen,
    LastThreeDays,
    LastTenWithinThreeDays,
}

impl HistoryPolicy {
    pub(crate) fn count(self) -> usize {
        match self {
            Self::Off => 0,
            Self::LastTen | Self::LastTenWithinThreeDays => 10,
            Self::LastThreeDays => 100,
        }
    }
    pub(crate) fn permits_time(self, authored: u64, invitation: u64) -> bool {
        authored <= invitation
            && (!matches!(self, Self::LastThreeDays | Self::LastTenWithinThreeDays)
                || authored >= invitation.saturating_sub(3 * 24 * 60 * 60 * 1000))
    }
}
