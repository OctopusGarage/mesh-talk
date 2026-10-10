//! Directory attachments use a tar payload inside the existing encrypted, chunked file
//! transfer. The MIME marker lets current clients extract it while older clients can
//! still save and open the ordinary `.tar` attachment.

use crate::eventlog::ConversationId;
use crate::file::AnyManifest;
use crate::node::{Node, NodeError};
use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

pub const MIME: &str = "application/x-mesh-talk-directory-tar";
const MAX_ENTRIES: usize = 10_000;
const MAX_DEPTH: usize = 32;
const MAX_PATH_BYTES: usize = 1024;
const MAX_ARCHIVE_BYTES: u64 = 4 * 1024 * 1024 * 1024;

fn invalid(message: &str) -> NodeError {
    NodeError::InvalidInput(message.into())
}

pub fn archive_name(root: &Path) -> Result<String, NodeError> {
    let name = root
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| invalid("directory name is not UTF-8"))?;
    Ok(format!(
        "{}.tar",
        crate::util::savename::sanitize_filename(&name.replace('\\', "_"))
    ))
}

fn safe_relative(path: &Path) -> bool {
    !path.as_os_str().is_empty()
        && path.as_os_str().len() <= MAX_PATH_BYTES
        && path.components().all(|c| matches!(c, Component::Normal(_)))
}

fn archive_component(raw: &std::ffi::OsStr, used: &mut HashSet<String>) -> String {
    // A backslash is a legal filename character on Unix but a separator on Windows.
    // Normalize before tar encoding so the same archive path has one meaning everywhere.
    let base = crate::util::savename::sanitize_filename(&raw.to_string_lossy().replace('\\', "_"));
    let split = base
        .rfind('.')
        .filter(|index| *index > 0)
        .unwrap_or(base.len());
    let (stem, ext) = base.split_at(split);
    let mut candidate = base.clone();
    let mut counter = 1u32;
    while !used.insert(candidate.to_lowercase()) {
        candidate = format!("{stem} ({counter}){ext}");
        counter += 1;
    }
    candidate
}

/// Pack a tree without following symlinks. The archive stays alive until staging ends.
pub fn pack(root: &Path) -> Result<tempfile::NamedTempFile, NodeError> {
    if !fs::symlink_metadata(root)
        .map_err(|e| NodeError::File(format!("inspect directory: {e}")))?
        .file_type()
        .is_dir()
    {
        return Err(invalid("select a directory"));
    }
    let archive = tempfile::NamedTempFile::new()
        .map_err(|e| NodeError::File(format!("create directory archive: {e}")))?;
    let output = archive
        .reopen()
        .map_err(|e| NodeError::File(format!("open directory archive: {e}")))?;
    let mut builder = tar::Builder::new(output);
    let mut count = 0usize;
    let mut estimated_size = 1024u64;
    let mut stack = vec![(root.to_path_buf(), PathBuf::new(), 0usize)];
    while let Some((dir, relative, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(invalid("directory is too deep"));
        }
        let mut children = Vec::new();
        for child in
            fs::read_dir(&dir).map_err(|e| NodeError::File(format!("read directory: {e}")))?
        {
            children
                .push(child.map_err(|e| NodeError::File(format!("read directory entry: {e}")))?);
            if children.len() > MAX_ENTRIES.saturating_sub(count) {
                return Err(invalid("directory has too many entries"));
            }
        }
        children.sort_by_key(|e| e.file_name());
        let mut used_names = HashSet::new();
        for child in children.into_iter().rev() {
            count += 1;
            if count > MAX_ENTRIES {
                return Err(invalid("directory has too many entries"));
            }
            let next = relative.join(archive_component(&child.file_name(), &mut used_names));
            if !safe_relative(&next) || next.components().count() > MAX_DEPTH {
                return Err(invalid("directory contains an unsafe path"));
            }
            let path = child.path();
            let kind = fs::symlink_metadata(&path)
                .map_err(|e| NodeError::File(format!("inspect entry: {e}")))?
                .file_type();
            if kind.is_dir() {
                estimated_size = estimated_size.saturating_add(512);
                builder
                    .append_dir(&next, &path)
                    .map_err(|e| NodeError::File(format!("archive directory: {e}")))?;
                stack.push((path, next, depth + 1));
            } else if kind.is_file() {
                let mut file =
                    File::open(&path).map_err(|e| NodeError::File(format!("open entry: {e}")))?;
                let size = file
                    .metadata()
                    .map_err(|e| NodeError::File(format!("inspect entry: {e}")))?
                    .len();
                estimated_size = estimated_size
                    .saturating_add(512)
                    .saturating_add(size.div_ceil(512).saturating_mul(512));
                if estimated_size > MAX_ARCHIVE_BYTES {
                    return Err(invalid("directory archive is too large"));
                }
                builder
                    .append_file(&next, &mut file)
                    .map_err(|e| NodeError::File(format!("archive file: {e}")))?;
            } else {
                return Err(invalid("directory contains a symlink or unsupported entry"));
            }
        }
    }
    builder
        .finish()
        .map_err(|e| NodeError::File(format!("finish directory archive: {e}")))?;
    drop(builder);
    if archive
        .as_file()
        .metadata()
        .map_err(|e| NodeError::File(format!("inspect archive: {e}")))?
        .len()
        > MAX_ARCHIVE_BYTES
    {
        return Err(invalid("directory archive is too large"));
    }
    Ok(archive)
}

