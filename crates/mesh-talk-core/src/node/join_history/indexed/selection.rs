//! Retry continuations preserve the first valid offer and exact legacy/account
//! release predicates. Their encrypted snapshot is capped by the wire protocol.
use super::continuation::HistoryContext;
use super::*;
use crate::node::channel::ChannelMembershipProjection;
use std::sync::Arc;

pub(super) struct ReceiveSelection {
    pub(super) release: Release,
    pub(super) offer: Offer,
    pub(super) author: PublicIdentity,
    pub(super) invitation: EventHeader,
    pub(super) originals: Vec<EventHeader>,
}
impl HistoryContext {
    pub(super) async fn admitted_account_release(
        &self,
        event: Event,
        projection: Arc<ChannelMembershipProjection>,
    ) -> Result<bool, LogError> {
        self.decision(event.id, move |graph| {
            let Some(current) = projection.state.as_ref() else {
                return Ok(false);
            };
            account_release_admitted(current, &event, &projection.authority, graph)
        })
        .await
    }
    pub(super) async fn prepare_receive(
        &self,
        id: EventId,
        me: PublicIdentity,
        projection: Arc<ChannelMembershipProjection>,
    ) -> Result<Option<ReceiveSelection>, LogError> {
        let Some(event) = self.body(id).await? else {
            return Err(LogError::StaleCursor);
        };
        let (release, account) = if event.kind == EventKind::ChannelDeviceInheritance {
            let Some(value) =
                decode_account_release(&event.ciphertext).filter(|value| value.recipient == me)
            else {
                return Ok(None);
            };
            if !self
                .admitted_account_release(event.clone(), projection)
                .await?
            {
                return Ok(None);
            }
            (value.release, true)
        } else if event.kind == EventKind::ChannelJoinHistory {
            let Some(HistoryControl::Release(value)) = decode(&event.ciphertext) else {
                return Ok(None);
            };
            (value, false)
        } else {
            return Ok(None);
        };
        let Some((offer_event, offer)) = self.first_offer(release.offer).await? else {
            return Ok(None);
        };
        if (!account && offer.recipient != me.user_id())
            || (account && self.lifetime_closed(&offer, Some(event.id)).await?)
            || (!account && self.release(&event, &offer_event, &offer).await?.is_none())
        {
            return Ok(None);
        }
        let Some(author) = self.author(event.id).await? else {
            return Ok(None);
        };
        let Some(invitation) = self.header(offer.invitation).await? else {
            return Ok(None);
        };
        let mut originals = Vec::with_capacity(offer.rows.len());
        for row in &offer.rows {
            let Some(header) = self.header(row.id).await? else {
                return Ok(None);
            };
            originals.push(header);
        }
        Ok(Some(ReceiveSelection {
            release,
            offer,
            author,
            invitation,
            originals,
        }))
    }
    pub(super) async fn live_offer(
        &self,
        invitation: EventId,
        recipient: &str,
    ) -> Result<Option<(Event, Offer)>, LogError> {
        let mut after = None;
        loop {
            let page = self
                .offer_candidates(invitation, recipient.to_owned(), after)
                .await?;
            if page.is_empty() {
                return Ok(None);
            }
            for (rank, id) in page {
                after = Some(rank);
                let event = self.body(id).await?.ok_or(LogError::StaleCursor)?;
                if let Some(offer) = self.offer(&event).await? {
                    return Ok(Some((event, offer)));
                }
            }
        }
    }
    pub(super) async fn already_released(
        &self,
        event: &Event,
        offer: &Offer,
        projection: Arc<ChannelMembershipProjection>,
        recipient: Option<&PublicIdentity>,
        me: &str,
    ) -> Result<(bool, bool), LogError> {
        let mut after = None;
        let mut found = false;
        let mut own = false;
        loop {
            let page = self.release_page(event.id, after).await?;
            if page.is_empty() {
                return Ok((found, own));
            }
            for (rank, id) in page {
                after = Some(rank);
                let prior = self.body(id).await?.ok_or(LogError::StaleCursor)?;
                let valid = if let Some(recipient) = recipient {
                    (decode_account_release(&prior.ciphertext).is_some_and(|value| {
                        value.release.offer == event.id && value.recipient == *recipient
                    }) && self
                        .admitted_account_release(prior.clone(), projection.clone())
                        .await?)
                        || (recipient.user_id() == offer.recipient
                            && prior.kind == EventKind::ChannelJoinHistory
                            && self.release(&prior, event, offer).await?.is_some())
                } else {
                    prior.kind == EventKind::ChannelJoinHistory
                        && self.release(&prior, event, offer).await?.is_some()
                };
                if valid {
                    found = true;
                    own |= prior.author.user_id() == me;
                    if recipient.is_some() || own {
                        return Ok((true, own));
                    }
                }
            }
        }
    }
}

