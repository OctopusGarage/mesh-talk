use super::*;

/// Explicit caller budgets are never clamped. Defaults bound an ordinary active
/// page to 512 rows / 4 MiB. Zero or unrepresentable row limits are errors.
#[derive(Clone, Copy, Debug)]
pub(crate) struct PageBudget {
    pub rows: usize,
    pub bytes: usize,
}
impl Default for PageBudget {
    fn default() -> Self {
        Self {
            rows: 512,
            bytes: 4 * 1024 * 1024,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageOrder {
    Headers,
    Bodies,
    Ids,
    Parents(EventId),
    Heads,
    Conversations,
    Authors,
}
#[derive(Clone, Debug)]
pub(crate) struct PageCursor {
    store_uuid: [u8; 16],
    domain: EventDomainId,
    conversation: Option<ConversationId>,
    order: PageOrder,
    revision: i64,
    last_clock: u64,
    last_key: [u8; 32],
}
#[derive(Debug)]
pub(crate) struct Page<T> {
    pub items: Vec<T>,
    pub next: Option<PageCursor>,
    pub charged_bytes: usize,
}
struct Builder<T> {
    page: Page<T>,
    budget: PageBudget,
    binding: PageCursor,
}
impl<T> Builder<T> {
    fn accepts(&self, cost: usize) -> Result<bool, LogError> {
        if self.page.items.len() == self.budget.rows {
            return Ok(false);
        }
        let total = self
            .page
            .charged_bytes
            .checked_add(cost)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        if total > self.budget.bytes {
            if self.page.items.is_empty() {
                return Err(LogError::PageBudgetTooSmall { required: cost });
            }
            return Ok(false);
        }
        Ok(true)
    }
    fn push(&mut self, item: T, clock: u64, key: [u8; 32], cost: usize) {
        // Vec's geometric spare capacity is otherwise invisible to byte
        // accounting (e.g. seven charged headers in an eight-header buffer).
        self.page.items.reserve_exact(1);
        self.page.items.push(item);
        self.page.charged_bytes += cost;
        self.binding.last_clock = clock;
        self.binding.last_key = key;
    }
    fn finish(mut self, more: bool) -> Page<T> {
        if more {
            self.page.next = Some(self.binding);
        }
        self.page
    }
}
fn sql_limit(budget: PageBudget) -> Result<i64, LogError> {
    if budget.rows == 0 || budget.bytes == 0 {
        return Err(LogError::InvalidPageBudget);
    }
    i64::try_from(budget.rows)
        .ok()
        .and_then(|n| n.checked_add(1))
        .ok_or(LogError::InvalidPageBudget)
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct AuthorMaximum {
    pub author: Author,
    pub max_seq: u64,
}
impl EventRepository {
    /// Discover the retained per-author maxima (availability hints, not dense
    /// high-water marks) without loading events or allocating a complete VV.
    pub(crate) fn author_maxima_page(
        &self,
        conv: &ConversationId,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<Page<AuthorMaximum>, LogError> {
        self.database.read(|c| {
            let mut builder=self.builder(c,Some(*conv),PageOrder::Authors,cursor,budget)?;
            let after=if cursor.is_some(){"AND author>?3"}else{""};
            let mut stmt=c.prepare(&format!("SELECT author,max_seq FROM event_author_max WHERE domain=?1 AND conversation=?2 {after} ORDER BY author LIMIT ?4"))?;
            let mut rows=stmt.query(params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),cursor.map_or([0;32],|c|c.last_key).as_slice(),sql_limit(budget)?])?;
            while let Some(row)=rows.next()? {
                let key=blob(row,0)?;
                let maximum=AuthorMaximum{author:Author::from_ed25519(key),max_seq:u64::from_be_bytes(blob(row,1)?)};
                let cost=std::mem::size_of::<AuthorMaximum>();
                if !builder.accepts(cost)? {return Ok(builder.finish(true));}
                builder.push(maximum,0,key,cost);
            }
            Ok(builder.finish(false))
        })
    }
    fn builder<T>(
        &self,
        c: &Connection,
        conv: Option<ConversationId>,
        order: PageOrder,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<Builder<T>, LogError> {
        sql_limit(budget)?;
        let revision = if let Some(conv) = conv {
            self.scope_revision_on(c, &conv)?
        } else {
            nonnegative(c.query_row(
                "SELECT source_revision FROM event_domains WHERE domain=?1",
                [self.domain.0.as_slice()],
                |r| r.get(0),
            )?)?
        };
        if let Some(cursor) = cursor
            && (cursor.store_uuid != self.store_uuid
                || cursor.domain != self.domain
                || cursor.conversation != conv
                || cursor.order != order
                || cursor.revision != revision)
        {
            return Err(LogError::StaleCursor);
        }
        if let Some(conv) = conv {
            self.require_active_on(c, &conv)?;
        }
        Ok(Builder {
            page: Page {
                items: Vec::new(),
                next: None,
                charged_bytes: 0,
            },
            budget,
            binding: PageCursor {
                store_uuid: self.store_uuid,
                domain: self.domain,
                conversation: conv,
                order,
                revision,
                last_clock: 0,
                last_key: [0; 32],
            },
        })
    }
    pub(crate) fn headers_page(
        &self,
        conv: &ConversationId,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<Page<EventHeader>, LogError> {
        self.database.read(|c| {
            self.clock_page(c, conv, cursor, budget, PageOrder::Headers, |_, header| {
                Ok(header)
            })
        })
    }
    /// The charge is stored bytes three times (encoded input plus decoded variable
    /// fields), plus Event's fixed allocation. This conservative allocation
    /// estimate differs from encoded_bytes, which remains the wire budget.
    /// Page/cursor containers and SQLite's fixed connection cache are separate.
    /// An oversized first row returns a typed required-byte count before body
    /// fetch. A sync progress caller can explicitly retry with that count.
    pub(crate) fn events_page(
        &self,
        conv: &ConversationId,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<Page<Event>, LogError> {
        self.database.read(|c| {
            self.clock_page(c, conv, cursor, budget, PageOrder::Bodies, |c, header| {
                self.get_on(c, &header.id)?
                    .ok_or_else(|| corrupt("selected event disappeared"))
            })
        })
    }
    fn clock_page<T>(
        &self,
        c: &Connection,
        conv: &ConversationId,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
        order: PageOrder,
        mut project: impl FnMut(&Connection, EventHeader) -> Result<T, LogError>,
    ) -> Result<Page<T>, LogError> {
        let mut builder = self.builder(c, Some(*conv), order, cursor, budget)?;
        let after = if cursor.is_some() {
            "AND (lamport,id)>(?3,?4)"
        } else {
            ""
        };
        let mut stmt=c.prepare(&format!("SELECT {HEADER_COLUMNS},length(body) FROM retained_events WHERE domain=?1 AND conversation=?2 {after} ORDER BY lamport,id LIMIT ?5"))?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            conv.as_bytes().as_slice(),
            cursor.map_or(0, |c| c.last_clock).to_be_bytes().as_slice(),
            cursor.map_or([0; 32], |c| c.last_key).as_slice(),
            sql_limit(budget)?
        ])?;
        while let Some(row) = rows.next()? {
            let header = read_header(row)?;
            let cost = match order {
                PageOrder::Headers => std::mem::size_of::<EventHeader>(),
                PageOrder::Bodies => header
                    .stored_bytes
                    .checked_mul(3)
                    .and_then(|n| n.checked_add(std::mem::size_of::<Event>()))
                    .ok_or(LogError::RepositoryCounterExhausted)?,
                _ => unreachable!("clock page only accepts header/body orders"),
            };
            // SQLite length(BLOB) reads the stored length, not the content.
            // A malformed small indexed length must not bypass the byte check
            // and cause a much larger selected BLOB to be materialized.
            if size(row, 12)? != header.stored_bytes {
                return Err(corrupt("stored event length disagrees with body"));
            }
            if !builder.accepts(cost)? {
                return Ok(builder.finish(true));
            }
            let item = project(c, header)?;
            builder.push(item, header.lamport, *header.id.as_bytes(), cost);
        }
        Ok(builder.finish(false))
    }
    pub(crate) fn ids_page(
        &self,
        conv: &ConversationId,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<Page<EventId>, LogError> {
        self.database
            .read(|c| self.ids_on(c, *conv, PageOrder::Ids, cursor, budget))
    }
    pub(crate) fn heads_page(
        &self,
        conv: &ConversationId,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<Page<EventId>, LogError> {
        self.database
            .read(|c| self.ids_on(c, *conv, PageOrder::Heads, cursor, budget))
    }
    pub(crate) fn parent_ids_page(
        &self,
        event: &EventId,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<Option<Page<EventId>>, LogError> {
        sql_limit(budget)?;
        self.database.read(|c| {
            let mut stmt =
                c.prepare("SELECT conversation FROM retained_events WHERE domain=?1 AND id=?2")?;
            let mut rows = stmt.query(params![
                self.domain.0.as_slice(),
                event.as_bytes().as_slice()
            ])?;
            let Some(row) = rows.next()? else {
                if cursor.is_some() {
                    return Err(LogError::StaleCursor);
                }
                return Ok(None);
            };
            let conv = ConversationId::new(blob(row, 0)?);
            self.ids_on(c, conv, PageOrder::Parents(*event), cursor, budget)
                .map(Some)
        })
    }
    fn ids_on(
        &self,
        c: &Connection,
        conv: ConversationId,
        order: PageOrder,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<Page<EventId>, LogError> {
        let mut builder = self.builder(c, Some(conv), order, cursor, budget)?;
        let (table, column, condition, scope) = match order {
            PageOrder::Ids => (
                "retained_events",
                "id",
                "conversation",
                conv.as_bytes().as_slice(),
            ),
            PageOrder::Heads => (
                "event_heads",
                "id",
                "conversation",
                conv.as_bytes().as_slice(),
            ),
            PageOrder::Parents(ref event) => (
                "retained_event_parents",
                "parent",
                "child",
                event.as_bytes().as_slice(),
            ),
            _ => unreachable!("ID page requires ID order"),
        };
        let after = if cursor.is_some() {
            format!("AND {column}>?3")
        } else {
            String::new()
        };
        let mut stmt=c.prepare(&format!("SELECT {column} FROM {table} WHERE domain=?1 AND {condition}=?2 {after} ORDER BY {column} LIMIT ?4"))?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            scope,
            cursor.map_or([0; 32], |c| c.last_key).as_slice(),
            sql_limit(budget)?
        ])?;
        while let Some(row) = rows.next()? {
            let id = EventId::new(blob(row, 0)?);
            if !builder.accepts(std::mem::size_of::<EventId>())? {
                return Ok(builder.finish(true));
            }
            builder.push(id, 0, *id.as_bytes(), std::mem::size_of::<EventId>());
        }
        Ok(builder.finish(false))
    }
    pub(crate) fn conversations_page(
        &self,
        cursor: Option<&PageCursor>,
        budget: PageBudget,
    ) -> Result<Page<ConversationMetadata>, LogError> {
        self.database.read(|c| {
            let mut builder=self.builder(c,None,PageOrder::Conversations,cursor,budget)?;
            let after=if cursor.is_some(){"AND conversation>?2"}else{""};
            let mut stmt=c.prepare(&format!("SELECT {} FROM event_conversations INDEXED BY event_conversations_active WHERE domain=?1 AND lifecycle=0 AND event_count>0 {after} ORDER BY conversation LIMIT ?3",metadata::METADATA_COLUMNS))?;
            let mut rows=stmt.query(params![self.domain.0.as_slice(),cursor.map_or([0;32],|c|c.last_key).as_slice(),sql_limit(budget)?])?;
            while let Some(row)=rows.next()? {
                self.require_active_on(c, &ConversationId::new(blob(row, 0)?))?;
                let metadata=metadata::read_metadata(row)?;
                let cost=std::mem::size_of::<ConversationMetadata>();
                if !builder.accepts(cost)? {return Ok(builder.finish(true));}
                builder.push(metadata,0,*metadata.conversation.as_bytes(),cost);
            }
            Ok(builder.finish(false))
        })
    }
}