/// Verify the encrypted payload before exposing any extracted entry. Extraction occurs
/// in a private staging directory and becomes visible only after the whole archive passes.
pub fn save(
    node: &Node,
    file_conv: ConversationId,
    dir: &Path,
    manifest: &AnyManifest,
) -> Result<PathBuf, NodeError> {
    let name = manifest
        .name()
        .strip_suffix(".tar")
        .filter(|name| !name.is_empty())
        .ok_or_else(|| invalid("invalid directory attachment name"))?;
    let dest = crate::util::savename::safe_save_path(dir, name)
        .ok_or_else(|| invalid("could not place directory safely"))?;
    fs::create_dir_all(dir)
        .map_err(|e| NodeError::File(format!("create download directory: {e}")))?;
    let staging = tempfile::tempdir_in(dir)
        .map_err(|e| NodeError::File(format!("create staging directory: {e}")))?;
    let archive = staging.path().join("transfer.tar");
    let extracted = staging.path().join("folder");
    fs::create_dir(&extracted)
        .map_err(|e| NodeError::File(format!("create extraction directory: {e}")))?;
    node.save_directory_archive(file_conv, &archive)?;
    extract(&archive, &extracted, manifest.size())?;
    #[cfg(unix)]
    sync_directories(&extracted)
        .map_err(|e| NodeError::File(format!("sync extracted directory: {e}")))?;
    fs::rename(&extracted, &dest)
        .map_err(|e| NodeError::File(format!("finish directory save: {e}")))?;
    #[cfg(unix)]
    File::open(dir)
        .and_then(|folder| folder.sync_all())
        .map_err(|e| NodeError::File(format!("sync download directory: {e}")))?;
    node.complete_directory_save(file_conv);
    Ok(dest)
}

#[cfg(unix)]
fn sync_directories(root: &Path) -> std::io::Result<()> {
    let mut pending = vec![root.to_path_buf()];
    let mut directories = Vec::new();
    while let Some(dir) = pending.pop() {
        for child in fs::read_dir(&dir)? {
            let child = child?;
            if child.file_type()?.is_dir() {
                pending.push(child.path());
            }
        }
        directories.push(dir);
    }
    for dir in directories.into_iter().rev() {
        File::open(dir)?.sync_all()?;
    }
    Ok(())
}

