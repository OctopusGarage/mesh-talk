//! Strict filesystem publication primitives; no authority is inferred from a
//! path. Archive/work locators are validated against their authenticated source.
use super::{
    manifest::{Source, invalid},
    publication::observe,
};
use crate::eventlog::LogError;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};
pub(super) fn source_layout(root: &Path, event: &Path) -> Result<[PathBuf; 2], LogError> {
    let mut result = [PathBuf::new(), PathBuf::new()];
    for (index, path) in [event.to_path_buf(), event.with_extension("retention")]
        .iter()
        .enumerate()
    {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if fs::canonicalize(parent)? != root {
            return Err(invalid(
                "relay source parent is not owned by the profile lease",
            ));
        }
        let leaf = path
            .file_name()
            .ok_or_else(|| invalid("relay source must name a file"))?;
        if leaf == "profile.db"
            || leaf == "profile.db-wal"
            || leaf == "profile.db-shm"
            || leaf == "node.lock"
        {
            return Err(invalid("relay source collides with profile authority"));
        }
        result[index] = PathBuf::from(leaf);
        inspect_regular(&root.join(leaf))?;
    }
    if result[0] == result[1] {
        return Err(invalid("relay event and arrival paths collide"));
    }
    Ok(result)
}

pub(super) fn inspect_regular(path: &Path) -> Result<bool, LogError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if !metadata.file_type().is_file() {
        return Err(invalid("publication path must be a regular file"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(invalid("publication path must not be a reparse point"));
        }
        let mut options = OpenOptions::new();
        use std::os::windows::fs::OpenOptionsExt;
        options.read(true).custom_flags(0x0020_0000);
        super::require_single_link(&options.open(path)?)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if metadata.nlink() != 1 {
            return Err(invalid("publication path must not have hardlink aliases"));
        }
    }
    Ok(true)
}

pub(super) fn reader(path: &Path) -> Result<File, LogError> {
    if !inspect_regular(path)? {
        return Err(invalid("missing publication source"));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.custom_flags(0x0020_0000);
    }
    let file = options.open(path)?;
    if !file.metadata()?.is_file() {
        return Err(invalid("publication source changed type"));
    }
    Ok(file)
}

pub(super) fn private_file(path: &Path) -> Result<File, LogError> {
    let mut options = OpenOptions::new();
    options.write(true).read(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    Ok(options.open(path)?)
}

pub(super) fn stream_identity(path: &Path) -> Result<(u64, Vec<u8>), LogError> {
    observe("hash");
    let mut file = reader(path)?;
    let mut hash = Sha256::new();
    let mut size = 0u64;
    let mut buffer = [0; 65536];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| invalid("source too large"))?;
        hash.update(&buffer[..n]);
    }
    Ok((size, hash.finalize().to_vec()))
}

pub(super) fn copy(source: &Path, target: &Path) -> Result<(u64, Vec<u8>), LogError> {
    let mut source = reader(source)?;
    let mut target = private_file(target)?;
    let mut buffer = [0; 65536];
    let mut hash = Sha256::new();
    let mut size = 0u64;
    loop {
        let n = source.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        target.write_all(&buffer[..n])?;
        hash.update(&buffer[..n]);
        size = size
            .checked_add(n as u64)
            .ok_or_else(|| invalid("source too large"))?;
    }
    target.sync_all()?;
    Ok((size, hash.finalize().to_vec()))
}

pub(super) fn matches_raw(path: &Path, source: &Source) -> Result<bool, LogError> {
    if !inspect_regular(path)? {
        return Ok(false);
    }
    let (size, hash) = stream_identity(path)?;
    Ok(size == source.raw_size && Some(hash) == source.raw_hash)
}

pub(super) fn matches_stub(path: &Path, stub: &[u8]) -> Result<bool, LogError> {
    if !inspect_regular(path)? {
        return Ok(false);
    }
    let mut file = reader(path)?;
    if file.metadata()?.len() != stub.len() as u64 {
        return Ok(false);
    }
    let mut actual = [0; 54];
    file.read_exact(&mut actual)?;
    Ok(actual.as_slice() == stub)
}

pub(super) fn sync_parent(path: &Path) -> Result<(), LogError> {
    #[cfg(unix)]
    File::open(path.parent().unwrap_or_else(|| Path::new(".")))?.sync_all()?;
    #[cfg(not(unix))]
    let _ = path;
    // Windows has no equivalent directory-fsync guarantee in this publication
    // helper. Native rename/kill and power-loss acceptance remains required.
    Ok(())
}

