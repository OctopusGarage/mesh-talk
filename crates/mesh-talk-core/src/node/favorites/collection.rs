//! Canonical snapshot capture and account-local favorites queries.
use super::*;

impl Node {
    /// Favorite snapshots have no reveal policy. Reject a retained canonical
    /// protected source rather than turning that snapshot into another reveal path.
    pub(in crate::node) fn favorite_has_protected_content(
        &self,
        favorite: &MessageFavorite,
    ) -> bool {
        if favorite
            .messages
            .iter()
            .filter_map(|m| m.file.as_ref())
            .any(|file| {
                conversation(&file.file_conv).is_some_and(|conv| {
                    self.files
                        .lock()
                        .expect("files mutex")
                        .manifest(&conv)
                        .is_some_and(|manifest| manifest.protection().is_some())
                })
            })
        {
            return true;
        }
        let source = &favorite.source;
        let selected: Vec<_> = source
            .message_ids
            .iter()
            .filter_map(|id| {
                conversation(id).map(|conv| crate::eventlog::EventId::new(*conv.as_bytes()))
            })
            .collect();
        let (conv, account) = if source.kind == "account" {
            (
                crate::node::conversation::account_conversation_id(
                    &self.account_id(),
                    &source.target,
                ),
                Some(source.target.as_str()),
            )
        } else {
            let Some(conv) = conversation(&source.target) else {
                return true;
            };
            (conv, None)
        };
        self.project_history(
            conv,
            account,
            crate::node::queries::HistorySelection::Selected(&selected),
        )
        .is_some_and(|context| {
            context
                .messages
                .iter()
                .any(|entry| entry.protection.is_some())
        })
    }

    pub fn list_message_favorites(
        &self,
        query: &str,
        offset: usize,
        limit: usize,
    ) -> Result<MessageFavoriteList, NodeError> {
        if query.chars().count() > 200 || limit == 0 || limit > 100 || offset > MAX_FAVORITES {
            return Err(invalid("invalid favorites page or query"));
        }
        let query = query.trim().to_lowercase();
        let store = self.favorites.lock().expect("favorites mutex");
        let now = self.expiry_now();
        let mut items: Vec<_> = store
            .current
            .values()
            .filter(|r| !r.removed)
            .map(|r| &r.favorite)
            .filter(|favorite| !self.favorite_has_protected_content(favorite))
            .filter(|f| {
                query.is_empty()
                    || [&f.metadata.title, &f.metadata.description, &f.source_label]
                        .into_iter()
                        .chain(f.metadata.notes.values())
                        .any(|s| s.to_lowercase().contains(&query))
                    || f.messages.iter().any(|m| {
                        m.sender_name.to_lowercase().contains(&query)
                            || (m.expires_at_ms.is_none_or(|end| end > now)
                                && (m.text.to_lowercase().contains(&query)
                                    || m.file.as_ref().is_some_and(|file| {
                                        if crate::file::sticker::valid_manifest(
                                            &file.name, &file.mime, file.size, file.media,
                                        ) && let Some(metadata) =
                                            crate::file::sticker::from_name(&file.name)
                                        {
                                            metadata.fallback.to_lowercase().contains(&query)
                                                || metadata.label.is_some_and(|label| {
                                                    label.to_lowercase().contains(&query)
                                                })
                                        } else {
                                            file.name.to_lowercase().contains(&query)
                                        }
                                    })))
                    })
            })
            .collect();
        items.sort_by(|a, b| (b.saved_at_ms, &b.id).cmp(&(a.saved_at_ms, &a.id)));
        let mut page = Vec::new();
        let mut used = 0;
        let remaining = items.len().saturating_sub(offset);
        for item in items.into_iter().skip(offset).take(limit) {
            let size =
                bincode::serialized_size(&item).unwrap_or(MAX_FAVORITE_BYTES as u64) as usize;
            if used + size > 1024 * 1024 {
                break;
            }
            used += size;
            page.push(project(item.clone(), now));
        }
        Ok(MessageFavoriteList {
            has_more: remaining > page.len(),
            items: page,
        })
    }
    pub fn inspect_message_favorites(
        &self,
        source: &FavoriteSource,
    ) -> Result<FavoriteInspection, NodeError> {
        if !((source.kind == "account" && hex_valid(&source.target, 32))
            || (source.kind == "channel" && hex_valid(&source.target, 64)))
            || source.message_ids.len() > 500
            || source
                .message_ids
                .iter()
                .any(|id| !hex_valid(id, 64) || id == &"0".repeat(64))
        {
            return Err(invalid("invalid favorite inspection source"));
        }
        let requested: BTreeSet<_> = source.message_ids.iter().cloned().collect();
        let mut saved = BTreeSet::new();
        let mut existing = None;
        let now = self.expiry_now();
        let store = self.favorites.lock().expect("favorites mutex");
        for record in store.current.values().filter(|r| {
            !r.removed
                && r.favorite.source.kind == source.kind
                && r.favorite.source.target == source.target
                && !self.favorite_has_protected_content(&r.favorite)
        }) {
            if record
                .favorite
                .source
                .message_ids
                .iter()
                .cloned()
                .collect::<BTreeSet<_>>()
                == requested
            {
                existing = Some(project(record.favorite.clone(), now));
            }
            for message in &record.favorite.messages {
                if requested.contains(&message.id)
                    && message.expires_at_ms.is_none_or(|end| end > now)
                {
                    saved.insert(message.id.clone());
                }
            }
        }
        Ok(FavoriteInspection {
            existing,
            saved_message_ids: saved.into_iter().collect(),
        })
    }
    fn favorite_name(&self, who: &str) -> String {
        let live_name = {
            let roster = self.roster.lock().expect("roster mutex");
            roster
                .peers()
                .into_iter()
                .find(|p| p.public.user_id() == who || p.account_id.as_deref() == Some(who))
                .map(|p| p.name)
        };
        live_name
            .or_else(|| {
                self.historical_dm_peers()
                    .into_iter()
                    .find(|p| p.user_id == who || p.account_id().as_deref() == Some(who))
                    .map(|p| p.name)
            })
            .unwrap_or_else(|| who.into())
    }

