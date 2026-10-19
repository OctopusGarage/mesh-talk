//! Background CLI queries must never open a Windows console window.

pub(crate) fn command(program: impl AsRef<std::ffi::OsStr>) -> tokio::process::Command {
    let mut command = tokio::process::Command::new(program);
    // Redirecting stdout/stderr alone does not suppress a Windows console.
    #[cfg(target_os = "windows")]
    command.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
    command.stdin(std::process::Stdio::null());
    command
}
