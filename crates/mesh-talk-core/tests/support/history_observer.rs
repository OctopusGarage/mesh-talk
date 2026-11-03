//! The persistence rig and its ordinary observer controls share one implementation.
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::sync::Mutex;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

pub(super) fn retain_line(transcript: &Mutex<VecDeque<String>>, line: String) {
    let mut transcript = transcript.lock().unwrap();
    if transcript.len() == 64 {
        transcript.pop_front();
    }
    transcript.push_back(line.chars().take(1024).collect());
}

pub(super) fn read_cli_lines(
    stream: impl Read,
    name: &str,
    transcript: &Mutex<VecDeque<String>>,
    output: Option<mpsc::Sender<String>>,
) {
    for line in BufReader::new(stream).lines() {
        match line {
            Ok(line) => {
                retain_line(transcript, format!("{name}: {line}"));
                if output
                    .as_ref()
                    .is_some_and(|output| output.send(line).is_err())
                {
                    break;
                }
            }
            Err(error) => {
                retain_line(transcript, format!("{name} read error: {error}"));
                return;
            }
        }
    }
    retain_line(transcript, format!("{name} EOF"));
}

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
