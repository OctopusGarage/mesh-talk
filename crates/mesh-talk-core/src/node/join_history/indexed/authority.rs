//! Frozen offers keep the same canonical-prefix and strict-ancestor checks as
//! the pure replay owner. Each lookup holds only the selected bounded control.
use super::*;
impl IndexedView<'_> {
    pub(super) fn descends(&self, event: EventId, parent: EventId) -> Result<bool, LogError> {
        self.repository
            .descends_from(self.source.channel, event, parent)
    }
    fn owner(&self, event: EventId) -> Result<bool, LogError> {
        let Some(header) = self.repository.header(&event)? else {
            return Ok(false);
        };
        self.owner_authorized(&header)
    }
    pub(super) fn offer(&self, event: &Event) -> Result<Option<Offer>, LogError> {
        let Some(HistoryControl::Offer(offer)) = decode(&event.ciphertext) else {
            return Ok(None);
        };
        let Some(invitation) = self.repository.get(&offer.invitation)? else {
            return Ok(None);
        };
        let Some(before) = self.before(invitation.id)? else {
            return Ok(None);
        };
        let Some(state) = self.before(event.id)? else {
            return Ok(None);
        };
        let Some(meta) = crate::channel::ChannelMeta::decode(&invitation.ciphertext) else {
            return Ok(None);
        };
        let Some(mut canonical) = self.canonical(invitation.id, false)? else {
            return Ok(None);
        };
        if canonical.is_member(&offer.recipient) {
            return Ok(None);
        }
        canonical.apply_meta(meta.clone());
        if !canonical.is_member(&offer.recipient)
            || !self.owner(invitation.id)?
            || !self.owner(event.id)?
            || invitation.kind != EventKind::MembershipChange
            || !crate::channel::ChannelMeta::is_invitation(&invitation.ciphertext)
            || before.is_member(&offer.recipient)
            || !meta.is_member(&offer.recipient)
            || !before.can_admin(&invitation.author.user_id())
            || event.author != invitation.author
            || !state.can_admin(&event.author.user_id())
            || state.is_dissolved()
            || !self.descends(event.id, invitation.id)?
            || offer.policy == HistoryPolicy::Off
            || offer.policy != self.policy(Some(invitation.id))?
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
            let Some(original) = self.repository.header(&row.id)? else {
                return Ok(None);
            };
            if !identities.insert(row.id)
                || original.kind != EventKind::Message
                || !offer
                    .policy
                    .permits_time(original.wall_clock, invitation.wall_clock)
                || !self.descends(invitation.id, original.id)?
            {
                return Ok(None);
            }
            let Some(authorized) = self.before(original.id)? else {
                return Ok(None);
            };
            let Some(canonical) = self.canonical(original.id, false)? else {
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
        self.checkpoints.check_source(&self.source)?;
        Ok(Some(offer))
    }
    pub(super) fn lifetime_closed(
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
                let page = self
                    .checkpoints
                    .control_page(&self.source, kind, after, false)?;
                if page.is_empty() {
                    break;
                }
                for (rank, id) in page {
                    after = Some(rank);
                    if !self.descends(id, offer.invitation)? {
                        continue;
                    }
                    if let Some(cutoff) = cutoff
                        && !self.descends(cutoff, id)?
                    {
                        continue;
                    }
                    if self
                        .canonical(id, false)?
                        .is_some_and(|s| s.is_member(&offer.recipient))
                        && self
                            .canonical(id, true)?
                            .is_some_and(|s| !s.is_member(&offer.recipient))
                    {
                        return Ok(true);
                    }
                }
            }
        }
        Ok(false)
    }
    pub(super) fn release(
        &self,
        event: &Event,
        offer_event: &Event,
        offer: &Offer,
    ) -> Result<Option<Release>, LogError> {
        let Some(HistoryControl::Release(release)) = decode(&event.ciphertext) else {
            return Ok(None);
        };
        let Some(acceptance) = self.repository.get(&release.acceptance)? else {
            return Ok(None);
        };
        let Some(state) = self.before(event.id)? else {
            return Ok(None);
        };
        let Some(accepted_before) = self.before(acceptance.id)? else {
            return Ok(None);
        };
        let Some(canonical) = self.canonical(event.id, false)? else {
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
            || !self.descends(acceptance.id, offer.invitation)?
            || !self.descends(event.id, acceptance.id)?
            || !self.descends(event.id, offer_event.id)?
            || self.lifetime_closed(offer, Some(event.id))?
        {
            return Ok(None);
        }
        Ok(Some(release))
    }
    pub(super) fn author(&self, event: EventId) -> Result<Option<PublicIdentity>, LogError> {
        let Some(header) = self.repository.header(&event)? else {
            return Ok(None);
        };
        Ok(self.before(event)?.and_then(|state| {
            state
                .members()
                .iter()
                .find(|member| member.user_id() == header.author.user_id())
                .cloned()
        }))
    }
}
