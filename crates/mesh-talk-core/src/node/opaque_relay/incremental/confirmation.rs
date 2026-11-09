//! Exact target resolution reuses qualified membership only under an unchanged
//! completed local selection and freshly observed relay incarnation.
use super::*;

#[derive(Default)]
struct ConfirmationLookup {
    outer: Option<EventId>,
    // True only after a ready selection proved actual absence. Missing bodies,
    // unavailable views and permission failures never produce this evidence.
    unselected: bool,
}

impl Node {
    /// Recreate the existing selected export for a target absent from the
    /// completed proof's bounded qualified membership memo.
    pub(in crate::node) async fn prepare_opaque_confirmation<F: Fn() -> bool + Sync>(
        &self,
        p: &mut OpaqueMailboxProgress,
        current: &F,
    ) -> Result<(), session::SessionError> {
        if !matches!(p.incremental, Mode::Active(_)) {
            return Ok(());
        }
        let generation = self
            .privacy
            .generation
            .load(std::sync::atomic::Ordering::Acquire);
        let Some(conversation) = p.conversation else {
            return Ok(());
        };
        loop {
            if !self.upload_permitted(p, current)
                || self
                    .privacy
                    .generation
                    .load(std::sync::atomic::Ordering::Acquire)
                    != generation
            {
                return Err(session::SessionError::UnexpectedMessage);
            }
            let ready = {
                let owner = self.sync_store(&p.recipient);
                let store = owner.lock().expect("projection");
                let mut entry = state(p).lease.0.lock().expect("upload entry");
                if entry.view.is_none() {
                    entry.view = Some(store.open_history_upload(&conversation)?);
                }
                let (step, advanced) = {
                    let view = entry.view.as_ref().expect("confirmation view");
                    let before = view.preparation_progress();
                    let step = store.prepare_history_upload_step(
                        view,
                        crate::eventlog::sync::network::PreparationBudget::default(),
                    )?;
                    store.validate_history_upload(view)?;
                    let advanced = step.rows != 0
                        || step.edges != 0
                        || step.bytes != 0
                        || view.preparation_progress() != before;
                    (step, advanced)
                };
                entry.note_progress(advanced, generation, std::time::Instant::now());
                #[cfg(test)]
                HISTORY_METADATA_ROWS.with(|rows| rows.set(rows.get() + step.rows));
                step.ready
            };
            if !self.upload_permitted(p, current)
                || self
                    .privacy
                    .generation
                    .load(std::sync::atomic::Ordering::Acquire)
                    != generation
            {
                return Err(session::SessionError::UnexpectedMessage);
            }
            if ready {
                return Ok(());
            }
            tokio::task::yield_now().await;
        }
    }
    fn confirmation_certificate(
        &self,
        p: &OpaqueMailboxProgress,
    ) -> Result<Option<CompletedUpload>, LogError> {
        let Mode::Active(s) = &p.incremental else {
            return Ok(None);
        };
        let (Some(certificate), Some(remote), Some(floor), Some(conversation)) = (
            s.receipt.completed.as_ref(),
            s.remote.as_ref(),
            s.remote_floor,
            p.conversation,
        ) else {
            return Ok(None);
        };
        if !certificate.accepts(remote, floor) {
            return Ok(None);
        }
        let current = self
            .sync_store(&p.recipient)
            .lock()
            .expect("projection")
            .history_sync_stamp(&conversation)?;
        Ok((current.as_ref() == Some(&certificate.local)).then(|| certificate.clone()))
    }

