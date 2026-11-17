//! Bounded output capture shared by real CLI process fixtures.
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::sync::{Mutex, mpsc};

pub(crate) fn retain_line(transcript: &Mutex<VecDeque<String>>, line: String) {
    let mut transcript = transcript.lock().unwrap();
    if transcript.len() == 64 {
        transcript.pop_front();
    }
    transcript.push_back(line.chars().take(1024).collect());
}

pub(crate) fn read_cli_lines(
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
