//! The persistence rig and its ordinary observer controls share one implementation.
use std::collections::VecDeque;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

#[path = "cli_output.rs"]
mod cli_output;
pub(super) use cli_output::{read_cli_lines, retain_line};

#[derive(Debug, PartialEq, Eq)]
pub(super) enum HistoryObservationError {
    Response(String),
    Timeout,
    Closed,
    TooManyRows,
}

pub(super) fn wait_for_history_line(
    lines: &Receiver<String>,
    pending: &mut VecDeque<String>,
    message: &str,
    timeout: Duration,
) -> Result<String, HistoryObservationError> {
    let deadline = Instant::now() + timeout;
    if let Some(index) = pending.iter().position(|line| line.contains(message)) {
        return Ok(pending.remove(index).unwrap());
    }
    loop {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(HistoryObservationError::Timeout)?;
        let line = lines.recv_timeout(remaining).map_err(|error| match error {
            mpsc::RecvTimeoutError::Timeout => HistoryObservationError::Timeout,
            mpsc::RecvTimeoutError::Disconnected => HistoryObservationError::Closed,
        })?;
        if line == "(no history)"
            || line.starts_with("history unavailable:")
            || line.starts_with("no peer matches prefix")
            || line == "peer vanished from roster"
            || line.ends_with("peers; be more specific")
            || line.starts_with("usage: /history")
        {
            return Err(HistoryObservationError::Response(line));
        }
        // Live reception output is `from ...`; only the history command prints
        // the timestamped row. Replayed reception cannot prove this query.
        if line.starts_with('[') {
            if line.contains(message) {
                return Ok(line);
            }
            // One /history command requested at most 20 rows. Equal clocks
            // sort by event identity, so the reply may precede our sent row.
            if pending.len() == 20 {
                return Err(HistoryObservationError::TooManyRows);
            }
            pending.push_back(line);
        }
    }
}
