//! Participant-owned mailbox watches; hints invalidate, existing drains ingest.
use super::*;
use crate::postoffice::{MailboxWatchToken, opaque::RelayMailbox};
use futures_util::{FutureExt, StreamExt, stream::FuturesUnordered};

type Watch = (SecureChannel<TcpStream>, Vec<MailboxWatchToken>);
const WATCH_REPAIR: Duration = Duration::from_secs(300);

enum Completion {
    Drain {
        result: Option<bool>,
        watch: Option<Watch>,
        retry_watch: Option<Duration>,
    },
    Watch(Option<Watch>),
}

impl Node {
    fn parked_receive_deadline(
        &self,
        candidates: &[PeerRecord],
        active: &[DrainKey],
        watching: &[DrainKey],
        now: tokio::time::Instant,
    ) -> tokio::time::Instant {
        // Expiring retry cooldowns do not publish an event. Only fully watched
        // or currently owned relays may use the long quiet deadline.
        let waiting = candidates
            .iter()
            .filter(|peer| {
                let key = self.drain_key(peer);
                !active.contains(&key) && !watching.contains(&key)
            })
            .map(|peer| peer.public.user_id())
            .collect::<Vec<_>>();
        let retries = self
            .relay_retry_after
            .lock()
            .expect("relay retry lock not poisoned");
        waiting.iter().fold(now + WATCH_REPAIR, |deadline, id| {
            let due = retries
                .get(id)
                .copied()
                .map(tokio::time::Instant::from_std)
                .unwrap_or(now + POST_OFFICE_DRAIN_INTERVAL);
            deadline.min(due.max(now))
        })
    }

    fn receive_watch_scopes(
        &self,
        po: &PeerRecord,
        peers: &[crate::discovery::Announce],
    ) -> Option<Vec<ConversationId>> {
        let federated = self.bridge_directory_allowed();
        if peers.is_empty() || peers.len() > if federated { 32 } else { 64 } {
            return None;
        }
        let mut scopes = Vec::new();
        for peer in peers {
            scopes.push(
                RelayMailbox::new(&self.identity, &peer.public(), &po.public)
                    .ok()?
                    .id(),
            );
            if federated {
                scopes.push(
                    RelayMailbox::federated(&self.identity, &peer.public())
                        .ok()?
                        .id(),
                );
            }
        }
        scopes.sort_unstable_by_key(|conversation| *conversation.as_bytes());
        scopes.dedup();
        Some(scopes)
    }

    async fn drain_with_watch(
        &self,
        po: &PeerRecord,
        try_watch: bool,
        mut watch: Option<Watch>,
    ) -> Completion {
        if self.cached_peer_snapshot_async().await.is_err() {
            log::warn!("verified discovery cache update failed");
        }
        let peers = self.post_office_drain_peers();
        if peers.is_empty() {
            return Completion::Drain {
                result: None,
                watch: None,
                retry_watch: None,
            };
        }
        let mut retry_watch = None;
        let mut legacy_channel = None;
        if watch.is_none() && try_watch {
            if let Some(scopes) = self.receive_watch_scopes(po, &peers) {
                let observed = tokio::time::timeout(
                    super::super::delivery_runtime::RELAY_EXCHANGE_TIMEOUT,
                    async {
                        let mut channel = self.privacy_watch_dial(po.addr, &po.public).await?;
                        if !super::super::session::mailbox_watch::negotiate(&mut channel).await? {
                            // Explicit legacy support stays on this authenticated
                            // connection. Transfer actual capacity before using it
                            // as an ordinary sequential exchange; no second dial.
                            channel.retain_connection_permits(self.sessions.outbound().await?);
                            return Ok((None, Some(channel)));
                        }
                        let tokens =
                            super::super::session::mailbox_watch::observe(&mut channel, &scopes)
                                .await?;
                        Ok::<_, SessionError>((Some((channel, tokens)), None))
                    },
                )
                .await;
                match observed {
                    Ok(Ok((Some(current), _))) => watch = Some(current),
                    Ok(Ok((None, channel))) => {
                        legacy_channel = channel;
                        retry_watch = Some(WATCH_REPAIR);
                    }
                    _ => retry_watch = Some(Duration::from_secs(30)),
                }
            } else {
                retry_watch = Some(WATCH_REPAIR);
            }
        }
        // Observation tokens survive connection replacement. Never park a socket
        // during the aggregate drain: many individually bounded scopes can exceed
        // the server's between-request deadline.
        let observed = watch.take().map(|(channel, tokens)| {
            drop(channel);
            tokens
        });
        // Observe BEFORE draining. Every captured peer gets a bounded turn before
        // quiet admission; a shared rotating cursor alone is not full coverage.
        for _ in 0..peers.len().div_ceil(8) {
            if !self
                .drain_one_post_office_with_channel(po, &peers, legacy_channel.take())
                .await
            {
                return Completion::Drain {
                    result: Some(false),
                    watch: None,
                    retry_watch: Some(Duration::from_secs(30)),
                };
            }
        }
        if let Some(tokens) = observed {
            #[cfg(test)]
            self.trace_account_client(&po.public, || "mailbox_watch_rearming".into());
            let rearmed = tokio::time::timeout(
                super::super::delivery_runtime::RELAY_EXCHANGE_TIMEOUT,
                async {
                    let mut channel = self.privacy_watch_dial(po.addr, &po.public).await?;
                    if !super::super::session::mailbox_watch::negotiate(&mut channel).await? {
                        return Err(SessionError::ResourceUnavailable);
                    }
                    Ok::<_, SessionError>((channel, tokens))
                },
            )
            .await;
            match rearmed {
                Ok(Ok(current)) => watch = Some(current),
                _ => retry_watch = Some(Duration::from_secs(30)),
            }
        }
        Completion::Drain {
            result: Some(true),
            watch,
            retry_watch,
        }
    }

