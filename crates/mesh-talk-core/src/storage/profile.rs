//! Exclusive ownership of one profile storage root.
use std::path::{Path, PathBuf};

/// Exclusive cooperating ownership of a profile storage root. The open file retains
/// the OS lock for the guard's lifetime; its stable path must never be unlinked
/// on release, since another process may already have that same file open.
pub struct ProfileStorageLease {
    root: PathBuf,
    file: std::fs::File,
    external: Vec<ProfileStorageLease>,
}

impl ProfileStorageLease {
    /// Acquire the persistent `node.lock` before opening profile storage.
    /// Root aliases resolve to the same lock; the lock is not an identity proof.
    pub fn acquire(data_dir: &Path) -> std::io::Result<Self> {
        use std::io::{Read, Seek, Write};
        let data_dir = if data_dir.as_os_str().is_empty() {
            Path::new(".")
        } else {
            data_dir
        };
        std::fs::create_dir_all(data_dir)?;
        let root = std::fs::canonicalize(data_dir)?;
        let data_dir = root.as_path();
        let path = data_dir.join("node.lock");
        let busy = || {
            std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!(
                    "data dir {} is locked by another storage owner; stop the owning process before retrying",
                    data_dir.display()
                ),
            )
        };
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Nonblocking open also prevents a substituted FIFO from hanging startup.
            options
                .mode(0o600)
                .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // FILE_FLAG_OPEN_REPARSE_POINT: inspect the entry itself, not its target.
            options.custom_flags(0x0020_0000);
        }
        let mut file = options.open(&path)?;
        let metadata = file.metadata()?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            // FILE_ATTRIBUTE_REPARSE_POINT includes symlinks and other redirections.
            if metadata.file_attributes() & 0x400 != 0 {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "node.lock must not be a reparse point",
                ));
            }
        }
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "node.lock must be a regular file",
            ));
        }
        require_single_link(&file)?;
        match file.try_lock() {
            Ok(()) => {}
            Err(std::fs::TryLockError::WouldBlock) => return Err(busy()),
            Err(std::fs::TryLockError::Error(error)) => return Err(error),
        }
        // Old binaries use a bare PID without an OS lock. Retain a live legacy
        // holder conservatively; this cannot make mixed-version startup race-free.
        // Limit the diagnostic read even if an unrelated large file is present.
        let mut diagnostic = Vec::new();
        Read::by_ref(&mut file)
            .take(32)
            .read_to_end(&mut diagnostic)?;
        let holder = (metadata.len() <= 32)
            .then(|| {
                std::str::from_utf8(&diagnostic)
                    .ok()?
                    .trim()
                    .parse::<u32>()
                    .ok()
            })
            .flatten();
        if metadata.len() != 0 && holder.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "node.lock contains unrecognized nonempty data",
            ));
        }
        if holder.is_some_and(pid_is_alive) {
            return Err(busy());
        }
        #[cfg(test)]
        tests::profile_lock_before_publish();
        file.set_len(0)?;
        file.rewind()?;
        write!(file, "{}", std::process::id())?;
        Ok(ProfileStorageLease {
            file,
            root,
            external: Vec::new(),
        })
    }

    /// The canonical root whose persistent lock this lease owns.
    pub fn canonical_root(&self) -> &Path {
        &self.root
    }

    /// Validate the actual device keystore and its account-keystore sibling
    /// before either load-or-create can follow or replace a redirected source.
    pub fn validate_identity_sources(
        &self,
        identity_path: &Path,
    ) -> Result<(), crate::eventlog::LogError> {
        let parent = identity_path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let leaf = identity_path
            .file_name()
            .ok_or_else(|| manifest::invalid("identity source must name a file"))?;
        if std::fs::canonicalize(parent)? != self.root {
            return Err(manifest::invalid(
                "identity source parent is not owned by profile lease",
            ));
        }
        let standard_identity = leaf
            .to_str()
            .is_some_and(|leaf| leaf.eq_ignore_ascii_case("identity.keystore"));
        let default_source = leaf.to_str().is_some_and(|leaf| {
            ["messages.log", "sent.log", "relay.log", "relay.retention"]
                .iter()
                .any(|name| leaf.eq_ignore_ascii_case(name))
        });
        if (!standard_identity && participant::reserved_sidecar_name(leaf))
            || default_source
            || participant::reserved_directory_name(leaf)
        {
            return Err(manifest::invalid(
                "identity source collides with profile authority",
            ));
        }
        publication_files::inspect_regular(&self.root.join(leaf))?;
        publication_files::inspect_regular(&self.root.join("account.keystore"))?;
        Ok(())
    }

    /// Extend this same owner to an explicitly supplied source parent. Never
    /// wait while holding another root, nor infer ownership of an ancestor.
    pub(crate) fn claim_source(
        &mut self,
        path: &Path,
    ) -> Result<PathBuf, crate::eventlog::LogError> {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let leaf = path
            .file_name()
            .ok_or_else(|| manifest::invalid("source must name a file"))?;
        std::fs::create_dir_all(parent)?;
        let parent = std::fs::canonicalize(parent)?;
        if parent != self.root && !self.external.iter().any(|owner| owner.root == parent) {
            self.external.push(Self::acquire(&parent)?);
        }
        let path = parent.join(leaf);
        publication_files::inspect_regular(&path)?;
        Ok(path)
    }
}

