use super::super::pages::PageBudget;
use super::*;
impl EventRepository {
    pub(crate) fn order_generation(
        &self,
        conv: &ConversationId,
        kind: CausalOrder,
    ) -> Result<Option<OrderGeneration>, LogError> {
        self.database
            .read(|c| self.order_generation_on(c, conv, kind))
    }
    fn order_generation_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
        kind: CausalOrder,
    ) -> Result<Option<OrderGeneration>, LogError> {
        let Some(metadata) = self.metadata_on(c, conv)?.filter(|m| m.count > 0) else {
            return Ok(None);
        };
        let current: Option<(Vec<u8>, i64)> = c.query_row(
            "SELECT job,source_revision FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=?3",
            params![self.domain.0.as_slice(), conv.as_bytes().as_slice(), kind.code()],
            |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?;
        let Some((id, revision)) = current else {
            return Ok(None);
        };
        let id: [u8; 16] = id
            .try_into()
            .map_err(|_| corrupt("invalid current order identity"))?;
        let job = self
            .order_job(c, conv, kind)?
            .ok_or_else(|| corrupt("missing current order job"))?;
        if job.id != id
            || job.phase != READY
            || job.revision != metadata.revision
            || revision != metadata.revision
            || job.count != job.next_rank
            || u64::try_from(job.count).ok() != Some(metadata.count)
        {
            return Err(corrupt("published order disagrees with source"));
        }
        Ok(Some(OrderGeneration {
            job: id,
            source_revision: revision,
            count: metadata.count,
        }))
    }
    /// A durable reducer may resume a known prefix after safe tail appends.
    /// Unlike an immutable query cursor, this checks generation identity while
    /// binding the current source revision within the same read transaction.
    pub(crate) fn causal_headers_after_rank(
        &self,
        conv: &ConversationId,
        kind: CausalOrder,
        job: [u8; 16],
        last_rank: i64,
        budget: PageBudget,
    ) -> Result<OrderReadState<EventHeader>, LogError> {
        self.database.read(|c| {
            let generation = self
                .order_generation_on(c, conv, kind)?
                .ok_or(LogError::StaleCursor)?;
            if generation.job != job
                || last_rank < -1
                || u64::try_from(last_rank).is_ok_and(|rank| rank >= generation.count)
            {
                return Err(LogError::StaleCursor);
            }
            let cursor = OrderCursor {
                store_uuid: self.store_uuid,
                domain: self.domain,
                conversation: *conv,
                order: kind,
                bodies: false,
                job,
                revision: generation.source_revision,
                rank: last_rank,
            };
            self.causal_page(
                c,
                conv,
                kind,
                (last_rank >= 0).then_some(&cursor),
                budget,
                false,
                |_, h| Ok(h),
            )
        })
    }
    pub(crate) fn causal_headers_page(
        &self,
        conv: &ConversationId,
        kind: CausalOrder,
        cursor: Option<&OrderCursor>,
        budget: PageBudget,
    ) -> Result<OrderReadState<EventHeader>, LogError> {
        self.database
            .read(|c| self.causal_page(c, conv, kind, cursor, budget, false, |_, h| Ok(h)))
    }
    pub(crate) fn causal_bodies_page(
        &self,
        conv: &ConversationId,
        kind: CausalOrder,
        cursor: Option<&OrderCursor>,
        budget: PageBudget,
    ) -> Result<OrderReadState<Event>, LogError> {
        self.database.read(|c| {
            self.causal_page(c, conv, kind, cursor, budget, true, |c, h| {
                self.get_on(c, &h.id)?
                    .ok_or_else(|| corrupt("ranked event body disappeared"))
            })
        })
    }
    #[allow(clippy::too_many_arguments)]
    fn causal_page<T>(
        &self,
        c: &Connection,
        conv: &ConversationId,
        kind: CausalOrder,
        cursor: Option<&OrderCursor>,
        budget: PageBudget,
        bodies: bool,
        mut project: impl FnMut(&Connection, EventHeader) -> Result<T, LogError>,
    ) -> Result<OrderReadState<T>, LogError> {
        let limit = i64::try_from(budget.rows)
            .ok()
            .and_then(|n| n.checked_add(1))
            .filter(|_| budget.rows > 0 && budget.bytes > 0)
            .ok_or(LogError::InvalidPageBudget)?;
        let revision = self.scope_revision_on(c, conv)?;
        // Check context before readiness so an old bound cursor cannot become a
        // successful empty page or Pending in another store/conversation/order.
        if let Some(cur) = cursor
            && (cur.store_uuid != self.store_uuid
                || cur.domain != self.domain
                || cur.conversation != *conv
                || cur.order != kind
                || cur.bodies != bodies
                || cur.revision != revision)
        {
            return Err(LogError::StaleCursor);
        }
        let metadata = self.metadata_on(c, conv)?;
        let mut page = OrderPage {
            items: Vec::new(),
            next: None,
            charged_bytes: 0,
        };
        let Some(metadata) = metadata.filter(|m| m.count > 0) else {
            if cursor.is_some() {
                return Err(LogError::StaleCursor);
            }
            return Ok(OrderReadState::Ready(page));
        };
        let current:Option<(Vec<u8>,i64)>=c.query_row("SELECT job,source_revision FROM event_order_current WHERE domain=?1 AND conversation=?2 AND order_kind=?3",params![self.domain.0.as_slice(),conv.as_bytes().as_slice(),kind.code()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
        let Some((id, source_revision)) = current else {
            if self
                .order_job(c, conv, kind)?
                .is_some_and(|j| j.phase == READY)
            {
                return Err(corrupt("ready causal generation lacks current binding"));
            }
            if cursor.is_some() {
                return Err(LogError::StaleCursor);
            }
            return Ok(OrderReadState::Pending);
        };
        let id: [u8; 16] = id
            .try_into()
            .map_err(|_| corrupt("invalid current job identity"))?;
        if source_revision != revision {
            return Err(corrupt("current causal generation is stale"));
        }
        let job = self
            .order_job(c, conv, kind)?
            .ok_or_else(|| corrupt("current causal job missing"))?;
        if job.id != id
            || job.phase != READY
            || job.revision != revision
            || u64::try_from(job.next_rank).ok() != Some(metadata.count)
            || job.count != job.next_rank
        {
            return Err(corrupt("current causal generation disagrees with source"));
        }
        if let Some(cur) = cursor
            && (cur.job != id || cur.rank < 0 || cur.rank >= job.count)
        {
            return Err(LogError::StaleCursor);
        }
        let mut binding = OrderCursor {
            store_uuid: self.store_uuid,
            domain: self.domain,
            conversation: *conv,
            order: kind,
            bodies,
            job: id,
            revision,
            rank: cursor.map_or(-1, |c| c.rank),
        };
        let columns = HEADER_COLUMNS
            .split(',')
            .map(|s| format!("e.{s}"))
            .collect::<Vec<_>>()
            .join(",");
        // The only Vec above has a fixed twelve metadata column names. SQL walks
        // published ranks, and length(body) does not fetch/decode its payload.
        let mut stmt=c.prepare(&format!("SELECT {columns},length(e.body),r.rank FROM event_order_rows r JOIN retained_events e ON e.domain=?1 AND e.id=r.id AND e.conversation=?2 WHERE r.job=?3 AND r.rank>?4 ORDER BY r.rank LIMIT ?5"))?;
        let mut rows = stmt.query(params![
            self.domain.0.as_slice(),
            conv.as_bytes().as_slice(),
            id.as_slice(),
            binding.rank,
            limit
        ])?;
        while let Some(row) = rows.next()? {
            let header = read_header(row)?;
            let rank = nonnegative(row.get(13)?)?;
            if binding.rank.checked_add(1) != Some(rank) {
                return Err(corrupt("causal generation has a rank gap"));
            }
            if size(row, 12)? != header.stored_bytes {
                return Err(corrupt("stored event length disagrees with body"));
            }
            let cost = if bodies {
                header
                    .stored_bytes
                    .checked_mul(3)
                    .and_then(|n| n.checked_add(std::mem::size_of::<Event>()))
                    .ok_or(LogError::RepositoryCounterExhausted)?
            } else {
                std::mem::size_of::<EventHeader>()
            };
            let total = page
                .charged_bytes
                .checked_add(cost)
                .ok_or(LogError::RepositoryCounterExhausted)?;
            if page.items.len() == budget.rows || total > budget.bytes {
                if page.items.is_empty() {
                    return Err(LogError::PageBudgetTooSmall { required: cost });
                }
                page.next = Some(binding);
                return Ok(OrderReadState::Ready(page));
            }
            page.items.reserve_exact(1);
            page.items.push(project(c, header)?);
            page.charged_bytes = total;
            binding.rank = rank;
        }
        if binding.rank.checked_add(1) != Some(job.count) {
            return Err(corrupt("causal generation is incomplete"));
        }
        Ok(OrderReadState::Ready(page))
    }
}
