//! Optional Favorites protocol, separately versioned from frozen history encodings.
use super::super::favorites::{FavoriteRecord, MAX_FAVORITE_BYTES, MAX_FAVORITE_FILE_BYTES};
use super::*;
use std::ops::Bound::{Excluded, Unbounded};
const PAGE: usize = 8;
async fn bounded_favorite_exchange(
    deadline: Duration,
    work: impl std::future::Future<Output = ()>,
) -> bool {
    tokio::time::timeout(deadline, work).await.is_ok()
}
impl Node {
    pub(in crate::node) async fn serve_message_favorites(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        account: &str,
        after: Option<String>,
        known_digest: [u8; 32],
    ) {
        let _ = bounded_favorite_exchange(
            Duration::from_secs(20),
            self.serve_message_favorites_inner(channel, account, after, known_digest),
        )
        .await;
    }
    pub(in crate::node) async fn serve_favorite_attachment(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        account: &str,
        file: ConversationId,
    ) {
        let size = self
            .favorite_file_size(file)
            .unwrap_or(0)
            .min(MAX_FAVORITE_FILE_BYTES);
        let _ = bounded_favorite_exchange(
            attachment_deadline(size),
            self.serve_favorite_attachment_inner(channel, account, file),
        )
        .await;
    }

    async fn serve_message_favorites_inner(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        account: &str,
        after: Option<String>,
        known_digest: [u8; 32],
    ) {
        if account != self.account_id()
            || !channel.io_admitted()
            || !self.certified_sibling(channel.peer_identity())
        {
            return;
        }
        let (policy, revision) = self.device_sync_policy_snapshot();
        if !policy.history {
            return;
        }
        let Ok((records, digest)) = self
            .account_blocking(move |node| {
                let store = node.favorites.lock().expect("favorites mutex");
                let digest = store.digest();
                if after.is_none() && digest == known_digest {
                    return Ok((Vec::new(), digest));
                }
                let mut records = Vec::new();
                for (_, record) in store
                    .current
                    .range::<String, _>((after.as_ref().map_or(Unbounded, Excluded), Unbounded))
                    .filter(|(_, record)| !node.favorite_has_protected_content(&record.favorite))
                    .take(PAGE + 1)
                {
                    records.push(record.clone());
                }
                Ok((records, digest))
            })
            .await
        else {
            return;
        };
        let next = if records.len() > PAGE {
            records.get(PAGE - 1).map(|r| r.favorite.id.clone())
        } else {
            None
        };
        for mut record in records.into_iter().take(PAGE) {
            record.favorite = super::super::favorites::project(record.favorite, self.expiry_now());
            let Ok(bytes) = bincode::serialize(&record) else {
                return;
            };
            if self
                .send_account_frame(
                    channel,
                    AccountSyncVersion::V2,
                    revision,
                    &AccountHistoryFrame::FavoriteHeader {
                        id: record.favorite.id,
                        total_len: bytes.len() as u32,
                    },
                )
                .await
                .is_err()
            {
                return;
            }
            for chunk in bytes.chunks(MAX_CHUNK) {
                if self
                    .send_account_frame(
                        channel,
                        AccountSyncVersion::V2,
                        revision,
                        &AccountHistoryFrame::FavoriteChunk {
                            bytes: chunk.to_vec(),
                        },
                    )
                    .await
                    .is_err()
                {
                    return;
                }
            }
        }
        let _ = self
            .send_account_frame(
                channel,
                AccountSyncVersion::V2,
                revision,
                &AccountHistoryFrame::FavoritesComplete { next, digest },
            )
            .await;
    }
    #[cfg(test)]
    pub(in crate::node) async fn pull_message_favorites_page(
        self: &Arc<Self>,
        peer: &PeerRecord,
        after: Option<String>,
    ) -> Result<Option<String>, NodeError> {
        self.pull_message_favorites_page_at_digest(peer, after, [0; 32], &mut false)
            .await
            .map(|(next, _)| next)
    }
    pub(super) async fn pull_message_favorites_page_at_digest(
        self: &Arc<Self>,
        peer: &PeerRecord,
        after: Option<String>,
        known_digest: [u8; 32],
        supported: &mut bool,
    ) -> Result<(Option<String>, [u8; 32]), NodeError> {
        if peer.account_id.as_deref() != Some(self.account_id().as_str())
            || !self.certified_sibling(&peer.public)
        {
            return Err(NodeError::Authorization(
                "favorites require a certified sibling".into(),
            ));
        }
        let (policy, revision) = self.device_sync_policy_snapshot();
        if !policy.history {
            return Ok((None, [0; 32]));
        }
        let mut channel = self
            .privacy_dial(peer.addr, &peer.public)
            .await
            .map_err(transport)?;
        self.send_account_frame(
            &mut channel,
            AccountSyncVersion::V2,
            revision,
            &AccountHistoryFrame::FavoritesRequest {
                version: 1,
                account_id: self.account_id(),
                after: after.clone(),
                known_digest,
                attachments: policy.attachments,
            },
        )
        .await?;
        let mut pending: Option<(String, usize, Vec<u8>)> = None;
        let mut records = Vec::new();
        let mut last = after;
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::Channel("favorites read timed out".into()))?
                .map_err(transport)?;
            *supported = true;
            let Some((AccountSyncVersion::V2, frame)) =
                AccountHistoryFrame::decode_versioned(&bytes)
            else {
                return Err(NodeError::Channel("invalid favorites response".into()));
            };
            match frame {
                AccountHistoryFrame::FavoriteHeader { id, total_len }
                    if pending.is_none()
                        && records.len() < PAGE
                        && last.as_ref().is_none_or(|last| &id > last) =>
                {
                    pending = Some((id, total_len as usize, Vec::new()));
                }
                AccountHistoryFrame::FavoriteChunk { bytes } if pending.is_some() => {
                    let (id, total, data) = pending.as_mut().unwrap();
                    if data.len() + bytes.len() > *total {
                        return Err(NodeError::Channel("oversized favorite".into()));
                    }
                    data.extend(bytes);
                    if data.len() == *total {
                        let mut record: FavoriteRecord = bincode::DefaultOptions::new()
                            .with_fixint_encoding()
                            .with_limit(MAX_FAVORITE_BYTES as u64)
                            .reject_trailing_bytes()
                            .deserialize(data)
                            .map_err(|_| NodeError::Channel("invalid favorite record".into()))?;
                        if !record.valid() || record.favorite.id != *id {
                            return Err(NodeError::Channel("invalid favorite record".into()));
                        }
                        record.favorite =
                            super::super::favorites::project(record.favorite, self.expiry_now());
                        last = Some(id.clone());
                        records.push(record);
                        pending = None;
                    }
                }
                AccountHistoryFrame::FavoritesComplete { next, digest }
                    if pending.is_none()
                        && (next.is_none() || (records.len() == PAGE && next == last)) =>
                {
                    self.account_blocking(move |node| {
                        let _commit = node.account_sync_commit.lock().expect("sync commit mutex");
                        let (policy, current) = node.device_sync_policy_snapshot();
                        if current != revision || !policy.history {
                            return Err(NodeError::Authorization(
                                "favorites sync policy changed".into(),
                            ));
                        }
                        let records: Vec<_> = records
                            .into_iter()
                            .filter(|record| !node.favorite_has_protected_content(&record.favorite))
                            .collect();
                        let mut store = node.favorites.lock().expect("favorites mutex");
                        let mut changed = false;
                        for record in records {
                            changed |= store.merge(record).map_err(NodeError::Log)?;
                        }
                        node.favorite_media
                            .prune_unreferenced(&store.live_files(node.expiry_now()))
                            .map_err(|e| NodeError::File(e.to_string()))?;
                        drop(store);
                        if changed {
                            node.note_account_history_change();
                            node.account_sync_notify.notify_one();
                        }
                        Ok(())
                    })
                    .await?;
                    return Ok((next, digest));
                }
                _ => return Err(NodeError::Channel("unexpected favorites response".into())),
            }
        }
    }
    async fn serve_favorite_attachment_inner(
        self: &Arc<Self>,
        channel: &mut SecureChannel<TcpStream>,
        account: &str,
        file: ConversationId,
    ) {
        if account != self.account_id()
            || !channel.io_admitted()
            || !self.certified_sibling(channel.peer_identity())
        {
            return;
        }
        let (policy, revision) = self.device_sync_policy_snapshot();
        if !policy.history || !policy.attachments {
            return;
        }
        let Ok(Some(bytes)) = self
            .account_blocking(move |node| Ok(node.read_favorite_file(file)))
            .await
        else {
            return;
        };
        let checksum: [u8; 32] = Sha256::digest(&bytes).into();
        if self
            .send_account_frame(
                channel,
                AccountSyncVersion::V2,
                revision,
                &AccountHistoryFrame::AttachmentStart {
                    file_conv: *file.as_bytes(),
                    name: "favorite.bin".into(),
                    size: bytes.len() as u64,
                    sha256: checksum,
                },
            )
            .await
            .is_err()
        {
            return;
        }
        for chunk in bytes.chunks(MAX_CHUNK) {
            if self
                .send_account_frame(
                    channel,
                    AccountSyncVersion::V2,
                    revision,
                    &AccountHistoryFrame::AttachmentChunk {
                        bytes: chunk.to_vec(),
                    },
                )
                .await
                .is_err()
            {
                return;
            }
        }
        let _ = self
            .send_account_frame(
                channel,
                AccountSyncVersion::V2,
                revision,
                &AccountHistoryFrame::AttachmentComplete,
            )
            .await;
    }
    pub(super) async fn pull_favorite_attachment(
        self: &Arc<Self>,
        peer: &PeerRecord,
        file: ConversationId,
        revision: u64,
    ) -> Result<(), NodeError> {
        let mut channel = self
            .privacy_dial(peer.addr, &peer.public)
            .await
            .map_err(transport)?;
        self.send_account_frame(
            &mut channel,
            AccountSyncVersion::V2,
            revision,
            &AccountHistoryFrame::FavoriteAttachmentRequest {
                version: 1,
                account_id: self.account_id(),
                file_conv: *file.as_bytes(),
            },
        )
        .await?;
        let mut pending: Option<(usize, [u8; 32], Vec<u8>)> = None;
        loop {
            let bytes = tokio::time::timeout(READ_TIMEOUT, channel.recv())
                .await
                .map_err(|_| NodeError::Channel("favorite attachment timed out".into()))?
                .map_err(transport)?;
            let Some((AccountSyncVersion::V2, frame)) =
                AccountHistoryFrame::decode_versioned(&bytes)
            else {
                return Err(NodeError::Channel("invalid favorite attachment".into()));
            };
            match frame {
                AccountHistoryFrame::AttachmentStart {
                    file_conv,
                    size,
                    sha256,
                    ..
                } if pending.is_none()
                    && file_conv == *file.as_bytes()
                    && size <= MAX_FAVORITE_FILE_BYTES =>
                {
                    pending = Some((size as usize, sha256, Vec::new()));
                }
                AccountHistoryFrame::AttachmentChunk { bytes } if pending.is_some() => {
                    let (size, _, data) = pending.as_mut().unwrap();
                    if data.len() + bytes.len() > *size {
                        return Err(NodeError::Channel("oversized favorite attachment".into()));
                    }
                    data.extend(bytes);
                }
                AccountHistoryFrame::AttachmentComplete if pending.is_some() => {
                    let (size, checksum, data) = pending.take().unwrap();
                    if data.len() != size || <[u8; 32]>::from(Sha256::digest(&data)) != checksum {
                        return Err(NodeError::Channel(
                            "favorite attachment checksum mismatch".into(),
                        ));
                    }
                    return self
                        .account_blocking(move |node| {
                            let _commit =
                                node.account_sync_commit.lock().expect("sync commit mutex");
                            let (policy, current) = node.device_sync_policy_snapshot();
                            if current != revision || !policy.history || !policy.attachments {
                                return Err(NodeError::Authorization(
                                    "favorite attachment policy changed".into(),
                                ));
                            }
                            let store = node.favorites.lock().expect("favorites mutex");
                            let metadata = store
                                .current
                                .values()
                                .filter(|r| !r.removed)
                                .filter(|r| !node.favorite_has_protected_content(&r.favorite))
                                .flat_map(|r| &r.favorite.messages)
                                .filter(|m| {
                                    m.expires_at_ms.is_none_or(|end| end > node.expiry_now())
                                })
                                .filter_map(|m| m.file.as_ref())
                                .find(|f| {
                                    f.file_conv == hex::encode(file.as_bytes())
                                        && f.size == size as u64
                                })
                                .ok_or_else(|| {
                                    NodeError::Authorization(
                                        "favorite attachment unavailable".into(),
                                    )
                                })?;
                            node.favorite_media
                                .store_bytes(file, &metadata.name, &data)
                                .map_err(|e| NodeError::File(e.to_string()))?;
                            Ok(())
                        })
                        .await;
                }
                _ => {
                    return Err(NodeError::Channel(
                        "unexpected favorite attachment response".into(),
                    ));
                }
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::super::selection_tests::{sibling_nodes, sibling_peer};
    #[tokio::test]
    async fn favorites_sync_excludes_known_protected_sources_on_send_and_receive() {
        for protect_sender in [true, false] {
            let (_dir, source, receiver) = sibling_nodes();
            for node in [&source, &receiver] {
                node.set_device_sync_category(super::SyncCategory::History, true)
                    .unwrap();
            }
            let peer = "ab".repeat(16);
            let conv = super::super::super::conversation::account_conversation_id(
                &source.account_id(),
                &peer,
            );
            let envelope = super::super::super::DmEnvelope::new(
                source.account_id(),
                peer.clone(),
                [8; 32],
                super::super::super::MessageBody::new(b"ordinary".to_vec(), None).encode(),
            );
            source
                .sentlog
                .lock()
                .unwrap()
                .record(conv, 1, 1, &envelope.encode())
                .unwrap();
            let mut favorite = source
                .save_message_favorite_guarded(
                    crate::node::FavoriteSource {
                        kind: "account".into(),
                        target: peer.clone(),
                        message_ids: vec![hex::encode([8; 32])],
                    },
                    Default::default(),
                    |c| c(),
                )
                .unwrap();
            let protected_node = if protect_sender { &source } else { &receiver };
            let envelope = super::super::super::DmEnvelope::new(
                source.account_id(),
                peer.clone(),
                [9; 32],
                super::super::super::MessageBody::protected(
                    b"private".to_vec(),
                    None,
                    crate::node::MessageProtection::Authenticate,
                )
                .encode(),
            );
            protected_node
                .sentlog
                .lock()
                .unwrap()
                .record(conv, 2, 2, &envelope.encode())
                .unwrap();
            favorite.source.message_ids = vec![hex::encode([9; 32])];
            favorite.messages[0].id = hex::encode([9; 32]);
            favorite.messages[0].text = "private".into();
            favorite.revision += 1;
            source
                .favorites
                .lock()
                .unwrap()
                .merge(super::FavoriteRecord {
                    generation: 1,
                    favorite,
                    removed: false,
                })
                .unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
            let server_node = source.clone();
            let server = tokio::spawn(async move {
                loop {
                    let (stream, _) = listener.accept().await.unwrap();
                    let node = server_node.clone();
                    tokio::spawn(async move {
                        let channel = node.privacy_accept(stream).await.unwrap();
                        node.serve_connection(channel).await;
                    });
                }
            });
            receiver
                .pull_message_favorites_page(&peer, None)
                .await
                .unwrap();
            assert!(
                receiver.favorites.lock().unwrap().current.is_empty(),
                "protected snapshot must not enter sibling store"
            );
            assert!(
                receiver
                    .list_message_favorites("private", 0, 10)
                    .unwrap()
                    .items
                    .is_empty()
            );
            server.abort();
            let _ = server.await;
        }
    }
    #[tokio::test]
    async fn favorite_sync_catches_up_edits_and_tombstones_without_source_history() {
        let (_dir, source, receiver) = sibling_nodes();
        let id = crate::eventlog::event::EventId::new([9; 32]);
        let peer = "ab".repeat(16);
        let conv =
            super::super::super::conversation::account_conversation_id(&source.account_id(), &peer);
        let env = super::super::super::DmEnvelope::new(
            source.account_id(),
            peer.clone(),
            *id.as_bytes(),
            super::super::super::MessageBody::new(b"important".to_vec(), None).encode(),
        );
        source
            .sentlog
            .lock()
            .unwrap()
            .record(conv, 1, 1, &env.encode())
            .unwrap();
        let mut f = source
            .save_message_favorite_guarded(
                super::super::super::FavoriteSource {
                    kind: "account".into(),
                    target: peer,
                    message_ids: vec![id.to_hex()],
                },
                Default::default(),
                |c| c(),
            )
            .unwrap();
        source.clear_account_conversation(&f.source.target).unwrap();
        let file = crate::eventlog::event::ConversationId::new([11; 32]);
        let contents = b"retained bytes";
        f.messages[0].file = Some(crate::node::FavoriteFile {
            file_conv: hex::encode(file.as_bytes()),
            name: "favorite.txt".into(),
            size: contents.len() as u64,
            mime: "text/plain".into(),
            media: false,
        });
        f.revision += 1;
        source
            .favorites
            .lock()
            .unwrap()
            .merge(super::FavoriteRecord {
                generation: 1,
                favorite: f.clone(),
                removed: false,
            })
            .unwrap();
        source
            .favorite_media
            .store_bytes(file, "favorite.txt", contents)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let peer = sibling_peer(&receiver, &source, listener.local_addr().unwrap());
        let server_node = source.clone();
        let server = tokio::spawn(async move {
            loop {
                let (stream, _) = listener.accept().await.unwrap();
                let node = server_node.clone();
                tokio::spawn(async move {
                    let channel = node.privacy_accept(stream).await.unwrap();
                    node.serve_connection(channel).await;
                });
            }
        });
        receiver
            .set_device_sync_category(super::SyncCategory::History, false)
            .unwrap();
        receiver
            .pull_message_favorites_page(&peer, None)
            .await
            .unwrap();
        assert!(
            receiver
                .list_message_favorites("", 0, 10)
                .unwrap()
                .items
                .is_empty()
        );
        receiver
            .set_device_sync_category(super::SyncCategory::History, true)
            .unwrap();
        source
            .set_device_sync_category(super::SyncCategory::History, false)
            .unwrap();
        assert!(
            receiver
                .pull_message_favorites_page(&peer, None)
                .await
                .is_err()
        );
        source
            .set_device_sync_category(super::SyncCategory::History, true)
            .unwrap();
        receiver
            .pull_message_favorites_page(&peer, None)
            .await
            .unwrap();
        assert_eq!(
            receiver
                .list_message_favorites("important", 0, 10)
                .unwrap()
                .items
                .len(),
            1
        );
        let (_, attachment_revision) = receiver.device_sync_policy_snapshot();
        let _ = receiver
            .pull_favorite_attachment(&peer, file, attachment_revision)
            .await;
        assert!(!receiver.favorite_file_available(file));
        receiver
            .set_device_sync_category(super::SyncCategory::Attachments, true)
            .unwrap();
        receiver
            .pull_message_favorites_page(&peer, None)
            .await
            .unwrap();
        let (_, attachment_revision) = receiver.device_sync_policy_snapshot();
        let _ = receiver
            .pull_favorite_attachment(&peer, file, attachment_revision)
            .await;
        assert!(!receiver.favorite_file_available(file));
        source
            .set_device_sync_category(super::SyncCategory::Attachments, true)
            .unwrap();
        receiver
            .pull_message_favorites_page(&peer, None)
            .await
            .unwrap();
        let (_, attachment_revision) = receiver.device_sync_policy_snapshot();
        receiver
            .pull_favorite_attachment(&peer, file, attachment_revision)
            .await
            .unwrap();
        assert_eq!(receiver.read_favorite_file(file).unwrap(), contents);
        let (_other_dir, _, unrelated) = sibling_nodes();
        let foreign_peer = sibling_peer(&unrelated, &source, peer.addr);
        assert!(
            unrelated
                .pull_message_favorites_page(&foreign_peer, None)
                .await
                .is_err()
        );
        let mut foreign_channel = unrelated
            .privacy_dial(peer.addr, &source.identity.public())
            .await
            .unwrap();
        super::send_frame(
            &mut foreign_channel,
            super::AccountSyncVersion::V2,
            &super::AccountHistoryFrame::FavoritesRequest {
                version: 1,
                account_id: source.account_id(),
                after: None,
                known_digest: [0; 32],
                attachments: false,
            },
        )
        .await
        .unwrap();
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(2), foreign_channel.recv())
                .await
                .unwrap()
                .is_err()
        );
        let f = source
            .edit_message_favorite_guarded(
                &f.id,
                f.revision,
                &f.device_id,
                super::super::super::FavoriteMetadata {
                    description: "deployment notes".into(),
                    ..Default::default()
                },
                |c| c(),
            )
            .unwrap();
        receiver
            .pull_message_favorites_page(&peer, None)
            .await
            .unwrap();
        assert_eq!(
            receiver
                .list_message_favorites("deployment", 0, 10)
                .unwrap()
                .items
                .len(),
            1
        );
        source
            .remove_message_favorite_guarded(&f.id, f.revision, &f.device_id, |c| c())
            .unwrap();
        receiver
            .pull_message_favorites_page(&peer, None)
            .await
            .unwrap();
        assert!(
            receiver
                .list_message_favorites("", 0, 10)
                .unwrap()
                .items
                .is_empty()
        );
        // Journal pages advance without waiting for retained attachment bytes.
        for index in 1..=17u8 {
            let mut item = f.clone();
            item.id = hex::encode([index; 32]);
            item.source.message_ids = vec![hex::encode([index; 32])];
            item.messages.truncate(1);
            item.messages[0].id = hex::encode([index; 32]);
            item.metadata = Default::default();
            source
                .favorites
                .lock()
                .unwrap()
                .merge(super::FavoriteRecord {
                    generation: 1,
                    favorite: item,
                    removed: false,
                })
                .unwrap();
        }
        let mut after = None;
        let mut pages = 0;
        let mut supported = false;
        let digest;
        loop {
            let (next, current) = receiver
                .pull_message_favorites_page_at_digest(&peer, after, [0; 32], &mut supported)
                .await
                .unwrap();
            pages += 1;
            if next.is_none() {
                digest = current;
                break;
            }
            after = next;
        }
        assert_eq!(pages, 3);
        assert!(supported);
        assert_eq!(
            receiver
                .list_message_favorites("", 0, 100)
                .unwrap()
                .items
                .len(),
            17
        );
        let mut unchanged = receiver
            .privacy_dial(peer.addr, &source.identity.public())
            .await
            .unwrap();
        super::send_frame(
            &mut unchanged,
            super::AccountSyncVersion::V2,
            &super::AccountHistoryFrame::FavoritesRequest {
                version: 1,
                account_id: source.account_id(),
                after: None,
                known_digest: digest,
                attachments: false,
            },
        )
        .await
        .unwrap();
        let bytes = unchanged.recv().await.unwrap();
        assert!(
            matches!(super::AccountHistoryFrame::decode_versioned(&bytes),Some((super::AccountSyncVersion::V2,super::AccountHistoryFrame::FavoritesComplete {next:None,digest:actual})) if actual==digest)
        );
        server.abort();
    }
    #[tokio::test(start_paused = true)]
    async fn favorites_whole_exchange_deadline_does_not_reset_for_continuous_progress() {
        let ticks = std::sync::atomic::AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let completed =
            super::bounded_favorite_exchange(std::time::Duration::from_secs(20), async {
                loop {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                    ticks.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                }
            })
            .await;
        assert!(!completed);
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(20));
        assert!(ticks.load(std::sync::atomic::Ordering::Relaxed) > 1);
        assert_eq!(
            super::attachment_deadline(64 * 1024 * 1024),
            std::time::Duration::from_secs(30 + 4096)
        );
    }
}
