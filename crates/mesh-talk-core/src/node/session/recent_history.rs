//! Negotiated recent/older message pages; no page certifies global equality.
use super::*;
use crate::eventlog::sync::network::{RecentPage, RecentStatus};
use sha2::{Digest, Sha256};
const PAGE_BYTES: usize = 60 * 1024;
fn marker() -> [u8; 32] {
    Sha256::digest(b"mesh-talk-recent-history-v1-capability").into()
}
pub(in crate::node) struct Service<V> {
    view: Option<(ConversationId, V)>,
    completed: bool,
}
impl<V> Default for Service<V> {
    fn default() -> Self {
        Self {
            view: None,
            completed: false,
        }
    }
}
impl<V> Service<V> {
    pub(in crate::node) fn retire<S: NetworkSyncStore<View = V>>(&mut self, store: &Mutex<S>) {
        self.completed = false;
        if let Some((_, view)) = self.view.take() {
            drop(store.lock().expect("store lock").retain_range_view(view));
        }
    }
}
#[derive(Serialize, Deserialize)]
struct Request {
    scope: ConversationId,
    cursor: Option<Vec<u8>>,
}
#[derive(Serialize, Deserialize)]
struct Response {
    scope: ConversationId,
    page: RecentPage,
}
fn parse<T: serde::de::DeserializeOwned>(bytes: &[u8], limit: u64) -> Result<T, SessionError> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(limit)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .map_err(|e| SessionError::Serialization(e.to_string()))
}
pub(in crate::node) async fn serve<S: NetworkSyncStore, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    state: &mut Service<S::View>,
    bytes: &[u8],
) -> Result<Option<Served>, SessionError> {
    let probe = match decode(bytes) {
        Ok(SyncWire::FpRequest(r)) if r.fingerprint == marker() => Some(r.conversation),
        _ => None,
    };
    if probe.is_none() && !bytes.starts_with(b"MHQ1") {
        return Ok(None);
    }
    if let Some(scope) = probe {
        if state.view.as_ref().is_some_and(|(c, _)| *c != scope) {
            state.retire(store);
        }
        if state.view.is_none() {
            let mut source = store.lock().expect("store lock");
            source.bind_session_peer(channel.peer_identity());
            let view = match source.take_custody_view(&scope)? {
                Some(v) => v,
                None => source.open_view(&scope)?,
            };
            state.view = Some((scope, view));
        }
        channel
            .send(&[b"MHA1".as_slice(), scope.as_bytes()].concat())
            .await?;
        return Ok(Some(Served::Handled(scope)));
    }
    if bytes.len() > 1024 {
        return Err(SessionError::UnexpectedMessage);
    }
    let request: Request = parse(&bytes[4..], 1024)?;
    if request.cursor.as_ref().is_some_and(|c| c.len() > 512) {
        return Err(SessionError::UnexpectedMessage);
    }
    let refresh = if request.cursor.is_none() && state.completed {
        match &state.view {
            Some((_, view)) => !store.lock().expect("store lock").reusable_view(view)?,
            None => false,
        }
    } else {
        false
    };
    if refresh {
        state.retire(store);
        let mut source = store.lock().expect("store lock");
        source.bind_session_peer(channel.peer_identity());
        let view = match source.take_custody_view(&request.scope)? {
            Some(v) => v,
            None => source.open_view(&request.scope)?,
        };
        state.view = Some((request.scope, view));
    }
    let (scope, view) = state.view.as_ref().ok_or(SessionError::UnexpectedMessage)?;
    if request.scope != *scope {
        return Err(SessionError::UnexpectedMessage);
    }
    let scope = *scope;
    let page = {
        let source = store.lock().expect("store lock");
        match source.recent_page(view, request.cursor.as_deref(), 200, PAGE_BYTES) {
            Ok(page) => page,
            Err(crate::eventlog::LogError::StaleCursor) => RecentPage::status(RecentStatus::Reset),
            Err(e) => return Err(e.into()),
        }
    };
    let reset = page.status == RecentStatus::Reset;
    let preparing = page.status == RecentStatus::More;
    state.completed = matches!(page.status, RecentStatus::Ready | RecentStatus::Blocked);
    let payload = bincode::serialize(&Response { scope, page })
        .map_err(|e| SessionError::Serialization(e.to_string()))?;
    if payload.len() + 4 > MAX_PLAINTEXT {
        return Err(SessionError::ResourceUnavailable);
    }
    channel
        .send(&[b"MHP1".as_slice(), &payload].concat())
        .await?;
    if reset {
        state.retire(store);
    } else if preparing {
        // Share eligible partial work with the existing recovery owner. If an
        // idle-cache budget declines it, keep the active admitted view so large
        // groups can still finish instead of restarting at the cache ceiling.
        if let Some((scope, view)) = state.view.take()
            && let Some(view) = store.lock().expect("store lock").retain_range_view(view)?
        {
            state.view = Some((scope, view));
        }
    }
    Ok(Some(Served::Handled(scope)))
}
pub(in crate::node) struct PageResult {
    pub status: RecentStatus,
    pub next: Option<Vec<u8>>,
    pub report: ApplyReport,
}
/// One capability probe and one bounded page. Caller decides whether to ask for
/// older history; this never recursively drains the returned continuation.
pub(in crate::node) async fn request<S: NetworkSyncStore, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    scope: ConversationId,
    cursor: Option<&[u8]>,
) -> Result<PageResult, SessionError> {
    if cursor.is_some_and(|c| c.len() > 512) {
        return Err(SessionError::UnexpectedMessage);
    }
    store
        .lock()
        .expect("store lock")
        .bind_session_peer(channel.peer_identity());
    channel
        .send(&encode(&SyncWire::FpRequest(FpRequest {
            conversation: scope,
            fingerprint: marker(),
        }))?)
        .await?;
    let reply = channel.recv().await?;
    if matches!(decode(&reply), Ok(SyncWire::FpResponse(_)))
        || matches!(decode(&reply),Ok(SyncWire::UpgradeRequired{conversation}) if conversation==scope)
    {
        return Ok(PageResult {
            status: RecentStatus::Unsupported,
            next: None,
            report: ApplyReport::default(),
        });
    }
    if reply != [b"MHA1".as_slice(), scope.as_bytes()].concat() {
        return Err(SessionError::UnexpectedMessage);
    }
    let payload = bincode::serialize(&Request {
        scope,
        cursor: cursor.map(<[u8]>::to_vec),
    })
    .map_err(|e| SessionError::Serialization(e.to_string()))?;
    channel
        .send(&[b"MHQ1".as_slice(), &payload].concat())
        .await?;
    let reply = channel.recv().await?;
    if !reply.starts_with(b"MHP1") || reply.len() > MAX_PLAINTEXT {
        return Err(SessionError::UnexpectedMessage);
    }
    let mut response: Response = parse(&reply[4..], MAX_PLAINTEXT as u64)?;
    validate(&response, scope, cursor)?;
    response.page.events = causal_page(response.page.events)?;
    let report = {
        let mut source = store.lock().expect("store lock");
        crate::eventlog::sync::network::apply_received_events(&response.page.events, |event| {
            source.ingest(event)
        })?
    };
    if !report.rejected.is_empty() {
        return Ok(PageResult {
            status: RecentStatus::Blocked,
            // This is navigation across an explicit unresolved gap, not a
            // durability acknowledgement for the rejected records.
            next: response.page.next,
            report,
        });
    }
    Ok(PageResult {
        status: response.page.status,
        next: response.page.next,
        report,
    })
}
/// Sort only the admitted page, never traverse its external ancestor closure.
/// Missing external parents remain explicit ingest rejections for bounded repair.
fn causal_page(
    events: Vec<crate::eventlog::Event>,
) -> Result<Vec<crate::eventlog::Event>, SessionError> {
    let ids: std::collections::HashMap<_, _> =
        events.iter().enumerate().map(|(i, e)| (e.id, i)).collect();
    let mut waiting = vec![0usize; events.len()];
    let mut children = vec![Vec::new(); events.len()];
    for (i, event) in events.iter().enumerate() {
        for parent in &event.parents {
            if let Some(&p) = ids.get(parent) {
                waiting[i] += 1;
                children[p].push(i);
            }
        }
    }
    let mut ready: std::collections::VecDeque<_> = waiting
        .iter()
        .enumerate()
        .filter_map(|(i, n)| (*n == 0).then_some(i))
        .collect();
    let mut sorted = Vec::with_capacity(events.len());
    while let Some(i) = ready.pop_front() {
        sorted.push(events[i].clone());
        for &child in &children[i] {
            waiting[child] -= 1;
            if waiting[child] == 0 {
                ready.push_back(child);
            }
        }
    }
    if sorted.len() != events.len() {
        return Err(SessionError::UnexpectedMessage);
    }
    Ok(sorted)
}

