//! Native observer controls, compiled with rustc, never linked into the application.
#![cfg_attr(not(console_child), windows_subsystem = "windows")]

#[cfg(console_child)]
#[link(name = "kernel32")]
unsafe extern "system" {
    fn GetConsoleWindow() -> *mut std::ffi::c_void;
    fn SetConsoleTitleW(title: *const u16) -> i32;
}

#[cfg(console_child)]
fn main() {
    let title: Vec<u16> = "mesh-talk-console-fixture\0".encode_utf16().collect();
    // SAFETY: the string is NUL-terminated and both functions have no ownership transfer.
    let present = unsafe {
        SetConsoleTitleW(title.as_ptr());
        !GetConsoleWindow().is_null()
    };
    println!("{present}");
    std::thread::sleep(std::time::Duration::from_millis(1000));
}

#[cfg(not(console_child))]
fn main() {
    use std::os::windows::process::CommandExt;
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    assert_eq!(
        args.len(),
        3,
        "Usage: console-launcher <child> <mode> <report>"
    );
    let mut command = std::process::Command::new(&args[0]);
    if args[1] == "negative" {
        command.creation_flags(0x08000000); // CREATE_NO_WINDOW, control only.
    } else {
        assert_eq!(args[1], "positive");
    }
    let output = command.output().expect("launch console control");
    assert!(output.status.success());
    std::fs::write(&args[2], output.stdout).expect("write console-presence report");
}
