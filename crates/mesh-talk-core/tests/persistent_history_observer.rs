//! Ordinary controls for the real-process persistence rig's output observer.
#[path = "support/history_observer.rs"]
mod history_observer;

use history_observer::{
    HistoryObservationError, read_cli_lines, retain_line, wait_for_history_line,
};
use std::collections::VecDeque;
use std::sync::{Mutex, mpsc};
use std::time::Duration;

#[test]
fn history_observer_preserves_terminal_query_failure_without_reissuing_command() {
    for response in [
        "(no history)",
        "history unavailable: persistent order cursor is stale",
        "no peer matches prefix '01234567'",
        "peer vanished from roster",
    ] {
        let (sender, lines) = mpsc::channel();
        sender.send(response.to_owned()).unwrap();
        assert_eq!(
            wait_for_history_line(
                &lines,
                &mut VecDeque::new(),
                "ping-from-alice",
                Duration::from_secs(1)
            ),
            Err(HistoryObservationError::Response(response.to_owned()))
        );
    }
    let (sender, lines) = mpsc::channel();
    drop(sender);
    assert_eq!(
        wait_for_history_line(
            &lines,
            &mut VecDeque::new(),
            "ping-from-alice",
            Duration::from_secs(1)
        ),
        Err(HistoryObservationError::Closed)
    );
}

#[test]
fn history_observer_requires_query_rows_and_retains_bounded_nonmatching_output() {
    let transcript = Mutex::new(VecDeque::new());
    let mut pending = VecDeque::new();
    let (sender, lines) = mpsc::channel();
    read_cli_lines(
        std::io::Cursor::new(
            b"from bob: ping-from-alice\n[123] you: ping-from-alice\n[124] bob: pong-from-bob\n",
        ),
        "stdout",
        &transcript,
        Some(sender),
    );
    assert_eq!(
        wait_for_history_line(
            &lines,
            &mut pending,
            "ping-from-alice",
            Duration::from_secs(1)
        )
        .unwrap(),
        "[123] you: ping-from-alice"
    );
    assert_eq!(
        wait_for_history_line(
            &lines,
            &mut pending,
            "pong-from-bob",
            Duration::from_secs(1)
        )
        .unwrap(),
        "[124] bob: pong-from-bob"
    );
    assert!(
        transcript
            .lock()
            .unwrap()
            .iter()
            .any(|line| line == "stdout: from bob: ping-from-alice")
    );
    let (sender, reversed) = mpsc::channel();
    read_cli_lines(
        std::io::Cursor::new(b"[123] bob: pong-from-bob\n[123] you: ping-from-alice\n"),
        "stdout",
        &transcript,
        Some(sender),
    );
    assert_eq!(
        wait_for_history_line(
            &reversed,
            &mut pending,
            "ping-from-alice",
            Duration::from_secs(1)
        )
        .unwrap(),
        "[123] you: ping-from-alice"
    );
    assert_eq!(
        pending.front().map(String::as_str),
        Some("[123] bob: pong-from-bob")
    );
    assert_eq!(
        wait_for_history_line(
            &reversed,
            &mut pending,
            "pong-from-bob",
            Duration::from_secs(1)
        )
        .unwrap(),
        "[123] bob: pong-from-bob"
    );
    assert!(pending.is_empty());
    for index in 0..100 {
        retain_line(&transcript, format!("stderr: {index} {}", "x".repeat(2000)));
    }
    let retained = transcript.lock().unwrap();
    assert_eq!(retained.len(), 64);
    assert!(retained.front().unwrap().starts_with("stderr: 36 "));
    assert!(retained.iter().all(|line| line.len() <= 1024));
}