pub(super) fn validate_header(path: &Path, magic: &[u8; 6]) -> Result<(), LogError> {
    let mut file = reader(path)?;
    let mut header = [0; 22];
    file.read_exact(&mut header)
        .map_err(|_| invalid("short legacy source header"))?;
    if &header[..6] != magic {
        return Err(invalid("unknown legacy source header"));
    }
    Ok(())
}

pub(super) fn artifact_path(
    root: &Path,
    source: &Source,
    locator: &Path,
    extension: &str,
) -> Result<PathBuf, LogError> {
    let components = locator.components().collect::<Vec<_>>();
    if components.len() != 2 {
        return Err(invalid("invalid migration artifact layout"));
    }
    let directory = components[0]
        .as_os_str()
        .to_str()
        .ok_or_else(|| invalid("invalid migration directory name"))?;
    if !directory.starts_with(".relay-publication-")
        || directory.len() != ".relay-publication-".len() + 6
        || !directory[".relay-publication-".len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric())
    {
        return Err(invalid("unrecognized migration artifact directory"));
    }
    if components[1].as_os_str()
        != std::ffi::OsStr::new(&format!("source-{}.{}", source.source, extension))
    {
        return Err(invalid("migration artifact belongs to another source"));
    }
    let parent = root.join(directory);
    let metadata = fs::symlink_metadata(&parent)?;
    if !metadata.file_type().is_dir() {
        return Err(invalid(
            "migration artifact parent must be a private regular directory",
        ));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return Err(invalid("migration directory must not be a reparse point"));
        }
    }
    if fs::canonicalize(&parent)? != parent {
        return Err(invalid("migration artifact parent is redirected"));
    }
    Ok(root.join(locator))
}

pub(super) fn publish_absent(source: &Path, target: &Path) -> Result<(), LogError> {
    // No hard-link fallback: SQLCipher main files have exactly one name, and an
    // unexpectedly appeared final file is never overwritten.
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "android"
    ))]
    {
        use std::{ffi::CString, os::unix::ffi::OsStrExt};
        let source = CString::new(source.as_os_str().as_bytes())
            .map_err(|_| invalid("NUL in candidate path"))?;
        let target = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| invalid("NUL in final path"))?;
        // SAFETY: both C strings remain valid; flags request a no-replace rename.
        #[cfg(any(target_os = "macos", target_os = "ios"))]
        let result =
            unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), libc::RENAME_EXCL) };
        #[cfg(any(target_os = "linux", target_os = "android"))]
        let result = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result != 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        #[link(name = "kernel32")]
        unsafe extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
        let target: Vec<u16> = target.as_os_str().encode_wide().chain(Some(0)).collect();
        // SAFETY: terminated UTF16 buffers remain live; WRITE_THROUGH without
        // REPLACE_EXISTING refuses an appeared target. Native evidence pending.
        if unsafe { MoveFileExW(source.as_ptr(), target.as_ptr(), 0x8) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    #[cfg(not(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "linux",
        target_os = "android",
        windows
    )))]
    {
        let _ = (source, target);
        Err(invalid(
            "atomic absent database publication unsupported on this platform",
        ))
    }
}

/// Existing source identity, independent of path spelling. Callers also reject
/// portable case aliases for absent leaves before opening any legacy writer.
pub(super) fn same_file(left: &Path, right: &Path) -> Result<bool, LogError> {
    if !inspect_regular(left)? || !inspect_regular(right)? {
        return Ok(false);
    }
    let left = reader(left)?;
    let right = reader(right)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let left = left.metadata()?;
        let right = right.metadata()?;
        Ok(left.dev() == right.dev() && left.ino() == right.ino())
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let identity = |file: &File| -> Result<(u32, u32, u32), LogError> {
            let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
            // SAFETY: a live borrowed file handle and valid output are supplied.
            if unsafe { GetFileInformationByHandle(file.as_raw_handle().cast(), &mut info) } == 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            Ok((
                info.dwVolumeSerialNumber,
                info.nFileIndexHigh,
                info.nFileIndexLow,
            ))
        };
        Ok(identity(&left)? == identity(&right)?)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (left, right);
        Err(invalid(
            "platform cannot verify existing profile source aliases",
        ))
    }
}
