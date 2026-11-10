//! User-level Codex bootstrap instructions for authenticated API discovery.
use std::{
    fs::{self, Metadata},
    io::{Read, Write},
    path::{Component, Path, PathBuf},
};

const BEGIN: &str = "<!-- MESH_TALK_AGENT_ACCESS_START -->";
const END: &str = "<!-- MESH_TALK_AGENT_ACCESS_END -->";
const MAX_FILE_BYTES: usize = 1024 * 1024;

pub(super) struct InstructionPaths {
    pub agents: PathBuf,
}

pub(super) fn paths() -> Result<InstructionPaths, String> {
    let codex = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| crate::user_home_dir().map(|home| home.join(".codex")))
        .ok_or("Cannot locate the user-level Codex directory")?;
    let result = InstructionPaths {
        agents: codex.join("AGENTS.md"),
    };
    if !result.agents.is_absolute() {
        return Err("Agent access requires absolute user-level paths".into());
    }
    Ok(result)
}

pub(super) fn render(help_url: &str) -> String {
    format!(
        "Mesh Talk local API: {}.\nRead this help URL before using the API for user-authorized tasks.\nThe desktop app must be running and unlocked, with agent access enabled.\n",
        serde_json::to_string(help_url).expect("serializing a string cannot fail"),
    )
}

pub(super) fn install(agents_path: &Path, help_url: &str) -> Result<String, String> {
    let port = help_url
        .strip_prefix("http://127.0.0.1:")
        .and_then(|value| value.strip_suffix("/v1/help"))
        .and_then(|value| value.parse::<u16>().ok());
    if port.is_none_or(|value| value == 0) {
        return Err("Agent API help must use a numeric loopback HTTP URL".into());
    }
    let before = read_snapshot(agents_path)?;
    let range = managed_range(&before.bytes)?;
    let prompt = render(help_url);
    let block = format!("{BEGIN}\n{prompt}{END}");
    let mut updated = before.bytes.clone();
    if let Some(range) = range {
        updated.splice(range, block.bytes());
    } else {
        // Keep every added byte inside the markers so removal is lossless.
        updated.extend_from_slice(block.as_bytes());
    }
    replace(agents_path, &before, &updated)?;
    Ok(prompt)
}

pub(super) fn remove(agents_path: &Path) -> Result<(), String> {
    let before = read_snapshot(agents_path)?;
    if let Some(range) = managed_range(&before.bytes)? {
        let mut updated = before.bytes.clone();
        updated.drain(range);
        replace(agents_path, &before, &updated)?;
    }
    Ok(())
}

fn managed_range(bytes: &[u8]) -> Result<Option<std::ops::Range<usize>>, String> {
    let starts: Vec<_> = bytes
        .windows(BEGIN.len())
        .enumerate()
        .filter_map(|(i, b)| (b == BEGIN.as_bytes()).then_some(i))
        .collect();
    let ends: Vec<_> = bytes
        .windows(END.len())
        .enumerate()
        .filter_map(|(i, b)| (b == END.as_bytes()).then_some(i))
        .collect();
    match (starts.as_slice(), ends.as_slice()) {
        ([], []) => Ok(None),
        ([start], [end]) if start < end => Ok(Some(*start..*end + END.len())),
        _ => Err(
            "Malformed or duplicate Mesh Talk instruction markers; the file was not changed".into(),
        ),
    }
}

struct Snapshot {
    bytes: Vec<u8>,
    metadata: Option<Metadata>,
}

fn check_path(path: &Path, create_parents: bool) -> Result<(), String> {
    if !path.is_absolute() {
        return Err("Agent access requires an absolute path".into());
    }
    let mut current = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err("Agent access paths must not contain parent traversal".into());
        }
        current.push(component);
        // A Windows drive/UNC prefix becomes a queryable path only once the root
        // separator is appended. Still inspect the root and every descendant.
        if matches!(component, Component::Prefix(_)) {
            continue;
        }
        match fs::symlink_metadata(&current) {
            Ok(metadata) => {
                if is_link(&metadata) || (current != path && !metadata.is_dir()) {
                    return Err(
                        "Agent access paths must not traverse symbolic links or non-directories"
                            .into(),
                    );
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if create_parents && current != path {
                    fs::create_dir(&current).map_err(io_error)?;
                }
            }
            Err(error) => return Err(io_error(error)),
        }
    }
    Ok(())
}