    pub(in crate::node) async fn resolve_opaque_confirmation<F: Fn() -> bool + Sync>(
        &self,
        p: &mut OpaqueMailboxProgress,
        inner: EventId,
        current: &F,
    ) -> Result<Option<EventId>, session::SessionError> {
        let generation = self
            .privacy
            .generation
            .load(std::sync::atomic::Ordering::Acquire);
        let permitted = || {
            self.upload_permitted(p, current)
                && self
                    .privacy
                    .generation
                    .load(std::sync::atomic::Ordering::Acquire)
                    == generation
        };
        if !permitted() {
            return Err(session::SessionError::UnexpectedMessage);
        }
        let before = self.confirmation_certificate(p)?;
        if let Some(outer) = before.as_ref().and_then(|certificate| {
            certificate
                .targets
                .iter()
                .find(|(id, _)| *id == inner)
                .map(|(_, outer)| *outer)
        }) {
            if !permitted() {
                return Err(session::SessionError::UnexpectedMessage);
            }
            // A memo certifies selected membership, never current relay custody.
            // The caller still performs its exact durable-have exchange.
            return Ok(Some(outer));
        }
        if let Some(certificate) = &before {
            let absent = self
                .opaque_upload_cache
                .lock()
                .expect("upload cache")
                .unselected_target(&state(p).key, &certificate.local, inner);
            if absent {
                // A runtime memo skips reconstruction only. Current authority
                // and fallible source observation still run for this request.
                if !permitted() {
                    return Err(session::SessionError::UnexpectedMessage);
                }
                if self
                    .confirmation_certificate(p)?
                    .as_ref()
                    .is_some_and(|after| after.local == certificate.local)
                {
                    if !permitted() {
                        return Err(session::SessionError::UnexpectedMessage);
                    }
                    return Ok(None);
                }
            }
        }
        // A completed upload may have only its compact certificate left. A
        // view reconstructed solely for this lookup is not unfinished upload
        // work and must not pin the cache's protected preparation slots.
        let confirmation_only = matches!(p.incremental, Mode::Active(_))
            && state(p).history_done
            && state(p)
                .lease
                .0
                .lock()
                .expect("upload entry")
                .view
                .is_none();
        self.prepare_opaque_confirmation(p, current).await?;
        let lookup = self.lookup_opaque_confirmation(p, inner)?;
        let outer = lookup.outer;
        if !self.upload_permitted(p, current)
            || self
                .privacy
                .generation
                .load(std::sync::atomic::Ordering::Acquire)
                != generation
        {
            return Err(session::SessionError::UnexpectedMessage);
        }
        let after = self.confirmation_certificate(p)?;
        let qualified = before
            .as_ref()
            .zip(after.as_ref())
            .is_some_and(|(before, after)| {
                before.local == after.local
                    && before.remote == after.remote
                    && before.upper == after.upper
                    && before.revision == after.revision
            });
        if let Some(outer) = outer
            && qualified
        {
            state_mut(p)
                .receipt
                .completed
                .as_mut()
                .expect("qualified completion")
                .remember_target(inner, outer);
            self.save_mailbox_receipt(p)?;
        }
        if !self.upload_permitted(p, current)
            || self
                .privacy
                .generation
                .load(std::sync::atomic::Ordering::Acquire)
                != generation
        {
            return Err(session::SessionError::UnexpectedMessage);
        }
        if lookup.unselected && qualified {
            self.opaque_upload_cache
                .lock()
                .expect("upload cache")
                .remember_unselected(
                    &state(p).key,
                    &before.as_ref().expect("qualified source").local,
                    inner,
                );
        }
        if confirmation_only {
            let retired = state(p).lease.0.lock().expect("upload entry").view.take();
            drop(retired);
        }
        Ok(outer)
    }

    fn lookup_opaque_confirmation(
        &self,
        p: &OpaqueMailboxProgress,
        inner: EventId,
    ) -> Result<ConfirmationLookup, session::SessionError> {
        let Mode::Active(s) = &p.incremental else {
            return Ok(ConfirmationLookup {
                outer: p
                    .uploaded()
                    .iter()
                    .find(|(id, _)| *id == inner)
                    .map(|(_, outer)| *outer),
                unselected: false,
            });
        };
        if !self.relay_send_allowed(&p.relay) {
            return Ok(ConfirmationLookup::default());
        }
        let entry = s.lease.0.lock().expect("upload entry");
        let Some(view) = &entry.view else {
            return Ok(ConfirmationLookup::default());
        };
        let owner = self.sync_store(&p.recipient);
        let store = owner.lock().expect("projection");
        if !store.history_upload_contains(view, inner)? {
            return Ok(ConfirmationLookup {
                outer: None,
                unselected: true,
            });
        }
        if let Some(outer) = self.cached_wrapper_id(p.mailbox.id(), inner) {
            return Ok(ConfirmationLookup {
                outer: Some(outer),
                unselected: false,
            });
        }
        let Some(event) = store.history_upload_event(view, inner, PAGE_BYTES)? else {
            return Ok(ConfirmationLookup::default());
        };
        let outer = p
            .mailbox
            .wrap(&event)
            .map_err(|error| session::SessionError::Serialization(error.to_string()))?
            .id;
        store.validate_history_upload(view)?;
        self.remember_wrapper_id(p.mailbox.id(), inner, outer);
        Ok(ConfirmationLookup {
            outer: Some(outer),
            unselected: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn stamp(byte: u8) -> HistorySyncStamp {
        bincode::deserialize(&[byte; 32]).expect("fixed-size history fingerprint")
    }
    #[test]
    fn unselected_membership_is_bounded_scoped_and_version_qualified() {
        let mut cache = UploadCache::default();
        let inner = EventId::new([7; 32]);
        for index in 0..=MAX_UNSELECTED_TARGETS {
            cache.remember_unselected(&(index as u16).to_le_bytes(), &stamp(1), inner);
        }
        assert_eq!(cache.unselected.len(), MAX_UNSELECTED_TARGETS);
        assert!(!cache.unselected_target(&0u16.to_le_bytes(), &stamp(1), inner));
        let scope = (MAX_UNSELECTED_TARGETS as u16).to_le_bytes();
        assert!(cache.unselected_target(&scope, &stamp(1), inner));
        assert!(!cache.unselected_target(&scope, &stamp(1), EventId::new([8; 32])));
        assert!(!cache.unselected_target(&scope, &stamp(2), inner));
        assert!(
            !cache.unselected_target(&scope, &stamp(1), inner),
            "new source retires old absence evidence"
        );
        let before = cache.unselected.len();
        cache.remember_unselected(&[1; 257], &stamp(1), inner);
        assert_eq!(
            cache.unselected.len(),
            before,
            "oversized scope must not become retained metadata"
        );
        cache.clear();
        assert!(cache.unselected.is_empty());
    }
}