use crate::node::channel_account_authority::{AccountAuthority, AccountGrant, AccountGrantId};
impl HistoryContext {
    async fn account_acceptance(
        &self,
        grant: &AccountGrant,
        authority: &AccountAuthority,
    ) -> Result<Option<EventId>, LogError> {
        let mut selected: Option<(i64, EventId)> = None;
        for proof in authority.original_device_proofs() {
            if authority
                .grant_for_device(&proof.public(), &proof)
                .and_then(AccountGrant::id)
                != grant.id()
            {
                continue;
            }
            let mut after = -1;
            loop {
                let author = proof.ed25519_pub;
                let before = selected.map_or(i64::MAX, |value| value.0);
                let page = self.acceptance_page(author, after, before).await?;
                if page.is_empty() {
                    break;
                }
                let mut found = false;
                for (rank, id) in page {
                    after = rank;
                    if !self.descends(id, grant.membership_event).await? {
                        continue;
                    }
                    let event = self.body(id).await?.ok_or(LogError::StaleCursor)?;
                    let candidate_grant = grant.clone();
                    let candidate_authority = authority.clone();
                    if self
                        .decision(event.id, move |graph| {
                            valid_account_acceptance(
                                &event,
                                &candidate_grant,
                                &candidate_authority,
                                graph,
                            )
                        })
                        .await?
                    {
                        selected = Some((rank, id));
                        found = true;
                        break;
                    }
                }
                if found {
                    break;
                }
            }
        }
        Ok(selected.map(|value| value.1))
    }
}

