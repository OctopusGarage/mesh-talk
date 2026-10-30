//! Current membership roots select live invitations. Signed bodies are read only
//! for those permissions, and an unchanged control prefix reuses its scalar result.
use super::*;
use crate::node::{
    channel::{ChannelMembershipProjection, graph::IndexedGraph},
    channel_account_authority::{AccountAuthority, AccountGrant, AccountGrantId},
};

const SCHEMA: &str = "CREATE TABLE channel_history_send_scan(job BLOB NOT NULL,channel BLOB NOT NULL,stamp INTEGER NOT NULL,ready INTEGER NOT NULL CHECK(ready IN(0,1)),PRIMARY KEY(job,channel)) WITHOUT ROWID;";
pub(super) fn initialize(c: &Connection, fresh: bool) -> Result<(), LogError> {
    if fresh {
        c.execute_batch(SCHEMA)?;
    }
    crate::storage::profile::manifest::validate_ddl(c, SCHEMA)
}
struct ReleasePlan {
    source: ChannelSource,
    payload: Option<(EventKind, Vec<u8>)>,
    ready: bool,
}
impl IndexedView<'_> {
    fn cached_send(&self, stamp: i64) -> Result<Option<bool>, LogError> {
        self.checkpoints.profile().database().read(|c| {
            self.checkpoints.check_source_on(c,&self.source)?;
            c.query_row("SELECT ready FROM channel_history_send_scan WHERE job=?1 AND channel=?2 AND stamp=?3",params![self.source.job.as_slice(),self.source.channel.as_bytes().as_slice(),stamp],|r|r.get(0)).optional().map_err(Into::into)
        })
    }
    fn finish_send(&self, stamp: i64, ready: bool) -> Result<(), LogError> {
        self.checkpoints.profile().database().transaction(|c| {
            self.checkpoints.check_source_on(c,&self.source)?;
            c.execute("INSERT INTO channel_history_send_scan VALUES(?1,?2,?3,?4) ON CONFLICT(job,channel) DO UPDATE SET stamp=excluded.stamp,ready=excluded.ready",params![self.source.job.as_slice(),self.source.channel.as_bytes().as_slice(),stamp,ready])?;
            Ok(())
        })
    }
    fn live_offer(
        &self,
        invitation: EventId,
        recipient: &str,
    ) -> Result<Option<(Event, Offer)>, LogError> {
        let mut after = None;
        loop {
            let page = self.offer_candidates(invitation, recipient, after)?;
            if page.is_empty() {
                return Ok(None);
            }
            for (rank, id) in page {
                after = Some(rank);
                let event = self.repository.get(&id)?.ok_or(LogError::StaleCursor)?;
                if let Some(offer) = self.offer(&event)? {
                    return Ok(Some((event, offer)));
                }
            }
        }
    }
    fn release_page(
        &self,
        offer: EventId,
        after: Option<i64>,
    ) -> Result<Vec<(i64, EventId)>, LogError> {
        self.checkpoints.profile().database().read(|c| {
            self.checkpoints.check_source_on(c,&self.source)?;
            let mut statement=c.prepare("SELECT rank,event FROM channel_history_controls WHERE job=?1 AND channel=?2 AND role IN(2,3) AND offer=?3 AND rank>?4 ORDER BY rank LIMIT 64")?;
            let mut rows=statement.query(params![self.source.job.as_slice(),self.source.channel.as_bytes().as_slice(),offer.as_bytes().as_slice(),after.unwrap_or(-1)])?;
            let mut page=Vec::new();
            while let Some(row)=rows.next()? {
                let id:Vec<u8>=row.get(1)?;
                page.push((row.get(0)?,EventId::new(id.try_into().map_err(|_|LogError::CorruptFile("invalid history release identity".into()))?)));
            }
            Ok(page)
        })
    }
    fn already_released(
        &self,
        event: &Event,
        offer: &Offer,
        current: &crate::channel::ChannelState,
        authority: &AccountAuthority,
        recipient: Option<&PublicIdentity>,
    ) -> Result<(bool, bool), LogError> {
        let graph = IndexedGraph {
            repository: self.repository,
            conversation: self.source.channel,
        };
        let mut after = None;
        let mut found = false;
        let mut own = false;
        loop {
            let page = self.release_page(event.id, after)?;
            if page.is_empty() {
                return Ok((found, own));
            }
            for (rank, id) in page {
                after = Some(rank);
                let prior = self.repository.get(&id)?.ok_or(LogError::StaleCursor)?;
                let valid = if let Some(recipient) = recipient {
                    (decode_account_release(&prior.ciphertext).is_some_and(|value| {
                        value.release.offer == event.id && value.recipient == *recipient
                    }) && account_release_admitted(current, &prior, authority, &graph)?)
                        || (recipient.user_id() == offer.recipient
                            && prior.kind == EventKind::ChannelJoinHistory
                            && self.release(&prior, event, offer)?.is_some())
                } else {
                    prior.kind == EventKind::ChannelJoinHistory
                        && self.release(&prior, event, offer)?.is_some()
                };
                if valid {
                    found = true;
                    own |= prior.author.user_id() == self.me.user_id();
                    if recipient.is_some() || own {
                        return Ok((true, own));
                    }
                }
            }
        }
    }
    fn account_acceptance(
        &self,
        grant: &AccountGrant,
        authority: &AccountAuthority,
    ) -> Result<Option<EventId>, LogError> {
        let graph = IndexedGraph {
            repository: self.repository,
            conversation: self.source.channel,
        };
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
                let page:Vec<(i64,EventId)>=self.checkpoints.profile().database().read(|c| {
                    self.checkpoints.check_source_on(c,&self.source)?;
                    let mut statement=c.prepare("SELECT a.rank,a.event FROM channel_event_authority a INDEXED BY channel_authority_author_kind WHERE a.job=?1 AND a.channel=?2 AND a.kind=?3 AND a.author=?4 AND a.rank>?5 AND a.rank<?6 ORDER BY a.rank LIMIT 64")?;
                    let mut rows=statement.query(params![self.source.job.as_slice(),self.source.channel.as_bytes().as_slice(),EventKind::ChannelAccept as i64,proof.ed25519_pub.as_slice(),after,selected.map_or(i64::MAX,|value|value.0)])?;
                    let mut page=Vec::new();
                    while let Some(row)=rows.next()? {let id:Vec<u8>=row.get(1)?;page.push((row.get(0)?,EventId::new(id.try_into().map_err(|_|LogError::CorruptFile("invalid channel acceptance identity".into()))?)));}
                    Ok(page)
                })?;
                if page.is_empty() {
                    break;
                }
                let mut found = false;
                for (rank, id) in page {
                    after = rank;
                    if !self.descends(id, grant.membership_event)? {
                        continue;
                    }
                    let event = self.repository.get(&id)?.ok_or(LogError::StaleCursor)?;
                    if valid_account_acceptance(&event, grant, authority, &graph)? {
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

impl Node {
    fn plan_join_history_release(
        &self,
        channel: ConversationId,
        projection: &ChannelMembershipProjection,
    ) -> Result<ReleasePlan, NodeError> {
        let log = self.log.lock().expect("log lock");
        let repository = log.repository()?;
        let source = self
            .channel_checkpoints
            .source_binding(channel)?
            .ok_or(LogError::StaleCursor)?;
        let view = IndexedView {
            checkpoints: &self.channel_checkpoints,
            repository,
            me: &self.identity,
            source,
        };
        let stamp = self.channel_checkpoints.control_stamp(&source)?;
        if let Some(ready) = view.cached_send(stamp)? {
            return Ok(ReleasePlan {
                source,
                payload: None,
                ready,
            });
        }
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
            let Some((event, offer)) = view.live_offer(invitation, &recipient)? else {
                continue;
            };
            if view.lifetime_closed(&offer, None)? {
                continue;
            }
            type HistoryRecipient = (PublicIdentity, EventId, Option<(AccountGrantId, EventId)>);
            let mut recipients: Vec<HistoryRecipient> = Vec::new();
            if let Some(grant) = grant {
                let Some(id) = grant.id() else {
                    continue;
                };
                let Some(acceptance) = view.account_acceptance(grant, authority)? else {
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
                    let (released, _) = view.already_released(
                        &event,
                        &offer,
                        current,
                        authority,
                        Some(&proof.public()),
                    )?;
                    if released {
                        ready = true;
                        continue;
                    }
                    recipients.push((proof.public(), acceptance, Some((id, binding))));
                }
            } else {
                let (released, own) =
                    view.already_released(&event, &offer, current, authority, None)?;
                if released {
                    ready |= own;
                    continue;
                }
                let Some(acceptance) = legacy_acceptance else {
                    continue;
                };
                let Some(accepted) = repository.get(&acceptance)? else {
                    continue;
                };
                if accepted.kind != EventKind::ChannelAccept
                    || !accepted.ciphertext.is_empty()
                    || accepted.author.user_id() != recipient
                    || !view.before(acceptance)?.is_some_and(|state| {
                        state.is_member(&recipient) && !state.is_accepted_member(&recipient)
                    })
                    || !view.descends(acceptance, invitation)?
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
            let Some(author) = view.author(event.id)? else {
                continue;
            };
            let Some(mut bytes) = offer.keys.open(&self.identity, &author.x25519_pub) else {
                continue;
            };
            let root = <&[u8; 32]>::try_from(bytes.as_slice())
                .ok()
                .map(|bytes| GroupKey::from_bytes(*bytes));
            bytes.zeroize();
            let Some(root) = root else {
                continue;
            };
            let mut keys = Vec::new();
            let now = crate::node::node::now_millis();
            for row in &offer.rows {
                if self
                    .recalls
                    .lock()
                    .expect("recall lock")
                    .contains(channel, row.id)?
                {
                    continue;
                }
                let key = row_key(&root, row.id);
                let Ok(mut plaintext) = crate::channel::open_channel_message(&key, &row.ciphertext)
                else {
                    continue;
                };
                let visible = crate::expiry::split(&plaintext)
                    .is_some_and(|(deadline, _)| deadline.is_none_or(|deadline| deadline > now));
                plaintext.zeroize();
                if visible {
                    keys.push(RowKey {
                        id: row.id,
                        key: *key.as_bytes(),
                    });
                }
            }
            let bytes = zeroize::Zeroizing::new(encode(&keys));
            let sealed_key = crate::dm::seal(&self.identity, &recipient.x25519_pub, &bytes)
                .map_err(|error| NodeError::Channel(error.to_string()))?;
            let release = Release {
                offer: event.id,
                acceptance,
                sealed_key,
            };
            let payload = if let Some((grant, device_authorization)) = account {
                (
                    EventKind::ChannelDeviceInheritance,
                    encode_account_release(&AccountRelease {
                        version: 1,
                        grant,
                        device_authorization,
                        recipient,
                        release,
                    }),
                )
            } else {
                (
                    EventKind::ChannelJoinHistory,
                    encode(&HistoryControl::Release(release)),
                )
            };
            view.checkpoints.check_source(&source)?;
            return Ok(ReleasePlan {
                source,
                payload: Some(payload),
                ready: true,
            });
        }
        view.finish_send(stamp, ready)?;
        Ok(ReleasePlan {
            source,
            payload: None,
            ready,
        })
    }

    /// Existing scheduling limits cap channels and writes per tick. Each selected
    /// release is source-fenced at allocation and resumed from durable controls.
    pub(in crate::node) fn retry_join_history(&self) -> Result<Vec<ConversationId>, NodeError> {
        let _membership = self
            .channel_membership_commit
            .lock()
            .expect("channel membership commit lock");
        let mut channels = self.channels.lock().expect("channels lock").channel_ids();
        channels.sort_by_key(|channel| *channel.as_bytes());
        if !channels.is_empty() {
            let offset = ((crate::node::node::now_millis() / 3000) as usize).wrapping_mul(32)
                % channels.len();
            channels.rotate_left(offset);
        }
        channels.truncate(32);
        let mut remaining = MAX_RETRIES;
        let mut ready = Vec::new();
        for channel in channels {
            if !self.channel_replica_enabled(channel, &self.user_id()) {
                continue;
            }
            loop {
                let projection = self.account_group_projection(channel)?;
                let Some(current) = projection.state.as_ref() else {
                    break;
                };
                if current.is_dissolved() || !self.channel_identity_is_current(current)? {
                    break;
                }
                let plan = self.plan_join_history_release(channel, &projection)?;
                if plan.ready && !ready.contains(&channel) {
                    ready.push(channel);
                }
                let Some((kind, payload)) = plan.payload else {
                    break;
                };
                self.append_channel_control_from_source(&plan.source, kind, payload)?;
                // Project our completed append before returning, including when
                // this consumes the tick's last write slot.
                self.account_group_projection(channel)?;
                remaining -= 1;
                if remaining == 0 {
                    return Ok(ready);
                }
            }
        }
        Ok(ready)
    }
}
