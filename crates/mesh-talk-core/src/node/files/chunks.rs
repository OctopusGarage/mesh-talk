//! Selected chunk access. File operations hold a fixed metadata page and at
//! most one admitted signed body; the durable event owner fences continuation.
use super::{ConversationId, EventId, EventKind, LogError, Node, NodeError};
use crate::eventlog::{
    Author, Event, PersistentEventLog,
    repository::{
        EventHeader,
        pages::{PageBudget, PageCursor},
    },
};
use sha2::{Digest, Sha256};
use std::io::{Cursor, Read};

const HEADER_ROWS: usize = 32;
const EVENT_BYTES: usize = 4 * 1024 * 1024;

/// Exact signed linear chunk ancestry, accumulated without retaining bodies.
#[derive(Default)]
pub(in crate::node) struct ChunkChain {
    pub(in crate::node) source: Option<Author>,
    pub(in crate::node) last: Option<EventId>,
    pub(in crate::node) count: u64,
}

impl ChunkChain {
    pub(in crate::node) fn accept(&mut self, event: &Event) -> bool {
        if event.kind != EventKind::Message
            || self.source.is_some_and(|source| event.author != source)
            || event.seq != self.count.saturating_add(1)
            || match self.last {
                None => !event.parents.is_empty(),
                Some(previous) => event.parents != [previous],
            }
            || !event.verify_integrity()
            || !event.verify_signature()
        {
            return false;
        }
        self.source = Some(event.author);
        self.last = Some(event.id);
        self.count += 1;
        true
    }
}

pub(in crate::node) struct FileHeaders<'a> {
    node: &'a Node,
    conversation: ConversationId,
    pending: std::vec::IntoIter<EventHeader>,
    cursor: Option<PageCursor>,
}

impl<'a> FileHeaders<'a> {
    fn budget() -> PageBudget {
        PageBudget {
            rows: HEADER_ROWS,
            bytes: HEADER_ROWS * std::mem::size_of::<EventHeader>(),
        }
    }

    fn new(
        node: &'a Node,
        conversation: ConversationId,
        log: &PersistentEventLog,
    ) -> Result<Self, LogError> {
        let page = log.headers_page(&conversation, None, Self::budget())?;
        Ok(Self {
            node,
            conversation,
            pending: page.items.into_iter(),
            cursor: page.next,
        })
    }
}

impl Iterator for FileHeaders<'_> {
    type Item = Result<EventHeader, LogError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            if let Some(header) = self.pending.next() {
                return Some(Ok(header));
            }
            let cursor = self.cursor.take()?;
            let page = self
                .node
                .log
                .lock()
                .expect("log lock not poisoned")
                .headers_page(&self.conversation, Some(&cursor), Self::budget());
            match page {
                Ok(page) => {
                    self.pending = page.items.into_iter();
                    self.cursor = page.next;
                }
                Err(error) => return Some(Err(error)),
            }
        }
    }
}

pub(super) struct ChunkIds<'a> {
    pub(super) total: u64,
    headers: FileHeaders<'a>,
}

pub(super) struct PlaintextChunks<'a> {
    node: &'a Node,
    manifest: &'a crate::file::AnyManifest,
    ids: ChunkIds<'a>,
    pending: Cursor<Vec<u8>>,
    index: u64,
    size: u64,
    checksum: Sha256,
    finished: bool,
}

impl<'a> PlaintextChunks<'a> {
    pub(super) fn new(
        node: &'a Node,
        manifest: &'a crate::file::AnyManifest,
    ) -> Result<Self, NodeError> {
        let ids = node.chunk_ids(manifest.file_conv())?;
        if ids.total != u64::from(manifest.chunk_count()) {
            return Err(NodeError::File("file incomplete".into()));
        }
        Ok(Self {
            node,
            manifest,
            ids,
            pending: Cursor::new(Vec::new()),
            index: 0,
            size: 0,
            checksum: Sha256::new(),
            finished: false,
        })
    }

    /// Encryption consumes the declared length. Verify EOF/checksum before its
    /// prepared ciphertext can be published, including the empty-file chunk.
    pub(super) fn finish(&mut self) -> std::io::Result<()> {
        if self.read(&mut [0])? != 0 {
            return Err(std::io::Error::other("file size mismatch"));
        }
        Ok(())
    }
}

impl Read for PlaintextChunks<'_> {
    fn read(&mut self, output: &mut [u8]) -> std::io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        loop {
            let copied = self.pending.read(output)?;
            if copied != 0 {
                return Ok(copied);
            }
            if self.finished {
                return Ok(0);
            }
            let Some(id) = self.ids.next() else {
                let actual: [u8; 32] = self.checksum.clone().finalize().into();
                if self.size != self.manifest.size() || actual != self.manifest.checksum() {
                    return Err(std::io::Error::other("file size or checksum mismatch"));
                }
                self.finished = true;
                return Ok(0);
            };
            let ciphertext = self
                .node
                .chunk_ciphertext(id.map_err(std::io::Error::other)?)
                .map_err(std::io::Error::other)?;
            let index = u32::try_from(self.index).map_err(std::io::Error::other)?;
            let plaintext = crate::file::open_chunk_for(self.manifest, index, &ciphertext)
                .map_err(std::io::Error::other)?;
            self.index += 1;
            self.size = self
                .size
                .checked_add(plaintext.len() as u64)
                .ok_or_else(|| std::io::Error::other("file size overflow"))?;
            if self.size > self.manifest.size() {
                return Err(std::io::Error::other("file size mismatch"));
            }
            self.checksum.update(&plaintext);
            self.pending = Cursor::new(plaintext);
        }
    }
}

impl Iterator for ChunkIds<'_> {
    type Item = Result<EventId, NodeError>;

    fn next(&mut self) -> Option<Self::Item> {
        loop {
            match self.headers.next()? {
                Ok(header) if header.kind == EventKind::Message => return Some(Ok(header.id)),
                Ok(_) => {}
                Err(error) => return Some(Err(error.into())),
            }
        }
    }
}

impl Node {
    pub(in crate::node) fn file_headers(
        &self,
        conversation: ConversationId,
    ) -> Result<FileHeaders<'_>, LogError> {
        let log = self.log.lock().expect("log lock not poisoned");
        FileHeaders::new(self, conversation, &log)
    }

    pub(super) fn chunk_ids(
        &self,
        conversation: ConversationId,
    ) -> Result<ChunkIds<'_>, NodeError> {
        let log = self.log.lock().expect("log lock not poisoned");
        let total = log.count_kind(&conversation, EventKind::Message)?;
        let headers = FileHeaders::new(self, conversation, &log)?;
        Ok(ChunkIds { total, headers })
    }

    pub(in crate::node) fn file_event(&self, id: EventId) -> Result<Event, LogError> {
        self.log
            .lock()
            .expect("log lock not poisoned")
            .get_bounded(&id, EVENT_BYTES)?
            .ok_or_else(|| {
                LogError::CorruptFile("selected file event is no longer available".into())
            })
    }

    pub(super) fn chunk_ciphertext(&self, id: EventId) -> Result<Vec<u8>, NodeError> {
        let event = self.file_event(id)?;
        if event.kind != EventKind::Message {
            return Err(NodeError::File("file chunk is no longer available".into()));
        }
        Ok(event.ciphertext)
    }
}