fn is_link(metadata: &Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        // Junctions and other reparse points can redirect writes just like symbolic links.
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    {
        metadata.file_type().is_symlink()
    }
}

fn io_error(error: std::io::Error) -> String {
    format!("Cannot update user-level agent instructions: {error}")
}

fn read_snapshot(path: &Path) -> Result<Snapshot, String> {
    check_path(path, false)?;
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Snapshot {
                bytes: Vec::new(),
                metadata: None,
            });
        }
        Err(error) => return Err(io_error(error)),
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES as u64 {
        return Err("Agent instruction files must be regular files of at most 1 MiB".into());
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    let mut bytes = Vec::new();
    options
        .open(path)
        .map_err(io_error)?
        .take(MAX_FILE_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() > MAX_FILE_BYTES {
        return Err("Agent instruction file exceeds 1 MiB".into());
    }
    Ok(Snapshot {
        bytes,
        metadata: Some(metadata),
    })
}

fn same_snapshot(left: &Snapshot, right: &Snapshot) -> bool {
    if left.bytes != right.bytes {
        return false;
    }
    match (&left.metadata, &right.metadata) {
        (None, None) => true,
        (Some(left), Some(right)) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                if left.dev() != right.dev()
                    || left.ino() != right.ino()
                    || left.mode() != right.mode()
                {
                    return false;
                }
            }
            left.modified().ok() == right.modified().ok()
                && left.permissions().readonly() == right.permissions().readonly()
        }
        _ => false,
    }
}

fn replace(path: &Path, expected: &Snapshot, bytes: &[u8]) -> Result<(), String> {
    if bytes.len() > MAX_FILE_BYTES {
        return Err("Agent instruction file exceeds 1 MiB".into());
    }
    check_path(path, true)?;
    let parent = path
        .parent()
        .ok_or("Agent instruction path requires a parent directory")?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    if let Some(metadata) = &expected.metadata {
        temporary
            .as_file()
            .set_permissions(metadata.permissions())
            .map_err(io_error)?;
    } else {
        private_permissions(temporary.as_file())?;
    }
    temporary.write_all(bytes).map_err(io_error)?;
    temporary.as_file().sync_all().map_err(io_error)?;
    if !same_snapshot(expected, &read_snapshot(path)?) {
        return Err(
            "Agent instructions changed concurrently; retry after reviewing the file".into(),
        );
    }
    temporary
        .persist(path)
        .map_err(|error| io_error(error.error))?;
    Ok(())
}