type HistoryRecipient = (PublicIdentity, EventId, Option<(AccountGrantId, EventId)>);
pub(super) struct SendSelection {
    pub(super) event: Event,
    pub(super) offer: Offer,
    pub(super) root: GroupKey,
    pub(super) recipient: PublicIdentity,
    pub(super) acceptance: EventId,
    pub(super) account: Option<(AccountGrantId, EventId)>,
}
impl HistoryContext {
    pub(super) async fn prepare_send(
        &self,
        me: PublicIdentity,
        projection: Arc<ChannelMembershipProjection>,
    ) -> Result<(Option<SendSelection>, bool), LogError> {
        let current = projection.state.as_ref().ok_or(LogError::StaleCursor)?;
        let authority = &projection.authority;
        let grants = authority.grants();
        // Both collections follow current membership, never historical offers.
        let mut invitations: Vec<_> = grants
            .iter()
            .map(|grant| {
                (
                    grant.membership_event,
                    grant.original.user_id.clone(),
                    Some(grant),
                    None,
                )
            })
            .collect();
        invitations.extend(
            projection
                .accepted
                .iter()
                .filter(|member| {
                    current.is_accepted_member(&member.public.user_id())
                        && !grants.iter().any(|grant| {
                            grant.membership_event == member.membership_event
                                && grant.original.user_id == member.public.user_id()
                        })
                })
                .map(|member| {
                    (
                        member.membership_event,
                        member.public.user_id(),
                        None,
                        member.acceptance_event,
                    )
                }),
        );
        invitations.sort_by(|left, right| (&left.0, &left.1).cmp(&(&right.0, &right.1)));
        let mut ready = false;
        for (invitation, recipient, grant, legacy_acceptance) in invitations {
            if !current.is_accepted_member(&recipient) {
                continue;
            }
            let Some((event, offer)) = self.live_offer(invitation, &recipient).await? else {
                continue;
            };
            if self.lifetime_closed(&offer, None).await? {
                continue;
            }

            let mut recipients: Vec<HistoryRecipient> = Vec::new();
            if let Some(grant) = grant {
                let Some(id) = grant.id() else {
                    continue;
                };
                let Some(acceptance) = self.account_acceptance(grant, authority).await? else {
                    continue;
                };
                for proof in authority.original_device_proofs() {
                    if authority
                        .grant_for_device(&proof.public(), &proof)
                        .and_then(AccountGrant::id)
                        != Some(id)
                    {
                        continue;
                    }
                    let Some(binding) = authority.device_authorization(&proof.public(), &proof)
                    else {
                        continue;
                    };
                    let (released, _) = self
                        .already_released(
                            &event,
                            &offer,
                            projection.clone(),
                            Some(&proof.public()),
                            &me.user_id(),
                        )
                        .await?;
                    if released {
                        ready = true;
                        continue;
                    }
                    recipients.push((proof.public(), acceptance, Some((id, binding))));
                }
            } else {
                let (released, own) = self
                    .already_released(&event, &offer, projection.clone(), None, &me.user_id())
                    .await?;
                if released {
                    ready |= own;
                    continue;
                }
                let Some(acceptance) = legacy_acceptance else {
                    continue;
                };
                let Some(accepted) = self.body(acceptance).await? else {
                    continue;
                };
                if accepted.kind != EventKind::ChannelAccept
                    || !accepted.ciphertext.is_empty()
                    || accepted.author.user_id() != recipient
                    || !self.before(acceptance).await?.is_some_and(|state| {
                        state.is_member(&recipient) && !state.is_accepted_member(&recipient)
                    })
                    || !self.descends(acceptance, invitation).await?
                {
                    continue;
                }
                let Some(public) = current
                    .members()
                    .iter()
                    .find(|member| member.user_id() == recipient)
                else {
                    continue;
                };
                recipients.push((public.clone(), acceptance, None));
            }
            let Some((recipient, acceptance, account)) = recipients.into_iter().next() else {
                continue;
            };
            let Some(author) = self.author(event.id).await? else {
                continue;
            };
            let (offer, root) = self.open_offer_root(offer, author).await?;
            let Some(root) = root else {
                continue;
            };
            return Ok((
                Some(SendSelection {
                    event,
                    offer,
                    root,
                    recipient,
                    acceptance,
                    account,
                }),
                true,
            ));
        }
        Ok((None, ready))
    }
}

impl HistoryContext {
    async fn open_offer_root(
        &self,
        offer: Offer,
        author: PublicIdentity,
    ) -> Result<(Offer, Option<GroupKey>), LogError> {
        let mut offer = Some(offer);
        self.request(move |view, budget| {
            if budget.bytes < SNAPSHOT_BYTES {
                return Err(LogError::PageBudgetTooSmall {
                    required: SNAPSHOT_BYTES,
                });
            }
            let offer = offer.take().ok_or(LogError::StaleCursor)?;
            let root = offer
                .keys
                .open(view.me, &author.x25519_pub)
                .and_then(|mut bytes| {
                    let key = <&[u8; 32]>::try_from(bytes.as_slice())
                        .ok()
                        .map(|bytes| GroupKey::from_bytes(*bytes));
                    bytes.zeroize();
                    key
                });
            Ok(super::continuation::Step::Ready(
                (offer, root),
                super::continuation::units(1, SNAPSHOT_BYTES),
            ))
        })
        .await
    }
}
