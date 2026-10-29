//! Sticker presentation rides the existing media manifest, authorization and custody.
//! Reuse is deliberately local to one host and deadline, never a public hash lookup.

use super::{Node, NodeError};
use crate::eventlog::{Author, ConversationId, Event, EventId, EventKind};
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
    ) -> Result<Option<FileManifestV3>, NodeError> {
        let checksum = file_checksum(bytes);
        let own = Author::from_ed25519(self.identity.public().ed25519_pub);
        // Canonical projection excludes recalled, locally erased and expired rows.
        // Bound the optimization: a miss always stages a fresh authorized transfer.
        let history = account.map_or_else(
            || self.history(host, 256),
            |account| self.account_history(account, 256),
        )?;
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
                return Ok(Some(manifest));
            }
        }
        Ok(None)
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

/// Validated, device-local candidate bytes. This cache grants no delivery authority.
#[derive(Default)]
pub(super) struct StickerSources {
    entries: std::collections::VecDeque<([u8; 32], std::sync::Arc<[u8]>)>,
    bytes: usize,
}

impl Node {
    /// Register installed sticker artwork for exact-byte reuse. Candidates are
    /// session-local and bounded; every incoming offer is authenticated separately.
    pub fn register_sticker_source(&self, name: &str, bytes: &[u8]) -> Result<(), NodeError> {
        sticker::validate_image(name, &super::files::mime_from_name(name), bytes)
            .map_err(NodeError::InvalidInput)?;
        let checksum = file_checksum(bytes);
        let mut sources = self.sticker_sources.lock().expect("sticker sources lock");
        if sources
            .entries
            .iter()
            .any(|(hash, stored)| *hash == checksum && stored.as_ref() == bytes)
        {
            drop(sources);
            self.file_pull_notify.notify_one();
            return Ok(());
        }
        while sources.entries.len() >= 128 || sources.bytes + bytes.len() > 32 * 1024 * 1024 {
            let Some((_, old)) = sources.entries.pop_front() else {
                break;
            };
            sources.bytes -= old.len();
        }
        sources.bytes += bytes.len();
        sources
            .entries
            .push_back((checksum, std::sync::Arc::from(bytes)));
        drop(sources);
        self.file_pull_notify.notify_one();
        Ok(())
    }

    /// Transport only original-author metadata; never disclose candidate hashes.
    pub(super) fn sticker_chunk_proof(
        &self,
        file: ConversationId,
    ) -> Result<Option<Vec<Event>>, crate::eventlog::LogError> {
        let Some(manifest) = self
            .files
            .lock()
            .expect("files lock")
            .manifest(&file)
            .cloned()
        else {
            return Ok(None);
        };
        let AnyManifest::V3(manifest) = manifest else {
            return Ok(None);
        };
        if manifest.kind != FileKind::Media
            || !sticker::is_sticker_name(&manifest.v2.name)
            || manifest.v2.size == 0
            || manifest.v2.size > sticker::MAX_STICKER_BYTES as u64
            || manifest.v2.chunk_count > 64
        {
            return Ok(None);
        }
        let log = self.log.lock().expect("log lock");
        let chunks = log.events(&file)?;
        if chunks.len() != manifest.v2.chunk_count as usize {
            return Ok(None);
        }
        let own = self.identity.public();
        let mut headers = Vec::with_capacity(chunks.len());
        for (index, chunk) in chunks.iter().enumerate() {
            if chunk.author.ed25519_pub() != &own.ed25519_pub
                || chunk.kind != EventKind::Message
                || chunk.seq != index as u64 + 1
                || !chunk.verify_integrity()
                || !chunk.verify_signature()
                || chunk.parents
                    != if index == 0 {
                        vec![]
                    } else {
                        vec![chunks[index - 1].id]
                    }
            {
                return Ok(None);
            }
            let mut header = (*chunk).clone();
            header.ciphertext.clear();
            headers.push(header);
        }
        Ok(Some(headers))
    }

    pub(super) fn sticker_recovery_offer(
        &self,
        file: ConversationId,
        peer: &crate::identity::device::PublicIdentity,
    ) -> Result<Option<EventId>, crate::eventlog::LogError> {
        let (manifest, ids) = {
            let files = self.files.lock().expect("files lock");
            let Some(manifest) = files.manifest(&file).cloned() else {
                return Ok(None);
            };
            (manifest, files.manifest_events(file))
        };
        if !matches!(&manifest, AnyManifest::V3(media) if media.kind == FileKind::Media)
            || !sticker::is_sticker_name(manifest.name())
            || !self
                .sticker_sources
                .lock()
                .expect("sticker sources lock")
                .entries
                .iter()
                .any(|(hash, bytes)| {
                    *hash == manifest.checksum() && bytes.len() as u64 == manifest.size()
                })
        {
            return Ok(None);
        }
        for id in ids {
            if self.incoming_offer(file, Some(id), Some(peer))?.is_some() {
                return Ok(Some(id));
            }
        }
        Ok(None)
    }

