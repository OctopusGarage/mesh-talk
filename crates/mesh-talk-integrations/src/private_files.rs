//! Secret-file handling shared by local adapters.
use crate::IntegrationError;
use serde::{Serialize, de::DeserializeOwned};
use std::{
    fs::File,
    io::{Read, Write},
    path::Path,
};

const MAX_PRIVATE_FILE: usize = 1024 * 1024;
pub(crate) fn private_error() -> IntegrationError {
    IntegrationError::new("private_file", "Cannot access a protected local file")
}

pub fn read_private_json<T: DeserializeOwned>(path: &Path) -> Result<T, IntegrationError> {
    let mut file = open_private(path, false)?;
    let metadata = file.metadata().map_err(|_| private_error())?;
    if !metadata.is_file() || metadata.len() > MAX_PRIVATE_FILE as u64 {
        return Err(private_error());
    }
    check_private(&file)?;
    let mut bytes = Vec::new();
    Read::by_ref(&mut file)
        .take((MAX_PRIVATE_FILE + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|_| private_error())?;
    if bytes.len() > MAX_PRIVATE_FILE {
        return Err(private_error());
    }
    serde_json::from_slice(&bytes).map_err(|_| private_error())
}

pub fn write_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), IntegrationError> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|_| private_error())?;
    if bytes.len() > MAX_PRIVATE_FILE {
        return Err(private_error());
    }
    let mut file = open_private(path, true)?;
    check_private(&file)?;
    file.write_all(&bytes)
        .and_then(|_| file.sync_all())
        .map_err(|_| private_error())?;
    #[cfg(unix)]
    File::open(path.parent().unwrap_or(Path::new(".")))
        .and_then(|f| f.sync_all())
        .map_err(|_| private_error())?;
    Ok(())
}