fn mapped_directory(
    root: &Path,
    relative: &Path,
    directories: &mut HashMap<PathBuf, PathBuf>,
    files: &HashSet<PathBuf>,
) -> Result<PathBuf, NodeError> {
    let mut original = PathBuf::new();
    let mut actual = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(name) = component else {
            return Err(invalid("archive contains an unsafe path"));
        };
        original.push(name);
        if files.contains(&original) {
            return Err(invalid("archive file conflicts with a directory"));
        }
        if let Some(mapped) = directories.get(&original) {
            actual = mapped.clone();
            continue;
        }
        let mapped = crate::util::savename::safe_save_path(&actual, &name.to_string_lossy())
            .ok_or_else(|| invalid("could not place archive directory safely"))?;
        fs::create_dir(&mapped)
            .map_err(|e| NodeError::File(format!("create archive directory: {e}")))?;
        directories.insert(original.clone(), mapped.clone());
        actual = mapped;
    }
    Ok(actual)
}

fn extract(archive_path: &Path, target: &Path, archive_size: u64) -> Result<(), NodeError> {
    let input =
        File::open(archive_path).map_err(|e| NodeError::File(format!("open archive: {e}")))?;
    let mut archive = tar::Archive::new(input);
    let mut count = 0usize;
    let mut extracted = 0u64;
    let mut directories = HashMap::new();
    let mut files = HashSet::new();
    for item in archive
        .entries()
        .map_err(|e| NodeError::File(format!("read archive: {e}")))?
    {
        let mut entry = item.map_err(|e| NodeError::File(format!("read archive entry: {e}")))?;
        count += 1;
        if count > MAX_ENTRIES {
            return Err(invalid("archive has too many entries"));
        }
        let path = entry
            .path()
            .map_err(|e| NodeError::File(format!("archive path: {e}")))?;
        if !safe_relative(&path) || path.components().count() > MAX_DEPTH {
            return Err(invalid("archive contains an unsafe path"));
        }
        let raw_path = path.to_path_buf();
        let kind = entry.header().entry_type();
        if kind.is_dir() {
            mapped_directory(target, &raw_path, &mut directories, &files)?;
        } else if kind.is_file() {
            if directories.contains_key(&raw_path) || !files.insert(raw_path.clone()) {
                return Err(invalid("archive contains duplicate or conflicting entries"));
            }
            extracted = extracted
                .checked_add(entry.size())
                .ok_or_else(|| invalid("archive size overflow"))?;
            if extracted > archive_size {
                return Err(invalid("archive expands beyond transfer size"));
            }
            let parent = mapped_directory(
                target,
                raw_path.parent().unwrap_or_else(|| Path::new("")),
                &mut directories,
                &files,
            )?;
            let name = raw_path
                .file_name()
                .ok_or_else(|| invalid("archive contains an unsafe file name"))?;
            let dest = crate::util::savename::safe_save_path(&parent, &name.to_string_lossy())
                .ok_or_else(|| invalid("could not place archive file safely"))?;
            let mut output = File::options()
                .write(true)
                .create_new(true)
                .open(&dest)
                .map_err(|e| NodeError::File(format!("create archive file: {e}")))?;
            let copied = std::io::copy(&mut entry, &mut output)
                .map_err(|e| NodeError::File(format!("extract archive entry: {e}")))?;
            if copied != entry.size() {
                return Err(invalid("archive entry is truncated"));
            }
            output
                .flush()
                .and_then(|_| output.sync_all())
                .map_err(|e| NodeError::File(format!("sync archive entry: {e}")))?;
        } else {
            return Err(invalid("archive contains a link or unsupported entry"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_nested_and_empty_folders() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("photos");
        fs::create_dir_all(source.join("nested/empty")).unwrap();
        fs::write(source.join("nested/pic.txt"), b"hello").unwrap();
        let packed = pack(&source).unwrap();
        let target = tempfile::tempdir().unwrap();
        extract(
            packed.path(),
            target.path(),
            packed.as_file().metadata().unwrap().len(),
        )
        .unwrap();
        assert_eq!(
            fs::read(target.path().join("nested/pic.txt")).unwrap(),
            b"hello"
        );
        assert!(target.path().join("nested/empty").is_dir());
    }

    #[test]
    fn empty_root_is_a_valid_folder_attachment() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("empty");
        fs::create_dir(&source).unwrap();
        let packed = pack(&source).unwrap();
        let target = tempfile::tempdir().unwrap();
        extract(
            packed.path(),
            target.path(),
            packed.as_file().metadata().unwrap().len(),
        )
        .unwrap();
        assert_eq!(fs::read_dir(target.path()).unwrap().count(), 0);
    }

    #[test]
    fn root_folder_name_is_portable() {
        assert_eq!(archive_name(Path::new("CON")).unwrap(), "_CON.tar");
    }

    #[test]
    fn rejects_traversal_and_links() {
        assert!(!safe_relative(Path::new("../escape")));
        assert!(!safe_relative(Path::new("/absolute")));
        let archive = tempfile::NamedTempFile::new().unwrap();
        let mut builder = tar::Builder::new(archive.reopen().unwrap());
        let mut header = tar::Header::new_gnu();
        header.set_entry_type(tar::EntryType::Symlink);
        header.set_size(0);
        header.set_cksum();
        builder
            .append_data(&mut header, "link", std::io::empty())
            .unwrap();
        builder.finish().unwrap();
        drop(builder);
        let target = tempfile::tempdir().unwrap();
        assert!(extract(
            archive.path(),
            target.path(),
            archive.as_file().metadata().unwrap().len()
        )
        .is_err());
        assert!(!target.path().join("link").exists());
    }

    #[test]
    fn extraction_sanitizes_cross_platform_names_and_keeps_collisions() {
        let archive = tempfile::NamedTempFile::new().unwrap();
        let mut builder = tar::Builder::new(archive.reopen().unwrap());
        for (name, content) in [
            ("pics?/report?.txt", "first"),
            ("pics*/report?.txt", "second"),
            ("pics?/report*.txt", "third"),
            ("CON.txt", "device"),
        ] {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, name, content.as_bytes())
                .unwrap();
        }
        builder.finish().unwrap();
        drop(builder);
        let target = tempfile::tempdir().unwrap();
        extract(
            archive.path(),
            target.path(),
            archive.as_file().metadata().unwrap().len(),
        )
        .unwrap();
        assert_eq!(
            fs::read(target.path().join("pics_/report_.txt")).unwrap(),
            b"first"
        );
        assert_eq!(
            fs::read(target.path().join("pics_ (1)/report_.txt")).unwrap(),
            b"second"
        );
        assert_eq!(
            fs::read(target.path().join("pics_/report_ (1).txt")).unwrap(),
            b"third"
        );
        assert_eq!(fs::read(target.path().join("_CON.txt")).unwrap(), b"device");
    }

    #[cfg(unix)]
    #[test]
    fn sender_rejects_symlink_inside_directory() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("folder");
        fs::create_dir(&source).unwrap();
        std::os::unix::fs::symlink(root.path(), source.join("link")).unwrap();
        assert!(pack(&source).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn unix_backslash_name_cannot_alias_a_windows_directory_path() {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("folder");
        fs::create_dir_all(source.join("a")).unwrap();
        fs::write(source.join("a/b"), b"nested").unwrap();
        fs::write(source.join("a\\b"), b"backslash").unwrap();
        fs::write(source.join("a_b"), b"underscore").unwrap();
        let packed = pack(&source).unwrap();
        let target = tempfile::tempdir().unwrap();
        extract(
            packed.path(),
            target.path(),
            packed.as_file().metadata().unwrap().len(),
        )
        .unwrap();
        assert_eq!(fs::read(target.path().join("a/b")).unwrap(), b"nested");
        let siblings = [
            fs::read(target.path().join("a_b")).unwrap(),
            fs::read(target.path().join("a_b (1)")).unwrap(),
        ];
        assert!(siblings.contains(&b"backslash".to_vec()));
        assert!(siblings.contains(&b"underscore".to_vec()));
    }
}