fn private_permissions(file: &fs::File) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(io_error)?;
    }
    #[cfg(not(unix))]
    let _ = file;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let directory = root.path().canonicalize().unwrap();
        (
            root,
            directory.join("AGENTS.md"),
            directory.join("client.json"),
        )
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_and_verbatim_paths_support_nested_instruction_lifecycle() {
        let root = tempfile::Builder::new()
            .prefix("mesh-agent-instructions-")
            .tempdir_in(std::env::current_dir().unwrap())
            .unwrap();
        let ordinary = root.path().to_path_buf();
        let verbatim = ordinary.canonicalize().unwrap();
        for (index, base) in [ordinary, verbatim].iter().enumerate() {
            let agents = base.join(format!("nested-{index}/codex/AGENTS.md"));
            install(&agents, "http://127.0.0.1:12345/v1/help").unwrap();
            install(&agents, "http://127.0.0.1:54321/v1/help").unwrap();
            let installed = fs::read_to_string(&agents).unwrap();
            assert_eq!(installed.matches(BEGIN).count(), 1);
            assert!(installed.contains("54321/v1/help"));
            remove(&agents).unwrap();
            assert!(fs::read(&agents).unwrap().is_empty());
        }
    }

    #[test]
    fn repeated_install_and_remove_preserve_unrelated_bytes() {
        let (_root, agents, _config) = fixture();
        let original = b"# Personal rules\r\n\xff Keep this byte.\n";
        std::fs::write(&agents, original).unwrap();
        let first = install(&agents, "http://127.0.0.1:12345/v1/help").unwrap();
        let installed = std::fs::read(&agents).unwrap();
        assert_eq!(first, render("http://127.0.0.1:12345/v1/help"));
        install(&agents, "http://127.0.0.1:12345/v1/help").unwrap();
        assert_eq!(installed, std::fs::read(&agents).unwrap());
        install(&agents, "http://127.0.0.1:54321/v1/help").unwrap();
        let changed = std::fs::read(&agents).unwrap();
        assert_eq!(
            changed
                .windows(BEGIN.len())
                .filter(|bytes| *bytes == BEGIN.as_bytes())
                .count(),
            1
        );
        assert!(String::from_utf8_lossy(&changed).contains("54321/v1/help"));
        assert!(!String::from_utf8_lossy(&changed).contains("12345/v1/help"));
        remove(&agents).unwrap();
        remove(&agents).unwrap();
        assert_eq!(original, std::fs::read(&agents).unwrap().as_slice());
    }

    #[test]
    fn malformed_or_duplicate_markers_fail_without_writing() {
        let (_root, agents, _config) = fixture();
        for value in [
            BEGIN.to_owned(),
            END.to_owned(),
            format!("{BEGIN}{BEGIN}{END}"),
            format!("{END}{BEGIN}"),
        ] {
            std::fs::write(&agents, &value).unwrap();
            assert!(install(&agents, "http://127.0.0.1:12345/v1/help").is_err());
            assert!(remove(&agents).is_err());
            assert_eq!(value.as_bytes(), std::fs::read(&agents).unwrap());
        }
    }

    #[test]
    fn removal_preserves_bytes_immediately_outside_markers() {
        let (_root, agents, _config) = fixture();
        let content = format!("before\n{BEGIN}\nowned\n{END}\nafter");
        std::fs::write(&agents, content).unwrap();
        remove(&agents).unwrap();
        assert_eq!(std::fs::read(&agents).unwrap(), b"before\n\nafter");
    }

    #[test]
    fn oversized_file_and_changed_snapshot_are_rejected() {
        let (_root, agents, _config) = fixture();
        std::fs::write(&agents, vec![b'x'; MAX_FILE_BYTES + 1]).unwrap();
        assert!(remove(&agents).is_err());
        std::fs::write(&agents, b"original").unwrap();
        let expected = read_snapshot(&agents).unwrap();
        std::fs::write(&agents, b"concurrent edit").unwrap();
        assert!(replace(&agents, &expected, b"replacement").is_err());
        assert_eq!(std::fs::read(&agents).unwrap(), b"concurrent edit");
    }

    #[cfg(unix)]
    #[test]
    fn permissions_are_preserved_and_symlinks_are_rejected() {
        use std::os::unix::fs::{PermissionsExt, symlink};
        let (_root, agents, _config) = fixture();
        std::fs::write(&agents, b"personal\n").unwrap();
        std::fs::set_permissions(&agents, std::fs::Permissions::from_mode(0o640)).unwrap();
        install(&agents, "http://127.0.0.1:12345/v1/help").unwrap();
        assert_eq!(
            std::fs::metadata(&agents).unwrap().permissions().mode() & 0o777,
            0o640
        );
        remove(&agents).unwrap();
        assert_eq!(
            std::fs::metadata(&agents).unwrap().permissions().mode() & 0o777,
            0o640
        );
        let link = agents.with_file_name("link.md");
        symlink(&agents, &link).unwrap();
        assert!(install(&link, "http://127.0.0.1:12345/v1/help").is_err());
        assert!(remove(&link).is_err());
        let directory_link = agents.with_file_name("linked-directory");
        symlink(agents.parent().unwrap(), &directory_link).unwrap();
        assert!(
            install(
                &directory_link.join("AGENTS.md"),
                "http://127.0.0.1:12345/v1/help"
            )
            .is_err()
        );
    }

    #[test]
    fn short_prompt_bootstraps_public_discovery_without_private_paths() {
        let (_root, _agents, config) = fixture();
        let prompt = render("http://127.0.0.1:12345/v1/help");
        assert!(!prompt.contains(config.to_str().unwrap()));
        assert!(prompt.contains("http://127.0.0.1:12345/v1/help"));
        assert!(!prompt.contains("Bearer"));
        assert!(!prompt.contains("token"));
        assert_eq!(prompt.lines().count(), 3);
        assert!(!prompt.contains("properties"));
        assert!(!prompt.contains("grant_create"));
    }
}