    /// Verify a complete proof before exposing any reconstructed event to admission.
    pub(super) fn reconstruct_sticker_proof(
        &self,
        file: ConversationId,
        offer: EventId,
        peer: &crate::identity::device::PublicIdentity,
        mut headers: Vec<Event>,
    ) -> Result<Option<Vec<Event>>, crate::eventlog::LogError> {
        if headers.is_empty()
            || headers.len() > 64
            || self.expired_received_file(file)?
            || self
                .delivery_suspended
                .load(std::sync::atomic::Ordering::Acquire)
            || !self.known_account_allowed(peer)
        {
            return Ok(None);
        }
        if self
            .incoming_offer(file, Some(offer), Some(peer))?
            .is_none()
        {
            return Ok(None);
        }
        let Some(manifest) = self
            .files
            .lock()
            .expect("files lock")
            .manifest(&file)
            .cloned()
        else {
            return Ok(None);
        };
        let AnyManifest::V3(ref media) = manifest else {
            return Ok(None);
        };
        if media.kind != FileKind::Media
            || !sticker::is_sticker_name(manifest.name())
            || media.v2.chunk_size != crate::file::CHUNK_SIZE as u32
            || media.v2.chunk_count as usize != headers.len()
        {
            return Ok(None);
        }
        let Some(card) = self
            .delivery
            .lock()
            .expect("delivery lock")
            .file_card(offer)
            .cloned()
        else {
            return Ok(None);
        };
        let Some(binding) = card.completion_binding.as_ref() else {
            return Ok(None);
        };
        if card.file_conversation != file
            || binding.source != *peer
            || binding.owner_account != self.account_id()
            || self
                .delivery
                .lock()
                .expect("delivery lock")
                .manifest_event_erased(offer)
        {
            return Ok(None);
        }
        let Some(source) = self.historical_author(&peer.ed25519_pub) else {
            return Ok(None);
        };
        if source.public() != *peer || source.account_cert != binding.certificate {
            return Ok(None);
        }
        let Some(row) = self
            .received_files
            .lock()
            .expect("received files lock")
            .entry(offer)?
        else {
            return Ok(None);
        };
        if super::files::validated_manifest(&row.plaintext).as_ref() != Some(&manifest)
            || row.from != peer.user_id()
            || crate::expiry::deadline(&row.plaintext)
                .is_some_and(|deadline| deadline <= super::node::now_millis())
        {
            return Ok(None);
        }
        {
            let log = self.log.lock().expect("log lock");
            let Some(event) = log.get(&offer)? else {
                return Ok(None);
            };
            if event.kind != EventKind::FileManifest
                || event.author.ed25519_pub() != &peer.ed25519_pub
                || event.conversation_id
                    != super::conversation::dm_conversation_id(&self.identity.public(), peer)
                || !event.is_canonical()
                || !event.verify_integrity()
                || !event.verify_signature()
            {
                return Ok(None);
            }
        }
        let Some(bytes) = self
            .sticker_sources
            .lock()
            .expect("sticker sources lock")
            .entries
            .iter()
            .find(|(hash, bytes)| {
                *hash == manifest.checksum() && bytes.len() as u64 == manifest.size()
            })
            .map(|(_, bytes)| std::sync::Arc::clone(bytes))
        else {
            return Ok(None);
        };
        if sticker::validate_image(manifest.name(), manifest.mime(), &bytes).is_err() {
            return Ok(None);
        }
        if file_checksum(&bytes) != manifest.checksum()
            || crate::file::chunk_count_for(bytes.len() as u64) as usize != headers.len()
        {
            return Ok(None);
        }
        let mut previous = None;
        for (index, (header, chunk)) in headers
            .iter_mut()
            .zip(bytes.chunks(crate::file::CHUNK_SIZE))
            .enumerate()
        {
            if !header.ciphertext.is_empty()
                || header.conversation_id != file
                || header.author.ed25519_pub() != &peer.ed25519_pub
                || header.seq != index as u64 + 1
                || header.kind != EventKind::Message
                || header.parents != previous.into_iter().collect::<Vec<_>>()
            {
                return Ok(None);
            }
            let Ok(ciphertext) = crate::file::seal_chunk_indexed(
                &media.v2.key(),
                &media.v2.file_nonce,
                index as u32,
                chunk,
            ) else {
                return Ok(None);
            };
            header.ciphertext = ciphertext;
            if !header.is_canonical()
                || !header.verify_integrity()
                || !header.verify_signature()
                || !crate::file::open_chunk_for(&manifest, index as u32, &header.ciphertext)
                    .is_ok_and(|opened| opened.as_slice() == chunk)
            {
                return Ok(None);
            }
            previous = Some(header.id);
        }
        Ok(Some(headers))
    }
}

