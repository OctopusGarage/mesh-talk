//! Charged selected-local snapshot and scalar bounded merge state. Remote
//! arrival authority remains exclusively relay_federation_entries.
use super::*;
impl SessionLease {
    /// Caller owns the role's current transaction and finite view validation.
    /// Both snapshot sides share the same global/session charges and cleanup.
    #[cfg(test)]
    pub(crate) fn stage_local_page_on(
        &self,
        c: &Connection,
        entries: &[FederatedMailbox],
        last: bool,
        now: u64,
    ) -> Result<(), LogError> {
        self.stage_local_batch_on(c, entries, now)?;
        self.finish_local_frame_on(c, entries.len(), last)
    }
    /// Derived raw selection slices do not consume wire frames. The sender can
    /// coalesce many empty/noneligible slices into one bounded encoded page.
    pub(crate) fn stage_local_batch_on(
        &self,
        c: &Connection,
        entries: &[FederatedMailbox],
        now: u64,
    ) -> Result<(), LogError> {
        if entries.len() > PAGE_SIZE {
            return Err(LogError::InvalidPageBudget);
        }
        self.validate_on(c, false)?;
        let (selected,complete,session_rows,session_bytes):(i64,i64,i64,i64)=c.query_row("SELECT local_entries,local_complete,row_charge,byte_charge FROM relay_federation_sessions WHERE session_id=?1",[self.session_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let selected = selected
            .checked_add(entries.len() as i64)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        if complete != 0 || selected > PAGE_SIZE as i64 * MAX_PAGES {
            return Err(LogError::AdmissionRejected(
                "oversized or completed local snapshot".into(),
            ));
        }
        let mut unique = std::collections::BTreeMap::new();
        for entry in entries {
            validate_origin(&entry.id, entry.first_seen_secs, now)?;
            if let Some(prior) =
                unique.insert(*entry.id.as_bytes(), (entry.digest, entry.first_seen_secs))
                && prior != (entry.digest, entry.first_seen_secs)
            {
                return Err(invalid("conflicting selected local digest"));
            }
        }
        let mut additional = 0i64;
        for (key, (digest, first)) in &unique {
            let prior:Option<(Vec<u8>,Vec<u8>)>=c.query_row("SELECT digest,first_seen FROM relay_federation_local_entries WHERE session_id=?1 AND conversation=?2",params![self.session_id,key.as_slice()],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            match prior {
                None => additional += 1,
                Some((d, f)) if d.as_slice() != digest || f.as_slice() != first.to_be_bytes() => {
                    return Err(invalid("selected local snapshot changed"));
                }
                _ => {}
            }
        }
        let cost = additional
            .checked_mul(ENTRY_BYTES)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let (rows, bytes): (i64, i64) = c.query_row(
            "SELECT staged_rows,staged_bytes FROM relay_federation_runtime WHERE singleton=1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let rows = rows
            .checked_add(additional)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let bytes = bytes
            .checked_add(cost)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let session_rows = session_rows
            .checked_add(additional)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let session_bytes = session_bytes
            .checked_add(cost)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        if rows > MAX_ROWS || bytes > MAX_BYTES || session_rows > MAX_ROWS {
            return Err(LogError::RelayCapacityUnavailable);
        }
        for (key, (digest, first)) in unique {
            c.execute("INSERT INTO relay_federation_local_entries(session_id,conversation,digest,first_seen) VALUES(?1,?2,?3,?4) ON CONFLICT(session_id,conversation) DO NOTHING",params![self.session_id,key.as_slice(),digest.as_slice(),first.to_be_bytes().as_slice()])?;
        }
        c.execute("UPDATE relay_federation_sessions SET local_entries=?2,row_charge=?3,byte_charge=?4 WHERE session_id=?1",params![self.session_id,selected,session_rows,session_bytes])?;
        c.execute(
            "UPDATE relay_federation_runtime SET staged_rows=?1,staged_bytes=?2 WHERE singleton=1",
            params![rows, bytes],
        )?;
        self.validate_on(c, false)
    }
    pub(crate) fn finish_local_frame_on(
        &self,
        c: &Connection,
        entries: usize,
        last: bool,
    ) -> Result<(), LogError> {
        if entries > PAGE_SIZE {
            return Err(LogError::InvalidPageBudget);
        }
        self.validate_on(c, false)?;
        let (pages,complete,selected,framed):(i64,i64,i64,i64)=c.query_row("SELECT local_pages,local_complete,local_entries,local_framed_entries FROM relay_federation_sessions WHERE session_id=?1",[self.session_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        let pages = pages
            .checked_add(1)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        let framed = framed
            .checked_add(entries as i64)
            .ok_or(LogError::RepositoryCounterExhausted)?;
        if complete != 0
            || pages > MAX_PAGES
            || (!last && pages == MAX_PAGES)
            || framed > selected
            || (last && framed != selected)
        {
            return Err(LogError::AdmissionRejected(
                "incomplete or oversized local federation frames".into(),
            ));
        }
        c.execute("UPDATE relay_federation_sessions SET local_pages=?2,local_complete=?3,local_framed_entries=?4 WHERE session_id=?1",params![self.session_id,pages,i64::from(last),framed])?;
        self.validate_on(c, false)
    }
    pub(crate) fn validate_both_complete_on(&self, c: &Connection) -> Result<(), LogError> {
        self.validate_on(c, true)?;
        let complete: i64 = c.query_row(
            "SELECT local_complete FROM relay_federation_sessions WHERE session_id=?1",
            [self.session_id],
            |r| r.get(0),
        )?;
        if complete != 1 {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
    pub(crate) fn begin_mismatch_on(
        &self,
        c: &Connection,
        pivot: Option<&ConversationId>,
    ) -> Result<(), LogError> {
        self.validate_both_complete_on(c)?;
        if c.execute("UPDATE relay_federation_sessions SET scan_phase=1,scan_pivot=?2,local_after=?2,remote_after=?2 WHERE session_id=?1 AND scan_phase=0",params![self.session_id,pivot.map(|p|p.as_bytes().as_slice())])?!=1{return Err(LogError::StaleCursor)}
        Ok(())
    }
    /// <=16 emitted, <=512 raw indexed probes. Empty with done=false is bounded
    /// progress; callers must continue rather than report convergence.
    pub(crate) fn mismatch_step_on(
        &self,
        c: &Connection,
        examined: usize,
        emitted: usize,
    ) -> Result<MismatchProgress, LogError> {
        if !(2..=512).contains(&examined) || !(1..=16).contains(&emitted) {
            return Err(LogError::InvalidPageBudget);
        }
        self.validate_both_complete_on(c)?;
        type ScanCursorRow = (i64, Option<Vec<u8>>, Option<Vec<u8>>, Option<Vec<u8>>);
        let (mut phase,pivot,mut local_after,mut remote_after):ScanCursorRow=c.query_row("SELECT scan_phase,scan_pivot,local_after,remote_after FROM relay_federation_sessions WHERE session_id=?1",[self.session_id],|r|Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?)))?;
        if phase == 0 {
            return Err(LogError::StaleCursor);
        }
        let mut p = MismatchProgress {
            ids: Vec::with_capacity(emitted),
            examined: 0,
            done: phase == 3,
        };
        while phase != 3 && p.examined + 2 <= examined && p.ids.len() < emitted {
            let local = peek(
                c,
                "local",
                self.session_id,
                local_after.as_deref().unwrap_or(&[]),
                if phase == 2 { pivot.as_deref() } else { None },
            )?;
            let remote = peek(
                c,
                "remote",
                self.session_id,
                remote_after.as_deref().unwrap_or(&[]),
                if phase == 2 { pivot.as_deref() } else { None },
            )?;
            p.examined += 2;
            let key = match (&local, &remote) {
                (None, None) => {
                    if phase == 1 && pivot.is_some() {
                        phase = 2;
                        local_after = None;
                        remote_after = None;
                        continue;
                    }
                    phase = 3;
                    break;
                }
                (Some((l, _)), Some((r, _))) => l.min(r).clone(),
                (Some((l, _)), None) => l.clone(),
                (None, Some((r, _))) => r.clone(),
            };
            let l = local.as_ref().filter(|(k, _)| *k == key);
            let r = remote.as_ref().filter(|(k, _)| *k == key);
            if l.map(|(_, d)| d) != r.map(|(_, d)| d) {
                p.ids.push(ConversationId::new(
                    key.as_slice()
                        .try_into()
                        .map_err(|_| invalid("malformed mismatch key"))?,
                ));
            }
            if l.is_some() {
                local_after = Some(key.clone())
            }
            if r.is_some() {
                remote_after = Some(key)
            }
        }
        p.done = phase == 3;
        c.execute("UPDATE relay_federation_sessions SET scan_phase=?2,local_after=?3,remote_after=?4 WHERE session_id=?1",params![self.session_id,phase,local_after,remote_after])?;
        self.validate_both_complete_on(c)?;
        Ok(p)
    }
}
pub(crate) struct MismatchProgress {
    pub ids: Vec<ConversationId>,
    pub examined: usize,
    pub done: bool,
}
type InventoryDigestEntry = (Vec<u8>, [u8; 32]);
fn peek(
    c: &Connection,
    side: &str,
    session: i64,
    after: &[u8],
    upper: Option<&[u8]>,
) -> Result<Option<InventoryDigestEntry>, LogError> {
    let sql = match (side, upper.is_some()) {
        ("local", false) => {
            "SELECT conversation,digest FROM relay_federation_local_entries WHERE session_id=?1 AND conversation>?2 ORDER BY conversation LIMIT 1"
        }
        ("local", true) => {
            "SELECT conversation,digest FROM relay_federation_local_entries WHERE session_id=?1 AND conversation>?2 AND conversation<=?3 ORDER BY conversation LIMIT 1"
        }
        (_, false) => {
            "SELECT conversation,digest FROM relay_federation_entries WHERE session_id=?1 AND conversation>?2 ORDER BY conversation LIMIT 1"
        }
        (_, true) => {
            "SELECT conversation,digest FROM relay_federation_entries WHERE session_id=?1 AND conversation>?2 AND conversation<=?3 ORDER BY conversation LIMIT 1"
        }
    };
    let row: Option<(Vec<u8>, Vec<u8>)> = if let Some(upper) = upper {
        c.query_row(sql, params![session, after, upper], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .optional()?
    } else {
        c.query_row(sql, params![session, after], |r| Ok((r.get(0)?, r.get(1)?)))
            .optional()?
    };
    row.map(|(key, digest)| {
        if key.len() != 32 {
            return Err(invalid("malformed mismatch key"));
        }
        Ok((
            key,
            digest
                .try_into()
                .map_err(|_| invalid("malformed staged digest"))?,
        ))
    })
    .transpose()
}