    pub(super) async fn run_watched_post_office_drains(self: Arc<Self>) {
        let mut network = self.network_changes.subscribe();
        let mut roster = self
            .roster
            .lock()
            .expect("roster mutex not poisoned")
            .subscribe_changes();
        let mut source = self.storage.database().subscribe_source_changes();
        loop {
            let _ = *source.borrow_and_update();
            let generation = self
                .privacy
                .generation
                .load(std::sync::atomic::Ordering::SeqCst);
            let mut candidates = self.post_office_drain_candidates();
            let mut schedule = PostOfficeDrainSchedule::default();
            let mut active = Vec::new();
            let mut watching = Vec::new();
            let mut dirty = Vec::new();
            let mut ready: Vec<(DrainKey, Watch)> = Vec::new();
            let mut retry_watch: Vec<(DrainKey, tokio::time::Instant)> = Vec::new();
            let mut cancellations = Vec::new();
            let mut attempts = FuturesUnordered::new();
            loop {
                let eligible = candidates
                    .iter()
                    .filter(|po| {
                        self.relay_receive_allowed(&po.public) && self.relay_retry_due(&po.public)
                    })
                    .map(|po| (self.drain_key(po), po.clone()))
                    .filter(|(key, _)| !watching.contains(key))
                    .collect::<Vec<_>>();
                while let Some(index) =
                    schedule.select(&eligible, &active, tokio::time::Instant::now())
                {
                    let (key, po) = eligible[index].clone();
                    let watch = ready
                        .iter()
                        .position(|(k, _)| k == &key)
                        .map(|i| ready.swap_remove(i).1);
                    let try_watch = candidates
                        .iter()
                        .take(super::super::session_pool::MAX_WATCH_CONNECTIONS)
                        .any(|candidate| self.drain_key(candidate) == key)
                        && !retry_watch
                            .iter()
                            .any(|(k, at)| k == &key && *at > tokio::time::Instant::now());
                    active.push(key.clone());
                    let (abort, registration) = futures_util::future::AbortHandle::new_pair();
                    cancellations.push((key.clone(), abort));
                    let node = &self;
                    attempts.push(
                        async move {
                            let result = futures_util::future::Abortable::new(
                                node.drain_with_watch(&po, try_watch, watch),
                                registration,
                            )
                            .await;
                            (key, result)
                        }
                        .boxed(),
                    );
                }
                let wake = if eligible.is_empty() {
                    self.parked_receive_deadline(
                        &candidates,
                        &active,
                        &watching,
                        tokio::time::Instant::now(),
                    )
                } else {
                    schedule.next_wake(&eligible, &active, tokio::time::Instant::now())
                };
                tokio::select! {
                    finished = attempts.next(), if !attempts.is_empty() => {
                        if let Some((key, result)) = finished {
                            active.retain(|k| k != &key);
                            watching.retain(|k| k != &key);
                            cancellations.retain(|(k,_)| k != &key);
                            match result {
                                Ok(Completion::Drain { result, mut watch, retry_watch: delay }) => {
                                    let follow_up = dirty.contains(&key);
                                    dirty.retain(|k| k != &key);
                                    if follow_up { watch = None; }
                                    schedule.completed(key.clone(), result, tokio::time::Instant::now());
                                    if follow_up { schedule.due.retain(|(k,_)| k != &key); }
                                    if let Some(delay) = delay {
                                        retry_watch.retain(|(k,_)| k != &key);
                                        retry_watch.push((key.clone(), tokio::time::Instant::now()+delay));
                                    }
                                    if let Some((mut channel, tokens)) = watch {
                                        watching.push(key.clone());
                                        let (abort, registration) = futures_util::future::AbortHandle::new_pair();
                                        cancellations.push((key.clone(), abort));
                                        attempts.push(async move {
                                            let wait = async {
                                                let tokens = tokio::time::timeout(WATCH_REPAIR+Duration::from_secs(10), super::super::session::mailbox_watch::wait(&mut channel, &tokens)).await.ok()?.ok()?;
                                                Some((channel, tokens))
                                            };
                                            let result = futures_util::future::Abortable::new(wait, registration).await.map(Completion::Watch);
                                            (key, result)
                                        }.boxed());
                                    }
                                }
                                Ok(Completion::Watch(watch)) => {
                                    schedule.due.retain(|(k,_)| k != &key);
                                    if let Some(watch) = watch { ready.push((key, watch)); }
                                    else {
                                        retry_watch.retain(|(k,_)| k != &key);
                                        retry_watch.push((key.clone(), tokio::time::Instant::now()+Duration::from_secs(30)));
                                        schedule.completed(key, Some(false), tokio::time::Instant::now());
                                    }
                                }
                                Err(_) => {
                                    if dirty.contains(&key) {
                                        dirty.retain(|k| k != &key);
                                        schedule.due.retain(|(k,_)| k != &key);
                                    }
                                }
                            }
                        }
                    }
                    _ = tokio::time::sleep_until(wake) => {}
                    _ = self.post_office_receive_notify.notified() => break,
                    _ = source.changed() => {
                        // A source update invalidates quietness, not valid work.
                        // Keep actual drains alive and coalesce one follow-up.
                        for key in active.iter().chain(watching.iter()) {
                            if !dirty.contains(key) { dirty.push(key.clone()); }
                        }
                        for (key, abort) in &cancellations {
                            if watching.contains(key) { abort.abort(); }
                        }
                        ready.clear();
                        schedule.due.clear();
                    },
                    _ = network.changed() => break,
                    _ = roster.changed() => {}
                }
                if generation
                    != self
                        .privacy
                        .generation
                        .load(std::sync::atomic::Ordering::SeqCst)
                {
                    break;
                }
                candidates = self.post_office_drain_candidates();
                let current = candidates
                    .iter()
                    .map(|po| self.drain_key(po))
                    .collect::<Vec<_>>();
                for (key, abort) in &cancellations {
                    if !current.contains(key) {
                        abort.abort();
                    }
                }
                dirty.retain(|key| current.contains(key));
                ready.retain(|(key, _)| current.contains(key));
                retry_watch.retain(|(key, _)| current.contains(key));
                schedule.due.retain(|(key, _)| current.contains(key));
                if schedule
                    .primary
                    .as_ref()
                    .is_some_and(|key| !current.contains(key))
                {
                    schedule.primary = None;
                }
            }
            // All socket futures live in this owner. Dropping a partial encrypted
            // read also drops its channel; it can never re-enter session reuse.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
    };

    #[test]
    fn failed_relay_retains_its_deadline_while_modern_watches_park() {
        let root = tempfile::tempdir().unwrap();
        let peer = Announce::new_with_account(
            &DeviceIdentity::generate(),
            &Account::generate(),
            "Peer",
            9,
        );
        let (node, _, _) = super::super::super::test_support::node_with_files(
            root.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &peer,
        );
        let relay = DeviceIdentity::generate();
        let proof = Announce::new_post_office(&relay, "Relay", 10);
        node.roster
            .lock()
            .unwrap()
            .update(&proof, "127.0.0.1".parse().unwrap(), &node.user_id());
        let candidates = node.post_office_drain_candidates();
        assert_eq!(candidates.len(), 1);
        let now = tokio::time::Instant::now();
        let key = node.drain_key(&candidates[0]);
        assert_eq!(
            node.parked_receive_deadline(&candidates, &[], &[key], now),
            now + WATCH_REPAIR
        );
        node.record_relay_attempt(&relay.public(), false);
        assert!(!node.relay_retry_due(&relay.public()));
        let due = node.parked_receive_deadline(&candidates, &[], &[], now);
        assert!(
            due > now + Duration::from_secs(9) && due <= now + Duration::from_secs(11),
            "failed relay must wake at its ten-second retry, not five-minute quiet repair"
        );
    }
}