#[cfg(test)]
mod source_cache_tests {
    use super::*;
    use crate::identity::{account::Account, device::DeviceIdentity};
    use base64::Engine;

    const NAME: &str = "mt-sticker-v1-cGFjazpleGFtcGxlOndhdmUK8J-Riw.png";

    fn node() -> (tempfile::TempDir, std::sync::Arc<Node>) {
        let dir = tempfile::tempdir().unwrap();
        let node = super::super::sticker_media_tests::open(
            dir.path(),
            DeviceIdentity::generate(),
            Account::generate(),
        );
        (dir, node)
    }

    fn artwork(identity: u32, size: usize) -> Vec<u8> {
        let mut bytes = base64::engine::general_purpose::STANDARD.decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==").unwrap();
        bytes.resize(size.max(bytes.len() + 4), 0);
        let end = bytes.len();
        bytes[end - 4..].copy_from_slice(&identity.to_le_bytes());
        bytes
    }

    #[test]
    fn installed_source_count_limit_evicts_old_artwork_without_affecting_new_candidates() {
        let (_dir, node) = node();
        for identity in 0..129 {
            node.register_sticker_source(NAME, &artwork(identity, 0))
                .unwrap();
        }
        let oldest_hash = file_checksum(&artwork(0, 0));
        let newest = artwork(128, 0);
        let newest_hash = file_checksum(&newest);
        let sources = node.sticker_sources.lock().unwrap();
        assert_eq!(sources.entries.len(), 128);
        assert_eq!(sources.bytes, 128 * artwork(0, 0).len());
        assert!(!sources.entries.iter().any(|(hash, _)| *hash == oldest_hash));
        assert!(
            sources
                .entries
                .iter()
                .any(|(hash, bytes)| *hash == newest_hash && bytes.as_ref() == newest.as_slice())
        );
        assert_eq!(
            sources
                .entries
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>(),
            sources.bytes
        );
    }

    #[test]
    fn installed_source_byte_budget_evicts_before_count_limit_and_accepts_reinstallation() {
        let (_dir, node) = node();
        for identity in 0..11 {
            node.register_sticker_source(NAME, &artwork(identity, sticker::MAX_STICKER_BYTES))
                .unwrap();
        }
        let evicted_hash = file_checksum(&artwork(0, sticker::MAX_STICKER_BYTES));
        {
            let sources = node.sticker_sources.lock().unwrap();
            assert_eq!(
                sources.entries.len(),
                10,
                "the byte budget applies before the 128-image count limit"
            );
            assert_eq!(sources.bytes, 10 * sticker::MAX_STICKER_BYTES);
            assert!(sources.bytes <= 32 * 1024 * 1024);
            assert!(
                !sources
                    .entries
                    .iter()
                    .any(|(hash, _)| *hash == evicted_hash)
            );
        }
        let restored = artwork(0, sticker::MAX_STICKER_BYTES);
        node.register_sticker_source(NAME, &restored).unwrap();
        let sources = node.sticker_sources.lock().unwrap();
        assert_eq!(sources.entries.len(), 10);
        assert_eq!(sources.bytes, 10 * sticker::MAX_STICKER_BYTES);
        assert!(
            sources
                .entries
                .iter()
                .any(|(_, bytes)| bytes.as_ref() == restored.as_slice())
        );
        assert_eq!(
            sources
                .entries
                .iter()
                .map(|(_, bytes)| bytes.len())
                .sum::<usize>(),
            sources.bytes
        );
    }

    #[tokio::test]
    async fn duplicate_registration_wakes_recovery_without_growth_and_invalid_input_cannot_mutate_cache()
     {
        let (_dir, node) = node();
        let bytes = artwork(7, 0);
        node.register_sticker_source(NAME, &bytes).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            node.file_pull_notify.notified(),
        )
        .await
        .expect("new installed artwork wakes pending recovery");
        let expected_hash = file_checksum(&bytes);
        for (name, invalid) in [
            (NAME, Vec::new()),
            (NAME, b"not an image".to_vec()),
            (NAME, artwork(9, sticker::MAX_STICKER_BYTES + 1)),
            ("ordinary.png", bytes.clone()),
        ] {
            assert!(node.register_sticker_source(name, &invalid).is_err());
            let sources = node.sticker_sources.lock().unwrap();
            assert_eq!(sources.entries.len(), 1);
            assert_eq!(sources.bytes, bytes.len());
            assert_eq!(sources.entries[0].0, expected_hash);
            assert_eq!(sources.entries[0].1.as_ref(), bytes.as_slice());
        }
        node.register_sticker_source(NAME, &bytes).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            node.file_pull_notify.notified(),
        )
        .await
        .expect("idempotent registration still retries a pending offer");
        let sources = node.sticker_sources.lock().unwrap();
        assert_eq!(sources.entries.len(), 1);
        assert_eq!(sources.bytes, bytes.len());
        assert_eq!(sources.entries[0].0, expected_hash);
    }
}