/// Atomically publish a new protected JSON document without exposing partial content.
/// Existing documents must already satisfy the private-file contract.
pub fn replace_private_json<T: Serialize>(path: &Path, value: &T) -> Result<(), IntegrationError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => {
            let _: serde_json::Value = read_private_json(path)?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(private_error()),
    }
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let temporary = parent.join(format!(".mesh-talk-{}.tmp", uuid::Uuid::new_v4().simple()));
    let result = (|| {
        write_private_json(&temporary, value)?;
        std::fs::rename(&temporary, path).map_err(|_| private_error())?;
        #[cfg(unix)]
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(|_| private_error())?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

#[cfg(unix)]
fn open_private(path: &Path, create: bool) -> Result<File, IntegrationError> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .read(!create)
        .write(create)
        .create_new(create)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .map_err(|_| private_error())
}
#[cfg(unix)]
fn check_private(file: &File) -> Result<(), IntegrationError> {
    use std::os::unix::fs::MetadataExt;
    let metadata = file.metadata().map_err(|_| private_error())?;
    // SAFETY: geteuid takes no pointers and has no preconditions.
    if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o777 != 0o600 {
        return Err(private_error());
    }
    Ok(())
}

#[cfg(windows)]
fn open_private(path: &Path, create: bool) -> Result<File, IntegrationError> {
    use std::os::windows::{ffi::OsStrExt, io::FromRawHandle};
    use windows_sys::Win32::{
        Foundation::{GENERIC_READ, GENERIC_WRITE, INVALID_HANDLE_VALUE},
        Storage::FileSystem::*,
    };
    let security = windows_security::Security::new()?;
    let mut attributes = security.attributes();
    let path: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: arguments point to live, terminated path and security allocations.
    let handle = unsafe {
        CreateFileW(
            path.as_ptr(),
            if create { GENERIC_WRITE } else { GENERIC_READ } | READ_CONTROL,
            FILE_SHARE_READ | if create { 0 } else { FILE_SHARE_DELETE },
            &mut attributes,
            if create { CREATE_NEW } else { OPEN_EXISTING },
            FILE_FLAG_OPEN_REPARSE_POINT,
            std::ptr::null_mut(),
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        return Err(private_error());
    }
    // SAFETY: CreateFileW returned an owned valid file handle.
    let file = unsafe { File::from_raw_handle(handle) };
    use std::os::windows::fs::MetadataExt;
    if file
        .metadata()
        .map_err(|_| private_error())?
        .file_attributes()
        & FILE_ATTRIBUTE_REPARSE_POINT
        != 0
    {
        return Err(private_error());
    }
    Ok(file)
}
#[cfg(windows)]
fn check_private(file: &File) -> Result<(), IntegrationError> {
    use std::os::windows::io::AsRawHandle;
    windows_security::check_handle(file.as_raw_handle())
}

#[cfg(windows)]
pub(crate) mod windows_security {
    use super::*;
    use std::{ffi::c_void, ptr};
    use windows_sys::Win32::{
        Foundation::{CloseHandle, LocalFree},
        Security::{Authorization::*, *},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
    };

    pub(crate) struct Security {
        descriptor: PSECURITY_DESCRIPTOR,
    }
    impl Security {
        pub(crate) fn new() -> Result<Self, IntegrationError> {
            let sid = current_sid()?;
            let sid_text = sid_string(sid.as_ptr().cast_mut().cast())?;
            let sddl: Vec<u16> = format!("O:{sid_text}D:P(A;;GA;;;{sid_text})")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let mut descriptor = ptr::null_mut();
            // SAFETY: input is terminated; Windows allocates the output descriptor.
            if unsafe {
                ConvertStringSecurityDescriptorToSecurityDescriptorW(
                    sddl.as_ptr(),
                    SDDL_REVISION_1,
                    &mut descriptor,
                    ptr::null_mut(),
                )
            } == 0
            {
                return Err(private_error());
            }
            Ok(Self { descriptor })
        }
        pub(crate) fn attributes(&self) -> SECURITY_ATTRIBUTES {
            SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: self.descriptor,
                bInheritHandle: 0,
            }
        }
    }
    impl Drop for Security {
        fn drop(&mut self) {
            unsafe {
                LocalFree(self.descriptor);
            }
        }
    }
    fn current_sid() -> Result<Vec<u8>, IntegrationError> {
        let mut token = ptr::null_mut();
        if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) } == 0 {
            return Err(private_error());
        }
        let mut needed = 0;
        unsafe {
            GetTokenInformation(token, TokenUser, ptr::null_mut(), 0, &mut needed);
        }
        // Vec<usize> guarantees alignment for TOKEN_USER.
        let mut buffer = vec![0usize; (needed as usize).div_ceil(std::mem::size_of::<usize>())];
        let ok = unsafe {
            GetTokenInformation(
                token,
                TokenUser,
                buffer.as_mut_ptr().cast(),
                needed,
                &mut needed,
            )
        };
        unsafe {
            CloseHandle(token);
        }
        if ok == 0 {
            return Err(private_error());
        }
        let sid = unsafe { (*(buffer.as_ptr().cast::<TOKEN_USER>())).User.Sid };
        let length = unsafe { GetLengthSid(sid) } as usize;
        let mut bytes = vec![0u8; length];
        if unsafe { CopySid(length as u32, bytes.as_mut_ptr().cast(), sid) } == 0 {
            return Err(private_error());
        }
        Ok(bytes)
    }
    fn sid_string(sid: PSID) -> Result<String, IntegrationError> {
        let mut output = ptr::null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut output) } == 0 {
            return Err(private_error());
        }
        let mut length = 0;
        unsafe {
            while *output.add(length) != 0 {
                length += 1;
            }
        }
        let result =
            String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(output, length) });
        unsafe {
            LocalFree(output.cast());
        }
        Ok(result)
    }
    pub(crate) fn check_handle(handle: *mut c_void) -> Result<(), IntegrationError> {
        let sid = current_sid()?;
        let sid = sid.as_ptr().cast_mut().cast();
        let mut owner = ptr::null_mut();
        let mut dacl: *mut ACL = ptr::null_mut();
        let mut descriptor = ptr::null_mut();
        let result = unsafe {
            GetSecurityInfo(
                handle,
                SE_FILE_OBJECT,
                OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
                &mut owner,
                ptr::null_mut(),
                &mut dacl,
                ptr::null_mut(),
                &mut descriptor,
            )
        };
        if result != 0 {
            return Err(private_error());
        }
        let valid = unsafe {
            let mut ace = ptr::null_mut();
            !owner.is_null()
                && EqualSid(owner, sid) != 0
                && !dacl.is_null()
                && (*dacl).AceCount == 1
                && GetAce(dacl, 0, &mut ace) != 0
                && {
                    let allowed = &*ace.cast::<ACCESS_ALLOWED_ACE>();
                    allowed.Header.AceType == 0
                        && allowed.Header.AceFlags == 0
                        && EqualSid((&allowed.SidStart as *const u32).cast_mut().cast(), sid) != 0
                }
        };
        unsafe {
            LocalFree(descriptor);
        }
        if valid { Ok(()) } else { Err(private_error()) }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    #[test]
    fn private_json_is_exclusive_and_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        let expected = json!({"token":"secret"});
        write_private_json(&path, &expected).unwrap();
        assert_eq!(read_private_json::<Value>(&path).unwrap(), expected);
        assert!(write_private_json(&path, &json!({})).is_err());
        assert_eq!(read_private_json::<Value>(&path).unwrap(), expected);
    }

    #[cfg(unix)]
    #[test]
    fn refuses_public_files_and_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        write_private_json(&path, &json!({})).unwrap();
        let link = dir.path().join("link.json");
        symlink(&path, &link).unwrap();
        assert!(read_private_json::<Value>(&link).is_err());
        assert!(replace_private_json(&link, &json!({"replacement":true})).is_err());
        assert_eq!(read_private_json::<Value>(&path).unwrap(), json!({}));
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(read_private_json::<Value>(&path).is_err());
    }

    #[test]
    fn atomic_replacement_keeps_private_permissions_and_no_temporary_files() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("endpoint.json");
        replace_private_json(&path, &json!({"endpoint":"one"})).unwrap();
        replace_private_json(&path, &json!({"endpoint":"two"})).unwrap();
        assert_eq!(
            read_private_json::<Value>(&path).unwrap(),
            json!({"endpoint":"two"})
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
            assert!(replace_private_json(&path, &json!({})).is_err());
            assert_eq!(
                std::fs::read_to_string(&path).unwrap(),
                "{\n  \"endpoint\": \"two\"\n}"
            );
        }
    }

    #[test]
    fn rejects_oversized_secret_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("client.json");
        assert!(write_private_json(&path, &"x".repeat(1024 * 1024)).is_err());
        assert!(!path.exists());
    }
}
