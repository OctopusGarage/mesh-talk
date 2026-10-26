//! Sticker presentation rides the existing media manifest, authorization and custody.
//! Reuse is deliberately local to one host and deadline, never a public hash lookup.

use super::{Node, NodeError};
use crate::eventlog::{Author, ConversationId};
use crate::file::{AnyManifest, FileKind, FileManifestV3, file_checksum, sticker};
use std::io::{Read, Write};
use std::path::Path;

pub(super) fn snapshot(path: &Path, kind: FileKind) -> Result<Option<Vec<u8>>, NodeError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if !sticker::is_sticker_name(name) {
        return Ok(None);
    }
    if kind != FileKind::Media {
        return Err(NodeError::InvalidInput(
            "stickers must be inline media".into(),
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .and_then(|file| {
            file.take(sticker::MAX_STICKER_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|error| NodeError::File(format!("read sticker: {error}")))?;
    validate(name, &super::files::mime_from_name(name), &bytes)?;
    Ok(Some(bytes))
}

pub(super) fn validate(name: &str, mime: &str, bytes: &[u8]) -> Result<(), NodeError> {
    if sticker::is_sticker_name(name) {
        sticker::validate_image(name, mime, bytes).map_err(NodeError::InvalidInput)?;
    }
    Ok(())
}

impl Node {
    pub(super) fn reusable_sticker(
        &self,
        host: ConversationId,
        account: Option<&str>,
        name: &str,
        bytes: &[u8],
        deadline: Option<u64>,
    ) -> Option<FileManifestV3> {
        let checksum = file_checksum(bytes);
        let own = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Canonical projection excludes recalled, locally erased and expired rows.
        // Bound the optimization: a miss always stages a fresh authorized transfer.
        let history = account.map_or_else(
            || self.history(host, 256),
            |account| self.account_history(account, 256),
        );
        for row in history.into_iter().rev() {
            if !row.from_me || row.recalled || row.expires_at_ms != deadline {
                continue;
            }
            let Some(file) = row.file.filter(|file| file.media && file.name == name) else {
                continue;
            };
            let manifest = {
                let book = self.files.lock().expect("files lock not poisoned");
                if !book.has_author(file.file_conv, own) {
                    continue;
                }
                book.manifest(&file.file_conv).cloned()
            };
            let Some(AnyManifest::V3(manifest)) = manifest else {
                continue;
            };
            if manifest.v2.checksum == checksum
                && manifest.v2.size == bytes.len() as u64
                // Verify retained signed chunks, not only a stale cache index or path.
                && self.read_file_internal(file.file_conv).ok().as_deref() == Some(bytes)
            {
                return Some(manifest);
            }
        }
        None
    }

    pub(super) fn stage_sticker_snapshot(
        &self,
        name: &str,
        bytes: &[u8],
        on_progress: impl FnMut(super::FileProgress),
    ) -> Result<(FileManifestV3, ConversationId), NodeError> {
        // Validation and transfer must use exactly the same bytes even if a local
        // source changes while staging. tempfile is private and removed on drop.
        let mut source = tempfile::NamedTempFile::new()
            .map_err(|error| NodeError::File(format!("snapshot sticker: {error}")))?;
        source
            .write_all(bytes)
            .map_err(|error| NodeError::File(error.to_string()))?;
        self.stage_regular_file(
            source.path(),
            FileKind::Media,
            on_progress,
            Some((name.to_owned(), &super::files::mime_from_name(name))),
        )
    }
}
