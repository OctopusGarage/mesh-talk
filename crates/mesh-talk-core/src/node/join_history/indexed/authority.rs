//! Frozen offers keep the same canonical-prefix and strict-ancestor checks as
//! the pure replay owner. Each lookup holds only the selected bounded control.
use super::continuation::HistoryContext;
use super::*;
impl HistoryContext {
    pub(super) async fn offer(&self, event: &Event) -> Result<Option<Offer>, LogError> {
        let Some(HistoryControl::Offer(offer)) = decode(&event.ciphertext) else {
            return Ok(None);
        };
        let Some(invitation) = self.body(offer.invitation).await? else {
            return Ok(None);
        };
        let Some(before) = self.before(invitation.id).await? else {
            return Ok(None);
        };
        let Some(state) = self.before(event.id).await? else {
            return Ok(None);
        };
        let Some(meta) = crate::channel::ChannelMeta::decode(&invitation.ciphertext) else {
            return Ok(None);
        };
        let Some(mut canonical) = self.canonical(invitation.id, false).await? else {
            return Ok(None);
        };
        if canonical.is_member(&offer.recipient) {
            return Ok(None);
        }
        canonical.apply_meta(meta.clone());
        if !canonical.is_member(&offer.recipient)
            || !self.owner(invitation.id).await?
            || !self.owner(event.id).await?
            || invitation.kind != EventKind::MembershipChange
            || !crate::channel::ChannelMeta::is_invitation(&invitation.ciphertext)
            || before.is_member(&offer.recipient)
            || !meta.is_member(&offer.recipient)
            || !before.can_admin(&invitation.author.user_id())
            || event.author != invitation.author
            || !state.can_admin(&event.author.user_id())
            || state.is_dissolved()
            || !self.descends(event.id, invitation.id).await?
            || offer.policy == HistoryPolicy::Off
            || offer.policy != self.policy(Some(invitation.id)).await?
            || offer.keys.sender != event.author.user_id()
            || offer.rows.len() > offer.policy.count()
            || offer
                .rows
                .iter()
                .map(|row| row.ciphertext.len() + 32)
                .sum::<usize>()
                > SNAPSHOT_BYTES
            || offer.keys.entries.len() > MAX_SOURCES
            || offer.keys.entries.iter().any(|entry| {
                entry.user_id == offer.recipient || !before.is_accepted_member(&entry.user_id)
            })
        {
            return Ok(None);
        }
        // The wire policy bounds this set to at most ten copied rows.
        let mut identities = HashSet::new();
        for row in &offer.rows {
            let Some(original) = self.header(row.id).await? else {
                return Ok(None);
            };
            if !identities.insert(row.id)
                || original.kind != EventKind::Message
                || !offer
                    .policy
                    .permits_time(original.wall_clock, invitation.wall_clock)
                || !self.descends(invitation.id, original.id).await?
            {
                return Ok(None);
            }
            let Some(authorized) = self.before(original.id).await? else {
                return Ok(None);
            };
            let Some(canonical) = self.canonical(original.id, false).await? else {
                return Ok(None);
            };
            if offer.keys.entries.iter().any(|source| {
                source.user_id != event.author.user_id()
                    && (!authorized.is_accepted_member(&source.user_id)
                        || !canonical.is_accepted_member(&source.user_id))
            }) {
                return Ok(None);
            }
        }
        Ok(Some(offer))
    }
    pub(super) async fn lifetime_closed(
        &self,
        offer: &Offer,
        cutoff: Option<EventId>,
    ) -> Result<bool, LogError> {
        for kind in [
            EventKind::MembershipChange,
            EventKind::ChannelLeave,
            EventKind::ChannelAccountAuthority,
        ] {
            let mut after = None;
            loop {
                let page = self.control_page(kind, after, false).await?;
                if page.is_empty() {
                    break;
                }
                for (rank, id) in page {
                    after = Some(rank);
                    if !self.descends(id, offer.invitation).await? {
                        continue;
                    }
                    if let Some(cutoff) = cutoff
                        && !self.descends(cutoff, id).await?
                    {
                        continue;
                    }
                    if self
                        .canonical(id, false)
                        .await?
                        .is_some_and(|s| s.is_member(&offer.recipient))
                        && self
                            .canonical(id, true)
                            .await?
                            .is_some_and(|s| !s.is_member(&offer.recipient))
                    {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
    pub(super) async fn release(
        &self,
        event: &Event,
        offer_event: &Event,
        offer: &Offer,
    ) -> Result<Option<Release>, LogError> {
        let Some(HistoryControl::Release(release)) = decode(&event.ciphertext) else {
            return Ok(None);
        };
        let Some(acceptance) = self.body(release.acceptance).await? else {
            return Ok(None);
        };
        let Some(state) = self.before(event.id).await? else {
            return Ok(None);
        };
        let Some(accepted_before) = self.before(acceptance.id).await? else {
            return Ok(None);
        };
        let Some(canonical) = self.canonical(event.id, false).await? else {
            return Ok(None);
        };
        if canonical.is_dissolved()
            || !canonical.is_accepted_member(&offer.recipient)
            || !canonical.is_accepted_member(&event.author.user_id())
            || release.offer != offer_event.id
            || acceptance.kind != EventKind::ChannelAccept
            || !acceptance.ciphertext.is_empty()
            || acceptance.author.user_id() != offer.recipient
            || !accepted_before.is_member(&offer.recipient)
            || accepted_before.is_accepted_member(&offer.recipient)
            || !state.is_accepted_member(&offer.recipient)
            || state.is_dissolved()
            || !state.is_accepted_member(&event.author.user_id())
            || !offer
                .keys
                .entries
                .iter()
                .any(|source| source.user_id == event.author.user_id())
            || !self.descends(acceptance.id, offer.invitation).await?
            || !self.descends(event.id, acceptance.id).await?
            || !self.descends(event.id, offer_event.id).await?
            || self.lifetime_closed(offer, Some(event.id)).await?
        {
            return Ok(None);
        }
        Ok(Some(release))
    }
    pub(super) async fn author(&self, event: EventId) -> Result<Option<PublicIdentity>, LogError> {
        let Some(header) = self.header(event).await? else {
            return Ok(None);
        };
        Ok(self.before(event).await?.and_then(|state| {
            state
                .members()
                .iter()
                .find(|member| member.user_id() == header.author.user_id())
                .cloned()
        }))
    }
}

impl HistoryContext {
    pub(super) async fn policy(&self, cutoff: Option<EventId>) -> Result<HistoryPolicy, LogError> {
        let mut after = None;
        loop {
            let page = self
                .control_page(EventKind::ChannelHistoryPolicy, after, true)
                .await?;
            if page.is_empty() {
                return Ok(HistoryPolicy::default());
            }
            for (rank, id) in page {
                after = Some(rank);
                if let Some(cutoff) = cutoff
                    && !self.descends(cutoff, id).await?
                {
                    continue;
                }
                if !self.owner(id).await? {
                    continue;
                }
                let event = self.body(id).await?.ok_or(LogError::StaleCursor)?;
                if let Some(policy) = decode(&event.ciphertext) {
                    return Ok(policy);
                }
            }
        }
    }
}

impl HistoryContext {
    pub(super) async fn first_offer(
        &self,
        selected: EventId,
    ) -> Result<Option<(Event, Offer)>, LogError> {
        let Some(event) = self.body(selected).await? else {
            return Ok(None);
        };
        if event.kind != EventKind::ChannelJoinHistory {
            return Ok(None);
        }
        let Some(HistoryControl::Offer(shape)) = decode(&event.ciphertext) else {
            return Ok(None);
        };
        let mut after = None;
        loop {
            let page = self
                .offer_candidates(shape.invitation, shape.recipient.clone(), after)
                .await?;
            if page.is_empty() {
                return Ok(None);
            }
            for (rank, id) in page {
                after = Some(rank);
                let Some(candidate) = self.body(id).await? else {
                    return Err(LogError::StaleCursor);
                };
                if let Some(offer) = self.offer(&candidate).await? {
                    return Ok((id == selected).then_some((candidate, offer)));
                }
                if id == selected {
                    return Ok(None);
                }
            }
        }
    }
}