fn validate(
    response: &Response,
    scope: ConversationId,
    cursor: Option<&[u8]>,
) -> Result<(), SessionError> {
    let page = &response.page;
    if response.scope != scope
        || page.events.len() > 200
        || page.examined > 1000
        || page.events.len() > page.examined
        || page
            .next
            .as_ref()
            .is_some_and(|c| c.len() > 512 || c.is_empty())
    {
        return Err(SessionError::UnexpectedMessage);
    }
    if page.status != RecentStatus::Ready
        && !(page.status == RecentStatus::Blocked && page.events.is_empty())
        && (!page.events.is_empty() || page.next.is_some())
    {
        return Err(SessionError::UnexpectedMessage);
    }
    if let (Some(before), Some(next)) = (cursor, page.next.as_deref())
        && before == next
    {
        return Err(SessionError::UnexpectedMessage);
    }
    if page.next.is_some() && page.examined == 0 {
        return Err(SessionError::UnexpectedMessage);
    }
    let mut seen = std::collections::HashSet::new();
    let mut bytes = 0u64;
    for event in &page.events {
        if event.conversation_id != scope
            || event.kind != EventKind::Message
            || !seen.insert(event.id)
        {
            return Err(SessionError::UnexpectedMessage);
        }
        bytes = bytes
            .checked_add(
                bincode::serialized_size(event)
                    .map_err(|e| SessionError::Serialization(e.to_string()))?,
            )
            .ok_or(SessionError::ResourceUnavailable)?;
    }
    if bytes > PAGE_BYTES as u64 {
        return Err(SessionError::ResourceUnavailable);
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    fn event() -> crate::eventlog::Event {
        crate::eventlog::Event::new(
            &crate::identity::device::DeviceIdentity::generate(),
            ConversationId::new([7; 32]),
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![1; 32],
        )
    }
    #[test]
    fn recent_page_rejects_duplicate_scope_and_nonadvancing_cursor() {
        let event = event();
        let scope = event.conversation_id;
        let mut r = Response {
            scope,
            page: RecentPage {
                status: RecentStatus::Ready,
                events: vec![event.clone(), event],
                next: Some(vec![2]),
                examined: 2,
            },
        };
        assert!(validate(&r, scope, None).is_err());
        r.page.events.pop();
        assert!(validate(&r, scope, Some(&[2])).is_err());
        assert!(validate(&r, ConversationId::new([8; 32]), None).is_err());
        assert!(validate(&r, scope, None).is_ok());
        r.page.status = RecentStatus::More;
        assert!(validate(&r, scope, None).is_err());
    }
}

#[cfg(test)]
mod production_tests {
    use super::*;
    use crate::{
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
    };
    use std::sync::Arc;
    #[tokio::test]
    async fn recent_production_pages_stop_before_full_history_and_backfill_pinned_snapshot() {
        exercise_recent_pages(false).await;
    }
    #[tokio::test]
    async fn recent_preparation_finishes_when_idle_cache_declines_the_active_view() {
        exercise_recent_pages(true).await;
    }
    async fn exercise_recent_pages(reject_idle: bool) {
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let li = DeviceIdentity::generate();
        let la = Account::generate();
        let ri = DeviceIdentity::generate();
        let ra = Account::generate();
        let lp = Announce::new_with_account(&li, &la, "Left", 9);
        let rp = Announce::new_with_account(&ri, &ra, "Right", 9);
        let (left, _, _) = crate::node::test_support::node_with_files(dirs[0].path(), li, la, &rp);
        let (right, _, _) = crate::node::test_support::node_with_files(dirs[1].path(), ri, ra, &lp);
        left.cached_peer_snapshot_async().await.unwrap();
        right.cached_peer_snapshot_async().await.unwrap();
        let group = left
            .create_channel("Recent production", vec![right.identity.public()])
            .await
            .unwrap();
        left.await_channel_metadata(group).await.unwrap();
        for event in left.log.lock().unwrap().events(&group).unwrap() {
            right.log.lock().unwrap().append_durable(event).unwrap();
        }
        crate::node::test_support::settle_group_metadata(&right, group);
        let mut expected = Vec::new();
        for _ in 0..201 {
            let (_, id) = left
                .append_event_with_profile_parent(group, EventKind::Message, vec![1], None)
                .unwrap();
            expected.push(id);
        }
        // Oversized-for-this-page data must not strand older fitting messages.
        {
            let source = left.serve_sync_store(&right.identity.public());
            let mut source = source.lock().unwrap();
            source.bind_session_peer(&right.identity.public());
            let view = source.open_view(&group).unwrap();
            let mut blocked = None;
            for _ in 0..512 {
                let page = source.recent_page(&view, None, 200, 1).unwrap();
                if page.status == RecentStatus::Blocked {
                    blocked = Some(page);
                    break;
                }
                assert_eq!(page.status, RecentStatus::More);
            }
            let blocked = blocked.expect("finite source preparation must finish");
            assert!(blocked.events.is_empty());
            let gap = blocked
                .next
                .expect("an oversized first record must expose explicit older navigation");
            let older = source
                .recent_page(&view, Some(&gap), 200, PAGE_BYTES)
                .unwrap();
            assert_eq!(older.status, RecentStatus::Ready);
            assert!(!older.events.is_empty());
        }
        let _cache_policy =
            reject_idle.then(crate::node::privacy_sync::prepared_views::RejectIdleCache::enter);
        let (a, b) = tokio::io::duplex(256 * 1024);
        let server = Arc::clone(&left);
        let task = tokio::spawn(async move {
            let mut ch = SecureChannel::accept(b, &server.identity).await.unwrap();
            let store = server.serve_sync_store(ch.peer_identity());
            let mut state = Service::default();
            while let Ok(bytes) = ch.recv().await {
                assert!(
                    serve(&mut ch, &store, &mut state, &bytes)
                        .await
                        .unwrap()
                        .is_some()
                );
            }
            state.retire(&store);
        });
        let mut ch = SecureChannel::connect(a, &right.identity, Some(&left.identity.public()))
            .await
            .unwrap();
        let store = right.pull_sync_store(ch.peer_identity());
        let mut first = None;
        for _ in 0..512 {
            let result = request(&mut ch, &store, group, None).await.unwrap();
            if matches!(result.status, RecentStatus::Ready | RecentStatus::Blocked) {
                first = Some(result);
                break;
            }
            assert_eq!(result.status, RecentStatus::More);
        }
        let first = first.expect("bounded preparation must finish");
        assert_eq!(
            first.status,
            RecentStatus::Blocked,
            "missing external parents must not be acknowledged as a committed recent page"
        );
        assert_eq!(first.report.applied, 0);
        let mut cursor = first
            .next
            .expect("201 records require an explicit older page");
        let present = expected
            .iter()
            .filter(|id| right.log.lock().unwrap().get(id).unwrap().is_some())
            .count();
        assert!(present <= 200, "first page must not drain all history");
        let (_, later) = left
            .append_event_with_profile_parent(group, EventKind::Message, vec![2], None)
            .unwrap();
        let mut exhausted = false;
        for _ in 0..32 {
            let result = request(&mut ch, &store, group, Some(&cursor))
                .await
                .unwrap();
            assert_eq!(result.status, RecentStatus::Ready);
            match result.next {
                Some(next) => cursor = next,
                None => {
                    exhausted = true;
                    break;
                }
            }
        }
        assert!(exhausted);
        assert!(
            right.log.lock().unwrap().get(&later).unwrap().is_none(),
            "older pages must retain the original cutoff"
        );
        assert!(
            expected
                .iter()
                .any(|id| right.log.lock().unwrap().get(id).unwrap().is_some())
        );
        // The existing dependency lane may supply the one older external parent
        // needed by the now-shifted recent window. It uses the same guarded ingest.
        let parent = left
            .log
            .lock()
            .unwrap()
            .get(&expected[1])
            .unwrap()
            .unwrap()
            .clone();
        NetworkSyncStore::ingest(&mut *store.lock().unwrap(), parent).unwrap();
        let mut refreshed = false;
        for _ in 0..512 {
            let result = request(&mut ch, &store, group, None).await.unwrap();
            if result.status == RecentStatus::Ready {
                refreshed = true;
                break;
            }
            assert_eq!(result.status, RecentStatus::More);
        }
        assert!(refreshed);
        for id in expected {
            assert!(
                right.log.lock().unwrap().get(&id).unwrap().is_some(),
                "retry after explicit dependency repair must commit the recent chain"
            );
        }
        assert!(
            right.log.lock().unwrap().get(&later).unwrap().is_some(),
            "fresh recent probes must see appended messages on pooled sessions"
        );
        drop(ch);
        task.await.unwrap();
    }
}
