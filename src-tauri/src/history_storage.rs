//! Bounded metadata-only observation of local profile files. No history is opened.
use crate::{commands::CommandError, runtime_admission::RuntimeAdmission};
use serde::Serialize;
use std::path::Path;

const MAX_ENTRIES: usize = 4096;

#[derive(Serialize)]
pub struct HistoryStorageUsage {
    bytes: u64,
    complete: bool,
}

fn measure(root: &Path, limit: usize) -> std::io::Result<HistoryStorageUsage> {
    let mut usage = HistoryStorageUsage {
        bytes: 0,
        complete: true,
    };
    // Do not descend into attachments, backups or migration archives. Each entry
    // is discarded before reading the next; no body, decryption or directory list.
    let mut entries = std::fs::read_dir(root)?;
    for _ in 0..limit {
        let Some(entry) = entries.next() else {
            return Ok(usage);
        };
        let entry = entry?;
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(value) => value,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                usage.complete = false;
                continue;
            }
            Err(error) => return Err(error),
        };
        if metadata.file_type().is_file() {
            usage.bytes = usage.bytes.saturating_add(metadata.len());
        }
    }
    // Reaching the budget is conservatively partial, even if it was the last entry.
    usage.complete = false;
    Ok(usage)
}

#[tauri::command]
pub async fn history_storage_usage(
    app: tauri::State<'_, crate::state::AppState>,
    node_state: tauri::State<'_, crate::chat_commands::NodeState>,
    owner: String,
    expected_account: String,
) -> Result<HistoryStorageUsage, CommandError> {
    RuntimeAdmission::capture(&app, &node_state, &owner)?
        .with_runtime(|runtime| {
            if runtime.account_id() != expected_account {
                return Err(CommandError::Authorization("storage owner changed".into()));
            }
            let anchor =
                crate::config_store::account_path(&crate::data_dir(), &owner, "profile.db")
                    .ok_or_else(|| CommandError::Authorization("invalid storage owner".into()))?;
            measure(
                anchor.parent().expect("account profile parent"),
                MAX_ENTRIES,
            )
            .map_err(|error| CommandError::Io(error.to_string()))
        })
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn counts_file_metadata_without_opening_history_or_nested_attachments() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("messages.log"),
            b"not a valid encrypted log",
        )
        .unwrap();
        let database = std::fs::File::create(root.path().join("profile.db")).unwrap();
        database.set_len(2 * 1024 * 1024 * 1024).unwrap();
        std::fs::create_dir(root.path().join("media")).unwrap();
        std::fs::write(root.path().join("media/attachment"), [0; 50]).unwrap();
        let usage = measure(root.path(), MAX_ENTRIES).unwrap();
        assert_eq!(usage.bytes, 2 * 1024 * 1024 * 1024 + 25);
        assert!(usage.complete);
    }
    #[test]
    fn bounds_directory_work_and_reports_partial_observation() {
        let root = tempfile::tempdir().unwrap();
        for name in ["one", "two", "three"] {
            std::fs::write(root.path().join(name), [0; 7]).unwrap();
        }
        let usage = measure(root.path(), 2).unwrap();
        assert_eq!(usage.bytes, 14);
        assert!(!usage.complete);
    }
    #[cfg(unix)]
    #[test]
    fn never_follows_file_or_directory_symlinks() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("private"), [0; 50]).unwrap();
        symlink(outside.path().join("private"), root.path().join("file")).unwrap();
        symlink(outside.path(), root.path().join("directory")).unwrap();
        assert_eq!(measure(root.path(), MAX_ENTRIES).unwrap().bytes, 0);
    }
}