    pub(in crate::node) fn capture_favorite(
        &self,
        mut source: FavoriteSource,
        metadata: FavoriteMetadata,
    ) -> Result<MessageFavorite, NodeError> {
        if !valid_source(&source) || !valid_metadata(&metadata, &source) {
            return Err(invalid("invalid favorite source or metadata"));
        }
        let selected_ids: Vec<_> = source
            .message_ids
            .iter()
            .map(|id| {
                conversation(id)
                    .map(|c| crate::eventlog::event::EventId::new(*c.as_bytes()))
                    .ok_or_else(|| invalid("invalid message"))
            })
            .collect::<Result<_, _>>()?;
        let (conv, account) = if source.kind == "account" {
            (
                crate::node::conversation::account_conversation_id(
                    &self.account_id(),
                    &source.target,
                ),
                Some(source.target.as_str()),
            )
        } else {
            let conv = conversation(&source.target).ok_or_else(|| invalid("invalid channel"))?;
            if !self.list_channels().iter().any(|c| c.id == conv) {
                return Err(invalid("unknown channel"));
            }
            (conv, None)
        };
        let history: Vec<HistoryEntry> = self
            .project_history(
                conv,
                account,
                crate::node::queries::HistorySelection::Selected(&selected_ids),
            )
            .map(|c| c.messages)
            .unwrap_or_default();
        let selected: BTreeSet<_> = source.message_ids.iter().cloned().collect();
        let mut messages = Vec::new();
        for e in history {
            let id = e.id.to_hex();
            if !selected.contains(&id) {
                continue;
            }
            if e.protection.is_some() {
                return Err(invalid("Protected messages cannot be saved to Favorites"));
            }
            if e.recalled || e.expires_at_ms.is_some_and(|end| end <= self.expiry_now()) {
                let position = source
                    .message_ids
                    .iter()
                    .position(|selected| selected == &id)
                    .unwrap_or(0)
                    + 1;
                return Err(invalid(&format!(
                    "Selected message {position} is no longer available"
                )));
            }
            let file = e.file.map(|f| FavoriteFile {
                file_conv: hex::encode(f.file_conv.as_bytes()),
                name: f.name,
                size: f.size,
                mime: f.mime,
                media: f.media,
            });
            messages.push(FavoriteMessage {
                id,
                sender_name: self.favorite_name(&e.who),
                who: e.who,
                text: String::from_utf8_lossy(&e.text).into_owned(),
                wall_clock: e.wall_clock,
                expires_at_ms: e.expires_at_ms,
                sticker: e.sticker,
                file,
            });
        }
        if messages.len() != selected.len() {
            let position = source
                .message_ids
                .iter()
                .position(|id| !messages.iter().any(|message| &message.id == id))
                .unwrap_or(0)
                + 1;
            return Err(invalid(&format!(
                "Selected message {position} is no longer available"
            )));
        }
        source.message_ids = messages.iter().map(|m| m.id.clone()).collect();
        let source_label = if source.kind == "channel" {
            self.list_channels()
                .into_iter()
                .find(|c| hex::encode(c.id.as_bytes()) == source.target)
                .map(|c| c.name)
                .unwrap_or_else(|| source.target.clone())
        } else {
            self.favorite_name(&source.target)
        };
        Ok(MessageFavorite {
            id: super::recovery::canonical_id(&self.account_id(), &source)?,
            source,
            source_label,
            metadata,
            messages,
            saved_at_ms: now_millis(),
            revision: 1,
            device_id: self.user_id(),
        })
    }
    /// Authority must call the supplied synchronous commit once and propagate its result.
    pub fn save_message_favorite_guarded(
        &self,
        source: FavoriteSource,
        metadata: FavoriteMetadata,
        guard: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<MessageFavorite, NodeError> {
        let mut result = None;
        guard(&mut || {
            if result.is_some() {
                return Err(invalid("favorite commit already executed"));
            }
            if !valid_source(&source) || !valid_metadata(&metadata, &source) {
                return Err(invalid("invalid favorite source or metadata"));
            }
            if let Some(existing) = self.inspect_message_favorites(&source)?.existing {
                result = Some(existing);
                return Ok(());
            }
            let mut favorite = self.capture_favorite(source.clone(), metadata.clone())?;
            let mut store = self.favorites.lock().expect("favorites mutex");
            if let Some(existing) = store.current.get(&favorite.id).filter(|r| !r.removed) {
                result = Some(project(existing.favorite.clone(), self.expiry_now()));
                return Ok(());
            }
            favorite.revision = store.next_revision()?;
            let generation = store.current.get(&favorite.id).map_or(Ok(1), |old| {
                old.generation
                    .checked_add(1)
                    .filter(|next| *next <= MAX_SAFE_REVISION)
                    .ok_or_else(|| invalid("favorite generation exhausted"))
            })?;
            let record = FavoriteRecord {
                generation,
                favorite: favorite.clone(),
                removed: false,
            };
            store.validate(&record).map_err(NodeError::Log)?;
            // Preserve verified bytes separately so clearing history cannot prune them.
            for message in &favorite.messages {
                if let Some(file) = &message.file {
                    if file.size > MAX_FAVORITE_FILE_BYTES {
                        continue;
                    }
                    let conv = conversation(&file.file_conv)
                        .ok_or_else(|| invalid("invalid attachment"))?;
                    let Some(bytes) = self.read_media(conv).or_else(|| self.read_file(conv).ok())
                    else {
                        continue;
                    };
                    self.favorite_media
                        .store_bytes(conv, &file.name, &bytes)
                        .map_err(|e| NodeError::File(e.to_string()))?;
                }
            }
            // Re-resolve current visibility after potentially slow attachment copies.
            // The authority callback still encloses this check and the journal commit.
            let current = self.capture_favorite(source.clone(), metadata.clone())?;
            if current.source != favorite.source || current.messages != favorite.messages {
                return Err(invalid("selected messages changed while saving"));
            }
            if favorite
                .messages
                .iter()
                .any(|m| m.expires_at_ms.is_some_and(|end| end <= self.expiry_now()))
            {
                return Err(invalid("selected message expired"));
            }
            store.merge(record).map_err(NodeError::Log)?;
            result = Some(favorite);
            drop(store);
            self.note_account_local_change();
            Ok(())
        })?;
        result.ok_or_else(|| NodeError::Authorization("favorite commit not authorized".into()))
    }
    pub fn edit_message_favorite_guarded(
        &self,
        id: &str,
        expected_revision: u64,
        expected_device_id: &str,
        metadata: FavoriteMetadata,
        guard: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<MessageFavorite, NodeError> {
        let mut result = None;
        guard(&mut || {
            if result.is_some() {
                return Err(invalid("favorite commit already executed"));
            }
            let mut store = self.favorites.lock().expect("favorites mutex");
            let mut record = store
                .current
                .get(id)
                .filter(|r| !r.removed)
                .cloned()
                .ok_or_else(|| invalid("favorite unavailable"))?;
            if self.favorite_has_protected_content(&record.favorite) {
                return Err(invalid("Protected messages cannot be saved to Favorites"));
            }
            if record.favorite.revision != expected_revision
                || record.favorite.device_id != expected_device_id
            {
                return Err(invalid("favorite was modified; refresh and retry"));
            }
            if !valid_metadata(&metadata, &record.favorite.source) {
                return Err(invalid("invalid favorite metadata"));
            }
            record.favorite.metadata = metadata.clone();
            record.favorite.revision = store.next_revision()?;
            record.favorite.device_id = self.user_id();
            store.merge(record.clone()).map_err(NodeError::Log)?;
            result = Some(project(record.favorite, self.expiry_now()));
            drop(store);
            self.note_account_local_change();
            Ok(())
        })?;
        result.ok_or_else(|| NodeError::Authorization("favorite commit not authorized".into()))
    }
    pub fn remove_message_favorite_guarded(
        &self,
        id: &str,
        expected_revision: u64,
        expected_device_id: &str,
        guard: impl FnOnce(&mut dyn FnMut() -> Result<(), NodeError>) -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let mut executed = false;
        guard(&mut || {
            if executed {
                return Err(invalid("favorite commit already executed"));
            }
            let mut store = self.favorites.lock().expect("favorites mutex");
            let mut record = store
                .current
                .get(id)
                .filter(|r| !r.removed)
                .cloned()
                .ok_or_else(|| invalid("favorite unavailable"))?;
            if record.favorite.revision != expected_revision
                || record.favorite.device_id != expected_device_id
            {
                return Err(invalid("favorite was modified; refresh and retry"));
            }
            record.removed = true;
            record.favorite.revision = store.next_revision()?;
            record.favorite.device_id = self.user_id();
            store.merge(record).map_err(NodeError::Log)?;
            self.favorite_media
                .prune_unreferenced(&store.live_files(self.expiry_now()))
                .map_err(|e| NodeError::File(e.to_string()))?;
            executed = true;
            drop(store);
            self.note_account_local_change();
            Ok(())
        })?;
        if !executed {
            return Err(NodeError::Authorization(
                "favorite commit not authorized".into(),
            ));
        }
        Ok(())
    }
    pub(in crate::node) fn prune_favorite_files(&self) -> Result<(), NodeError> {
        let store = self.favorites.lock().expect("favorites mutex");
        self.favorite_media
            .prune_unreferenced(&store.live_files(self.expiry_now()))
            .map_err(|e| NodeError::File(e.to_string()))
    }
    pub(in crate::node) fn missing_favorite_files(&self) -> Vec<ConversationId> {
        let store = self.favorites.lock().expect("favorites mutex");
        let mut files: Vec<_> = store
            .current
            .values()
            .filter(|r| !r.removed)
            .filter(|r| !self.favorite_has_protected_content(&r.favorite))
            .flat_map(|r| &r.favorite.messages)
            .filter(|m| m.expires_at_ms.is_none_or(|end| end > self.expiry_now()))
            .filter_map(|m| m.file.as_ref())
            .filter(|f| f.size <= MAX_FAVORITE_FILE_BYTES)
            .filter_map(|f| conversation(&f.file_conv))
            .filter(|file| !self.favorite_media.contains(*file))
            .collect();
        files.sort_by_key(|file| *file.as_bytes());
        files.dedup();
        files
    }
    pub(in crate::node) fn preserve_favorite_file(
        &self,
        file: ConversationId,
        revision: u64,
    ) -> Result<bool, NodeError> {
        let _commit = self.account_sync_commit.lock().expect("sync commit mutex");
        let (policy, current) = self.device_sync_policy_snapshot();
        if current != revision || !policy.history || !policy.attachments {
            return Err(NodeError::Authorization(
                "favorite attachment policy changed".into(),
            ));
        }
        let Some(metadata) = self.favorite_file_metadata(file) else {
            return Ok(false);
        };
        if metadata.size > MAX_FAVORITE_FILE_BYTES {
            return Ok(false);
        }
        let Some(bytes) = self.read_media(file).or_else(|| self.read_file(file).ok()) else {
            return Ok(false);
        };
        let store = self.favorites.lock().expect("favorites mutex");
        if !store.live_files(self.expiry_now()).contains(&file) {
            return Ok(false);
        }
        self.favorite_media
            .store_bytes(file, &metadata.name, &bytes)
            .map_err(|e| NodeError::File(e.to_string()))?;
        Ok(true)
    }
    fn favorite_file_metadata(&self, file_conv: ConversationId) -> Option<FavoriteFile> {
        self.favorites
            .lock()
            .expect("favorites mutex")
            .current
            .values()
            .filter(|r| !r.removed)
            .filter(|r| !self.favorite_has_protected_content(&r.favorite))
            .flat_map(|r| &r.favorite.messages)
            .filter(|m| m.expires_at_ms.is_none_or(|end| end > self.expiry_now()))
            .filter_map(|m| m.file.as_ref())
            .find(|f| f.file_conv == hex::encode(file_conv.as_bytes()))
            .cloned()
    }
    pub(in crate::node) fn favorite_file_size(&self, file_conv: ConversationId) -> Option<u64> {
        self.favorite_file_metadata(file_conv).map(|file| file.size)
    }
    pub fn favorite_file_available(&self, file_conv: ConversationId) -> bool {
        self.favorite_file_metadata(file_conv).is_some() && self.favorite_media.contains(file_conv)
    }
    pub fn save_favorite_file(
        &self,
        file_conv: ConversationId,
        dest: &Path,
    ) -> Result<(), NodeError> {
        use std::io::Write;
        let bytes = self
            .read_favorite_file(file_conv)
            .ok_or_else(|| NodeError::File("favorite attachment unavailable".into()))?;
        let parent = dest
            .parent()
            .ok_or_else(|| invalid("invalid favorite destination"))?;
        let mut temp =
            tempfile::NamedTempFile::new_in(parent).map_err(|e| NodeError::File(e.to_string()))?;
        temp.write_all(&bytes)
            .and_then(|_| temp.as_file().sync_all())
            .map_err(|e| NodeError::File(e.to_string()))?;
        if !self.favorite_file_available(file_conv) {
            return Err(NodeError::File("favorite attachment unavailable".into()));
        }
        temp.persist(dest)
            .map_err(|e| NodeError::File(e.to_string()))?;
        crate::node::media_store::sync_parent(dest).map_err(|e| NodeError::File(e.to_string()))?;
        Ok(())
    }
    pub fn save_favorite_file_into_dir(
        &self,
        file_conv: ConversationId,
        dir: &Path,
    ) -> Result<std::path::PathBuf, NodeError> {
        let file = self
            .favorite_file_metadata(file_conv)
            .ok_or_else(|| NodeError::File("favorite attachment unavailable".into()))?;
        if file.mime == crate::file::directory::MIME {
            return crate::file::directory::save_retained(dir, &file.name, file.size, |dest| {
                self.save_favorite_file(file_conv, dest)
            });
        }
        let dest = crate::util::savename::safe_save_path(dir, &file.name)
            .ok_or_else(|| invalid("could not place favorite attachment safely"))?;
        self.save_favorite_file(file_conv, &dest)?;
        Ok(dest)
    }
    pub fn read_favorite_file(&self, file_conv: ConversationId) -> Option<Vec<u8>> {
        self.favorite_file_metadata(file_conv)?;
        self.favorite_media.read(file_conv)
    }
}
