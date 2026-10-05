use std::io::Write;
use std::process::{Child, ChildStdin};
use std::time::{Duration, Instant};

/// Give normal nodes a bounded opportunity to drop their data-directory lock.
/// A relay has no REPL, so it reaches the owned-child kill fallback. Never delete
/// lock files on the child's behalf: a restart must expose bad process cleanup.
pub fn stop_cli(child: &mut Child, stdin: &mut ChildStdin) {
    let _ = writeln!(stdin, "/quit");
    let _ = stdin.flush();
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => std::thread::sleep(Duration::from_millis(25)),
            Err(_) => break,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}