/// Reject file aliases before mutating a lock or admitting a legacy source.
pub(super) fn require_single_link(file: &std::fs::File) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if file.metadata()?.nlink() != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "profile file must have exactly one filesystem link",
            ));
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // SAFETY: the live borrowed handle and output structure remain valid.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error());
        }
        if info.nNumberOfLinks != 1 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "profile file must have exactly one filesystem link",
            ));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = file;
    }
    Ok(())
}

/// Whether a process with `pid` is currently alive (signal 0 probe). Used only to decide
/// if a leftover lockfile is stale. On the rare PID-reuse race this can over-report
/// "alive" and refuse the lock — a safe (conservative) failure, never silent corruption.
#[cfg(unix)]
fn pid_is_alive(pid: u32) -> bool {
    if pid == 0 || pid > libc::pid_t::MAX as u32 {
        return false;
    }
    // SAFETY: `kill` with signal 0 performs no signal delivery — it only checks whether the
    // process exists / is permitted to be signalled. No memory is touched.
    let rc = unsafe { libc::kill(pid as libc::pid_t, 0) };
    if rc == 0 {
        return true; // exists and signallable
    }
    // Only ESRCH (no such process) proves it's gone; EPERM means it exists but isn't ours.
    std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

/// Windows service managers terminate a process without running `Drop`, so a PID
/// in the lockfile must be checked before refusing a restart. Failure to open or
/// query a process is treated as alive: only a confirmed exit makes it safe to
/// reclaim the lock.
#[cfg(windows)]
fn pid_is_alive(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };

    // SAFETY: this opens a query-only handle for a numeric PID, checks its exit
    // code, and closes the handle. No process memory is accessed or modified.
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid) };
    if handle.is_null() {
        return std::io::Error::last_os_error().raw_os_error()
            != Some(ERROR_INVALID_PARAMETER as i32);
    }
    let mut exit_code = 0;
    let queried = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    unsafe { CloseHandle(handle) };
    queried == 0 || exit_code == STILL_ACTIVE as u32
}

/// Other platforms conservatively retain a lock whose holder cannot be probed.
#[cfg(not(any(unix, windows)))]
fn pid_is_alive(_pid: u32) -> bool {
    true
}

