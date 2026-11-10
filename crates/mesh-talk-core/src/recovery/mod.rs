//! Portable, authenticated account recovery primitives.

pub mod archive;
pub mod key;
pub mod snapshot;

/// Failures surfaced by the portable backup API. Archive authentication errors
/// intentionally do not distinguish a wrong key from altered ciphertext.
#[derive(Debug)]
pub enum BackupError {
    Archive(archive::ArchiveError),
    Snapshot(snapshot::SnapshotError),
    Io(std::io::Error),
}

impl std::fmt::Display for BackupError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Archive(error) => error.fmt(f),
            Self::Snapshot(error) => error.fmt(f),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for BackupError {}

impl From<archive::ArchiveError> for BackupError {
    fn from(error: archive::ArchiveError) -> Self {
        Self::Archive(error)
    }
}

impl From<snapshot::SnapshotError> for BackupError {
    fn from(error: snapshot::SnapshotError) -> Self {
        Self::Snapshot(error)
    }
}

impl From<std::io::Error> for BackupError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BackupPreview {
    pub account_id: String,
    pub created_at_ms: u64,
    pub messages: u64,
    pub channels: u64,
    pub file_cards: u64,
    pub media_files: u64,
    pub missing_media: u64,
    #[serde(default)]
    pub favorites: u64,
    #[serde(default)]
    pub favorite_files: u64,
    #[serde(default)]
    pub missing_favorite_files: u64,
}