impl Drop for ProfileStorageLease {
    fn drop(&mut self) {
        // Clear the live PID before releasing ownership, permitting this process
        // to acquire again. A failed clear leaves conservative legacy detection.
        let _ = self.file.set_len(0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    // Only the subprocess helper sets these variables; ordinary test threads do
    // not mutate the environment or pause acquisitions.
    pub(super) fn profile_lock_before_publish() {
        if let Some(ready) = std::env::var_os("MESH_TEST_LOCK_READY") {
            std::fs::write(ready, b"ready").unwrap();
            let mut line = String::new();
            assert!(std::io::stdin().read_line(&mut line).unwrap() > 0);
        }
    }

    struct ProfileLockChild(std::process::Child);

    impl ProfileLockChild {
        fn spawn(root: &std::path::Path) -> Self {
            Self(
                std::process::Command::new(std::env::current_exe().unwrap())
                    .args([
                        "--exact",
                        "storage::profile::tests::profile_lock_child",
                        "--nocapture",
                    ])
                    .env("MESH_TEST_LOCK_DIR", root)
                    .env("MESH_TEST_LOCK_READY", root.join("ready"))
                    .stdin(std::process::Stdio::piped())
                    .spawn()
                    .unwrap(),
            )
        }

        fn wait_for(&mut self, path: &std::path::Path) {
            let deadline = Instant::now() + Duration::from_secs(10);
            while !path.exists() {
                assert!(self.0.try_wait().unwrap().is_none(), "lock child exited");
                assert!(Instant::now() < deadline, "lock child timed out");
                std::thread::sleep(Duration::from_millis(10));
            }
        }

        fn publish(&mut self) {
            use std::io::Write;
            self.0
                .stdin
                .as_mut()
                .unwrap()
                .write_all(b"publish\n")
                .unwrap();
        }
    }

    impl Drop for ProfileLockChild {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn profile_lock_child() {
        let Some(root) = std::env::var_os("MESH_TEST_LOCK_DIR") else {
            return;
        };
        let root = std::path::PathBuf::from(root);
        let _lock = ProfileStorageLease::acquire(&root).unwrap();
        std::fs::write(root.join("owned"), b"owned").unwrap();
        let mut line = String::new();
        assert!(std::io::stdin().read_line(&mut line).unwrap() > 0);
    }

    #[test]
    fn profile_lock_excludes_contender_before_pid_publication() {
        let root = tempfile::tempdir().unwrap();
        let mut child = ProfileLockChild::spawn(root.path());
        child.wait_for(&root.path().join("ready"));
        assert_eq!(
            std::fs::metadata(root.path().join("node.lock"))
                .unwrap()
                .len(),
            0
        );
        let contender = ProfileStorageLease::acquire(root.path());
        assert_eq!(
            contender
                .err()
                .expect("second process acquired the same profile")
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
        child.publish();
        child.wait_for(&root.path().join("owned"));
        assert!(ProfileStorageLease::acquire(root.path()).is_err());
    }

    #[test]
    fn profile_lock_releases_after_process_kill() {
        let root = tempfile::tempdir().unwrap();
        let mut child = ProfileLockChild::spawn(root.path());
        child.wait_for(&root.path().join("ready"));
        child.publish();
        child.wait_for(&root.path().join("owned"));
        assert!(ProfileStorageLease::acquire(root.path()).is_err());
        drop(child);
        let _lock = ProfileStorageLease::acquire(root.path()).unwrap();
    }

    #[test]
    fn profile_lock_reuses_persistent_file_and_releases_on_drop() {
        for content in ["", "4294967295"] {
            let root = tempfile::tempdir().unwrap();
            let path = root.path().join("node.lock");
            std::fs::write(&path, content).unwrap();
            let original = std::fs::File::open(&path).unwrap();
            let lock = ProfileStorageLease::acquire(root.path()).unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                assert_eq!(
                    original.metadata().unwrap().ino(),
                    std::fs::metadata(&path).unwrap().ino()
                );
            }
            drop(original);
            assert!(ProfileStorageLease::acquire(root.path()).is_err());
            drop(lock);
            assert!(path.exists());
            let _next = ProfileStorageLease::acquire(root.path()).unwrap();
        }
    }

    #[test]
    fn profile_lock_rejects_live_legacy_pid() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("node.lock"),
            std::process::id().to_string(),
        )
        .unwrap();
        assert_eq!(
            ProfileStorageLease::acquire(root.path())
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::AlreadyExists
        );
    }

    #[test]
    fn profile_lock_allows_different_directories() {
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();
        let _first = ProfileStorageLease::acquire(first.path()).unwrap();
        let _second = ProfileStorageLease::acquire(second.path()).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn profile_lock_rejects_aliases_and_unsafe_lock_paths() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("data");
        std::fs::create_dir(&data).unwrap();
        let alias = root.path().join("alias");
        symlink(&data, &alias).unwrap();
        let lock = ProfileStorageLease::acquire(&data).unwrap();
        assert!(ProfileStorageLease::acquire(&alias).is_err());
        drop(lock);
        std::fs::remove_file(data.join("node.lock")).unwrap();
        let target = root.path().join("target");
        std::fs::write(&target, b"untouched").unwrap();
        symlink(&target, data.join("node.lock")).unwrap();
        assert!(ProfileStorageLease::acquire(&data).is_err());
        assert_eq!(std::fs::read(&target).unwrap(), b"untouched");
        std::fs::remove_file(data.join("node.lock")).unwrap();
        std::fs::create_dir(data.join("node.lock")).unwrap();
        assert!(ProfileStorageLease::acquire(&data).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn profile_lock_creates_private_file_and_rejects_fifo() {
        use std::os::unix::{ffi::OsStrExt, fs::PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let lock = ProfileStorageLease::acquire(root.path()).unwrap();
        let path = root.path().join("node.lock");
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        drop(lock);
        std::fs::remove_file(&path).unwrap();
        let path_c = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: a valid NUL-terminated path is passed; no borrowed pointer escapes.
        assert_eq!(unsafe { libc::mkfifo(path_c.as_ptr(), 0o600) }, 0);
        assert_eq!(
            ProfileStorageLease::acquire(root.path())
                .err()
                .unwrap()
                .kind(),
            std::io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn profile_lock_canonicalizes_root_and_transfers_ownership() {
        let root = tempfile::tempdir().unwrap();
        let lease = ProfileStorageLease::acquire(root.path()).unwrap();
        assert_eq!(
            lease.canonical_root(),
            std::fs::canonicalize(root.path()).unwrap()
        );
        std::thread::spawn(move || {
            assert!(ProfileStorageLease::acquire(lease.canonical_root()).is_err());
            drop(lease);
        })
        .join()
        .unwrap();
        let _next = ProfileStorageLease::acquire(root.path()).unwrap();
    }
    #[test]
    fn profile_lock_empty_root_is_current_directory() {
        if std::env::var_os("MESH_TEST_EMPTY_ROOT").is_some() {
            let lease = ProfileStorageLease::acquire(Path::new("")).unwrap();
            assert_eq!(lease.canonical_root(), std::fs::canonicalize(".").unwrap());
            return;
        }
        let root = tempfile::tempdir().unwrap();
        assert!(
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "storage::profile::tests::profile_lock_empty_root_is_current_directory"
                ])
                .env("MESH_TEST_EMPTY_ROOT", "1")
                .current_dir(root.path())
                .status()
                .unwrap()
                .success()
        );
    }
}

pub(crate) mod context;
pub(crate) mod manifest;
pub(crate) mod participant;
mod participant_events;
pub(crate) mod publication;
#[cfg(test)]
mod publication_acceptance_tests;
pub(crate) mod publication_files;
#[cfg(test)]
mod publication_tests;

#[cfg(all(test, unix))]
mod lock_alias_regression {
    use super::*;
    #[test]
    fn hardlinked_lock_does_not_truncate_source() {
        let directory = tempfile::tempdir().unwrap();
        let source = directory.path().join("identity.keystore");
        std::fs::write(&source, b"private source must survive").unwrap();
        std::fs::hard_link(&source, directory.path().join("node.lock")).unwrap();
        assert!(ProfileStorageLease::acquire(directory.path()).is_err());
        assert_eq!(
            std::fs::read(source).unwrap(),
            b"private source must survive"
        );
    }
}
