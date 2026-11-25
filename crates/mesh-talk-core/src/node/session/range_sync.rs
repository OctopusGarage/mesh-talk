//! Negotiated bounded range service. Old peers see an ordinary unmatched probe.
use super::*;
use crate::eventlog::sync::{
    network::{ExportPageBudget, PreparationBudget, RequestedExportStatus},
    range::{
        RangeProgress, RangeReceiverProgress,
        index::{Checkpoint, SummaryFrame},
    },
};
use sha2::{Digest, Sha256};
use std::collections::HashSet;

fn marker() -> [u8; 32] {
    Sha256::digest(b"mesh-talk-range-profile-v2-capability").into()
}
fn probe(bytes: &[u8]) -> Option<ConversationId> {
    match decode(bytes).ok()? {
        SyncWire::FpRequest(r)
        | SyncWire::FpRequestV2(r)
        | SyncWire::FpRequestV3(r)
        | SyncWire::FpRequestV4(r)
        | SyncWire::FpRequestV5(r)
            if r.fingerprint == marker()
                || r.fingerprint
                    == <[u8; 32]>::from(Sha256::digest(
                        b"mesh-talk-range-profile-v1-capability",
                    )) =>
        {
            Some(r.conversation)
        }
        _ => None,
    }
}
pub(in crate::node) struct Service<V> {
    view: Option<(ConversationId, V)>,
    token: Option<[u8; 32]>,
    sent: SyncTurnBudget,
    requests: usize,
    continuing: bool,
    ordered: bool,
}
impl<V> Default for Service<V> {
    fn default() -> Self {
        Self {
            view: None,
            token: None,
            sent: SyncTurnBudget {
                records: 100,
                body_bytes: 512 * 1024,
            },
            requests: 0,
            continuing: true,
            ordered: true,
        }
    }
}
impl<V> Service<V> {
    pub(in crate::node) fn retire<S: NetworkSyncStore<View = V>>(&mut self, store: &Mutex<S>) {
        if let Some((_, view)) = self.view.take() {
            drop(store.lock().expect("store lock").retain_range_view(view));
        }
    }
}
#[derive(serde::Serialize, serde::Deserialize)]
struct Records {
    scope: ConversationId,
    token: [u8; 32],
    status: u8,
    events: Vec<crate::eventlog::Event>,
    unavailable: Vec<EventId>,
    consumed: usize,
}
fn scope(bytes: &[u8]) -> Option<ConversationId> {
    bytes
        .get(4..36)
        .and_then(|s| s.try_into().ok())
        .map(ConversationId::new)
}

pub(in crate::node) async fn serve<S: NetworkSyncStore, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    state: &mut Service<S::View>,
    bytes: &[u8],
) -> Result<Option<Served>, SessionError> {
    if let Ok(SyncWire::DurableHaveRequest {
        conversation,
        event_id,
    }) = decode(bytes)
        && let Some((scope, view)) = &state.view
        && *scope == conversation
    {
        let stored = {
            let source = store.lock().expect("store lock");
            // A token describes an earlier range root, not readiness or current
            // exact custody. Resumed preparation and later appends must use the
            // independent prepared responder without retiring this continuation.
            if !source.reusable_view(view)? {
                return Ok(None);
            }
            let stored = source.durable_have_in_view(view, &conversation, &event_id)?;
            source.validate_view(view)?;
            stored
        };
        channel
            .send(&encode(&SyncWire::DurableHaveResponse {
                conversation,
                event_id,
                stored,
            })?)
            .await?;
        return Ok(Some(Served::Handled(conversation)));
    }
    let open = probe(bytes);
    if open.is_some() {
        state.ordered =
            matches!(decode(bytes), Ok(SyncWire::FpRequest(r)) if r.fingerprint == marker());
    }
    let range = bytes.starts_with(b"MRT1")
        || bytes.starts_with(b"MRB1")
        || bytes.starts_with(b"MRGQ")
        || bytes.starts_with(b"MRD1")
        || bytes.starts_with(b"MRV1")
        || bytes.starts_with(b"MRF1");
    if open.is_none() && !range {
        return Ok(None);
    }
    let conversation = open
        .or_else(|| scope(bytes))
        .ok_or(SessionError::UnexpectedMessage)?;
    // Each probe opens a freshly requalified finite snapshot. Qualified cached
    // preparation can resume, but a previous completed snapshot is never reused
    // blindly on a pooled connection after appends or policy changes.
    let keep_building = open.is_some()
        && state.continuing
        && state.view.as_ref().is_some_and(|(scope, view)| {
            *scope == conversation
                && store
                    .lock()
                    .expect("store lock")
                    .validate_view(view)
                    .is_ok()
        });
    if open.is_some() && !keep_building {
        state.retire(store);
        let mut source = store.lock().expect("store lock");
        source.bind_session_peer(channel.peer_identity());
        let view = match source.take_custody_view(&conversation)? {
            Some(view) => view,
            None => source.open_view(&conversation)?,
        };
        state.view = Some((conversation, view));
        state.token = None;
        state.sent = SyncTurnBudget {
            records: 100,
            body_bytes: 512 * 1024,
        };
        state.requests = 0;
        state.continuing = true;
    }
    if open.is_some() {
        state.requests = 0;
        state.sent = SyncTurnBudget {
            records: 100,
            body_bytes: 512 * 1024,
        };
    }
    let (bound, view) = state.view.as_ref().ok_or(SessionError::UnexpectedMessage)?;
    if *bound != conversation {
        return Err(SessionError::UnexpectedMessage);
    }
    state.requests += 1;
    if state.requests > 5 {
        return Err(SessionError::ResourceUnavailable);
    }
    if bytes.starts_with(b"MRF1") {
        if bytes.len() != 37 || bytes[36] > 2 {
            return Err(SessionError::UnexpectedMessage);
        }
        state.continuing = bytes[36] != 1;
        channel
            .send(&[b"MRA1".as_slice(), conversation.as_bytes()].concat())
            .await?;
        return Ok(Some(Served::Handled(conversation)));
    }
    if bytes.starts_with(b"MRV1") {
        if bytes.len() != 77 || bytes[76] > 2 {
            return Err(SessionError::UnexpectedMessage);
        }
        let remote_token: [u8; 32] = bytes[36..68].try_into().unwrap();
        let remote_revision = u64::from_le_bytes(bytes[68..76].try_into().unwrap());
        let result = request_ready(
            channel,
            store,
            conversation,
            view,
            remote_token,
            remote_revision,
            state.ordered,
            2,
            100,
            512 * 1024,
        )
        .await?;
        let status = match result {
            Turn::Complete(_) => 1,
            Turn::Limited(_) => 2,
            _ => 0,
        };
        state.continuing = !(status == 1 && bytes[76] == 1);
        channel
            .send(&[b"MRE1".as_slice(), conversation.as_bytes(), &[status]].concat())
            .await?;
        return Ok(Some(Served::Handled(conversation)));
    }
    let response = {
        let source = store.lock().expect("store lock");
        source.validate_view(view)?;
        let response = if open.is_some() || bytes.starts_with(b"MRB1") {
            match source.range_prepare(view) {
                Err(error) if crate::eventlog::sync::range::is_index_capacity_error(&error) => {
                    [b"MRL1".as_slice(), conversation.as_bytes()].concat()
                }
                Err(error) => return Err(error.into()),
                Ok(progress) => match progress {
                    RangeProgress::Unsupported => {
                        encode(&SyncWire::FpResponse(FpResponse { matched: false }))?
                    }
                    RangeProgress::More => [b"MRP1".as_slice(), conversation.as_bytes()].concat(),
                    RangeProgress::Ready {
                        token,
                        revision,
                        count,
                        digest,
                    } => {
                        state.token = Some(token);
                        [
                            b"MRR1".as_slice(),
                            conversation.as_bytes(),
                            &token,
                            &revision.to_le_bytes(),
                            &count.to_le_bytes(),
                            &digest,
                        ]
                        .concat()
                    }
                },
            }
        } else if bytes.starts_with(b"MRT1") {
            if !state.ordered || bytes.len() < 83 {
                return Err(SessionError::UnexpectedMessage);
            }
            let token: [u8; 32] = bytes[36..68].try_into().unwrap();
            let after = i64::from_le_bytes(bytes[68..76].try_into().unwrap());
            let count = u16::from_le_bytes(bytes[76..78].try_into().unwrap()) as usize;
            let limit = u32::from_le_bytes(bytes[78..82].try_into().unwrap()) as usize;
            if state.token != Some(token)
                || after < -1
                || count == 0
                || count > 100
                || count > state.sent.records
                || limit == 0
                || limit > 57 * 1024
                || limit > state.sent.body_bytes
            {
                return Err(SessionError::UnexpectedMessage);
            }
            let anchor_count = bytes[82] as usize;
            if anchor_count > 32 || bytes.len() != 83 + anchor_count * 32 {
                return Err(SessionError::UnexpectedMessage);
            }
            let anchors: Vec<_> = bytes[83..]
                .as_chunks::<32>()
                .0
                .iter()
                .map(|b| EventId::new(*b))
                .collect();
            let (events, next, complete) = source
                .ordered_repair_page(
                    view,
                    after,
                    &anchors,
                    ExportPageBudget {
                        examined: 1000,
                        events: count,
                        encoded_bytes: limit,
                        allocated_bytes: limit.saturating_mul(4),
                    },
                )?
                .ok_or(SessionError::ResourceUnavailable)?;
            state.sent.charge_events(&events)?;
            let payload = bincode::serialize(&OrderedRecords {
                scope: conversation,
                token,
                after,
                next,
                complete,
                events,
            })
            .map_err(|e| SessionError::Serialization(e.to_string()))?;
            if payload.len() + 4 > MAX_PLAINTEXT {
                return Err(SessionError::ResourceUnavailable);
            }
            [b"MRU1".as_slice(), &payload].concat()
        } else if bytes.starts_with(b"MRGQ") {
            source.range_summary(view, bytes)?
        } else {
            if bytes.len() < 74 {
                return Err(SessionError::UnexpectedMessage);
            }
            let token: [u8; 32] = bytes[36..68].try_into().unwrap();
            let count = usize::from(u16::from_le_bytes(bytes[68..70].try_into().unwrap()));
            let budget = u32::from_le_bytes(bytes[70..74].try_into().unwrap()) as usize;
            if state.token != Some(token)
                || count == 0
                || count > 100
                || budget == 0
                || budget > 1024 * 1024
                || bytes.len() != 74 + count * 32
            {
                return Err(SessionError::UnexpectedMessage);
            }
            let targets: Vec<_> = bytes[74..]
                .as_chunks::<32>()
                .0
                .iter()
                .map(|b| EventId::new(*b))
                .collect();
            let budget = budget
                .min(MAX_PLAINTEXT.saturating_sub(1024 + count * 32))
                .min(state.sent.body_bytes);
            if count > state.sent.records || budget == 0 {
                return Err(SessionError::ResourceUnavailable);
            }
            let page = source.requested_export(
                view,
                &targets,
                ExportPageBudget {
                    examined: 2000,
                    events: count,
                    encoded_bytes: budget,
                    allocated_bytes: budget,
                },
                PreparationBudget::default(),
            )?;
            state.sent.charge_events(&page.events)?;
            let status = match page.status {
                RequestedExportStatus::Unsupported => 0,
                RequestedExportStatus::Preparing => 1,
                RequestedExportStatus::More => 2,
                RequestedExportStatus::Complete => 3,
            };
            let payload = bincode::serialize(&Records {
                scope: conversation,
                token,
                status,
                events: page.events,
                unavailable: page.unavailable,
                consumed: page.consumed,
            })
            .map_err(|e| SessionError::Serialization(e.to_string()))?;
            if payload.len() + 4 > MAX_PLAINTEXT {
                return Err(SessionError::ResourceUnavailable);
            }
            [b"MRS1".as_slice(), &payload].concat()
        };
        source.validate_view(view)?;
        response
    };
    channel.send(&response).await?;
    Ok(Some(Served::Handled(conversation)))
}

pub(in crate::node) enum Turn {
    Unsupported,
    More(ApplyReport),
    Limited(ApplyReport),
    Complete(ApplyReport),
}
/// One bounded client turn; local preparations and checkpoints stay with the
/// qualified store view. The caller retains/recycles the authenticated session.
pub(in crate::node) async fn request<S: NetworkSyncStore, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
) -> Result<Turn, SessionError> {
    store
        .lock()
        .expect("store lock")
        .bind_session_peer(channel.peer_identity());
    channel
        .send(&encode(&SyncWire::FpRequest(FpRequest {
            conversation,
            fingerprint: marker(),
        }))?)
        .await?;
    let reply = channel.recv().await?;
    if matches!(
        decode(&reply),
        Ok(SyncWire::FpResponse(FpResponse { matched: false }))
    ) || matches!(decode(&reply), Ok(SyncWire::UpgradeRequired { conversation: scope }) if scope==conversation)
    {
        return Ok(Turn::Unsupported);
    }
    if reply.len() == 36 && reply.starts_with(b"MRL1") && scope(&reply) == Some(conversation) {
        return Ok(Turn::Limited(ApplyReport::default()));
    }
    if reply.len() == 36 && reply.starts_with(b"MRP1") && scope(&reply) == Some(conversation) {
        return Ok(Turn::More(ApplyReport::default()));
    }
    if reply.len() != 116 || !reply.starts_with(b"MRR1") || scope(&reply) != Some(conversation) {
        return Err(SessionError::UnexpectedMessage);
    }
    let token: [u8; 32] = reply[36..68].try_into().unwrap();
    let revision = u64::from_le_bytes(reply[68..76].try_into().unwrap());
    let view = {
        let mut source = store.lock().expect("store lock");
        match source.take_custody_view(&conversation)? {
            Some(view) => view,
            None => source.open_view(&conversation)?,
        }
    };
    let forward = request_ready(
        channel,
        store,
        conversation,
        &view,
        token,
        revision,
        true,
        2,
        100,
        512 * 1024,
    )
    .await;
    let ordered_active = {
        let source = store.lock().expect("store lock");
        match source.range_checkpoint(&view) {
            Ok(checkpoint) => {
                checkpoint.is_some_and(|checkpoint| checkpoint.ordered_cursor().is_some())
            }
            Err(crate::eventlog::LogError::StaleCursor) if matches!(&forward, Ok(Turn::More(report)) if report.applied > 0) =>
            {
                // Accepted controls can invalidate our own captured authority.
                // Discard that derived view and requalify next turn; its
                // accepted reception work must not become a socket failure.
                drop(source);
                drop(store.lock().expect("store lock").retain_range_view(view));
                return forward;
            }
            Err(error) => return Err(error.into()),
        }
    };
    drop(store.lock().expect("store lock").retain_range_view(view));
    let forward = forward?;
    let reverse_required = store.lock().expect("store lock").range_reverse_required();
    if !reverse_required || ordered_active {
        let status = match &forward {
            Turn::Complete(_) => 1,
            Turn::Limited(_) => 2,
            _ => 0,
        };
        channel
            .send(&[b"MRF1".as_slice(), conversation.as_bytes(), &[status]].concat())
            .await?;
        let ack = channel.recv().await?;
        if ack != [b"MRA1".as_slice(), conversation.as_bytes()].concat() {
            return Err(SessionError::UnexpectedMessage);
        }
        return Ok(forward);
    }
    let mut service = Service::default();
    let (source_view, ready) = {
        let mut source = store.lock().expect("store lock");
        let source_view = match source.take_custody_view(&conversation)? {
            Some(view) => view,
            None => source.open_view(&conversation)?,
        };
        let ready = source.range_prepare(&source_view)?;
        (source_view, ready)
    };
    service.view = Some((conversation, source_view));
    let (source_token, source_revision) = match ready {
        RangeProgress::Ready {
            token, revision, ..
        } => (token, revision),
        _ => {
            service.retire(store);
            let status = match &forward {
                Turn::Complete(_) => 1,
                Turn::Limited(_) => 2,
                _ => 0,
            };
            channel
                .send(&[b"MRF1".as_slice(), conversation.as_bytes(), &[status]].concat())
                .await?;
            if channel.recv().await? != [b"MRA1".as_slice(), conversation.as_bytes()].concat() {
                return Err(SessionError::UnexpectedMessage);
            }
            return Ok(match (ready, forward) {
                (RangeProgress::Unsupported, Turn::Complete(report) | Turn::More(report)) => {
                    Turn::Limited(report)
                }
                (_, Turn::Complete(report) | Turn::More(report)) => Turn::More(report),
                (_, Turn::Limited(report)) => Turn::Limited(report),
                (_, Turn::Unsupported) => Turn::Unsupported,
            });
        }
    };
    service.token = Some(source_token);
    channel
        .send(
            &[
                b"MRV1".as_slice(),
                conversation.as_bytes(),
                &source_token,
                &source_revision.to_le_bytes(),
                &[match &forward {
                    Turn::Complete(_) => 1,
                    Turn::Limited(_) => 2,
                    _ => 0,
                }],
            ]
            .concat(),
        )
        .await?;
    let reverse = async {
        // Two summary queries and one body request at most; terminal frame
        // confirms only completion, never message custody or read state.
        for _ in 0..4 {
            let bytes = channel.recv().await?;
            if bytes.len() == 37
                && bytes.starts_with(b"MRE1")
                && scope(&bytes) == Some(conversation)
            {
                if bytes[36] > 2 {
                    return Err(SessionError::UnexpectedMessage);
                }
                return Ok::<u8, SessionError>(bytes[36]);
            }
            if bytes.starts_with(b"MRV1") || probe(&bytes).is_some() {
                return Err(SessionError::UnexpectedMessage);
            }
            if serve(channel, store, &mut service, &bytes).await?.is_none() {
                return Err(SessionError::UnexpectedMessage);
            }
        }
        Err(SessionError::ResourceUnavailable)
    }
    .await;
    service.retire(store);
    let reverse = reverse?;
    let result = match (forward, reverse) {
        (Turn::Limited(report), _) | (Turn::Complete(report) | Turn::More(report), 2) => {
            Turn::Limited(report)
        }
        (Turn::Complete(report), 1) => Turn::Complete(report),
        (Turn::Complete(report) | Turn::More(report), _) => Turn::More(report),
        (Turn::Unsupported, _) => Turn::Unsupported,
    };
    Ok(result)
}
async fn request_ready<S: NetworkSyncStore, IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<S>,
    conversation: ConversationId,
    view: &S::View,
    token: [u8; 32],
    revision: u64,
    allow_ordered: bool,
    summary_rpcs: usize,
    record_limit: usize,
    byte_limit: usize,
) -> Result<Turn, SessionError> {
    let snapshot = {
        let source = store.lock().expect("store lock");
        match source.range_receiver_prepare(view)? {
            RangeReceiverProgress::Ready(s) => s,
            RangeReceiverProgress::More => return Ok(Turn::More(ApplyReport::default())),
            RangeReceiverProgress::Unsupported => return Err(SessionError::ResourceUnavailable),
        }
    };
    let mut checkpoint = store
        .lock()
        .expect("store lock")
        .range_checkpoint(view)?
        .map(Ok)
        .unwrap_or_else(|| {
            Checkpoint::for_reader(
                *conversation.as_bytes(),
                snapshot.token(),
                snapshot.index(),
                revision,
            )
        })?;
    if !checkpoint.valid_reader_binding(
        snapshot.index(),
        *conversation.as_bytes(),
        token,
        snapshot.token(),
        revision,
    )? {
        store.lock().expect("store lock").retain_range_checkpoint(
            view,
            Checkpoint::for_reader(
                *conversation.as_bytes(),
                snapshot.token(),
                snapshot.index(),
                revision,
            )?,
        )?;
        return Ok(Turn::More(ApplyReport::default()));
    }

    if let Some(after) = checkpoint.ordered_cursor() {
        if !allow_ordered {
            return Err(SessionError::ResourceUnavailable);
        }
        let limit = byte_limit.min(57 * 1024);
        channel
            .send(
                &[
                    b"MRT1".as_slice(),
                    conversation.as_bytes(),
                    &token,
                    &after.to_le_bytes(),
                    &(record_limit as u16).to_le_bytes(),
                    &(limit as u32).to_le_bytes(),
                    &[checkpoint.ordered_anchors().len() as u8],
                    checkpoint.ordered_anchors().as_flattened(),
                ]
                .concat(),
            )
            .await?;
        let reply = channel.recv().await?;
        if !reply.starts_with(b"MRU1") || reply.len() > MAX_PLAINTEXT {
            return Err(SessionError::UnexpectedMessage);
        }
        let response: OrderedRecords = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_PLAINTEXT as u64)
            .reject_trailing_bytes()
            .deserialize(&reply[4..])
            .map_err(|e| SessionError::Serialization(e.to_string()))?;
        validate_ordered_records(&response, conversation, token, after)?;
        SyncTurnBudget {
            records: record_limit,
            body_bytes: limit,
        }
        .charge_events(&response.events)?;
        let mut source = store.lock().expect("store lock");
        source.validate_view(view)?;
        let mut missing_parents = Vec::new();
        let report =
            crate::eventlog::sync::network::apply_received_events(&response.events, |event| {
                let result = source.ingest(event);
                if let Err(crate::eventlog::LogError::MissingParents(ids)) = &result {
                    missing_parents.extend(
                        ids.iter()
                            .take(32usize.saturating_sub(missing_parents.len()))
                            .map(|id| *id.as_bytes()),
                    );
                }
                result
            })?;
        if applied_view_invalidated(&*source, view, &report)? {
            return Ok(Turn::More(report));
        }
        if checkpoint.ordered_is_warm() && report.applied > 0 {
            checkpoint.acknowledge_ordered_page(response.next, true);
        } else if !missing_parents.is_empty() {
            checkpoint.begin_ordered_repair_with_anchors(&missing_parents);
        } else if !report.rejected.is_empty() {
            return Err(SessionError::ResourceUnavailable);
        } else {
            checkpoint.acknowledge_ordered_page(response.next, response.complete);
        }
        source.retain_range_checkpoint(view, checkpoint)?;
        return Ok(Turn::More(report));
    }
    let mut missing = Vec::new();
    // The probe and optional body fetch consume the other two turn RPCs.
    for _ in 0..summary_rpcs {
        let Some(prefix) = checkpoint.next_query() else {
            break;
        };
        store.lock().expect("store lock").validate_view(view)?;
        let request = [
            b"MRGQ".as_slice(),
            conversation.as_bytes(),
            &token,
            &prefix.bits().to_le_bytes(),
            &prefix.key(),
        ]
        .concat();
        channel.send(&request).await?;
        let reply = channel.recv().await?;
        store.lock().expect("store lock").validate_view(view)?;
        let frame = SummaryFrame::decode(&reply, *conversation.as_bytes(), token)
            .ok_or(SessionError::UnexpectedMessage)?;
        match checkpoint.accept_reader(
            snapshot.index(),
            *conversation.as_bytes(),
            token,
            snapshot.token(),
            revision,
            frame,
        )? {
            Ok(ids) => missing.extend(ids),
            Err(crate::eventlog::sync::range::index::RemoteError::Invalidated) => {
                store.lock().expect("store lock").retain_range_checkpoint(
                    view,
                    Checkpoint::for_reader(
                        *conversation.as_bytes(),
                        snapshot.token(),
                        snapshot.index(),
                        revision,
                    )?,
                )?;
                return Ok(Turn::More(ApplyReport::default()));
            }
            Err(_) => return Err(SessionError::UnexpectedMessage),
        }
    }

    let mut report = ApplyReport::default();
    if !missing.is_empty() {
        if missing.len() > record_limit {
            return Err(SessionError::ResourceUnavailable);
        }
        let mut request = [
            b"MRD1".as_slice(),
            conversation.as_bytes(),
            &token,
            &(missing.len() as u16).to_le_bytes(),
            &((MAX_PLAINTEXT.saturating_sub(8192).min(byte_limit)) as u32).to_le_bytes(),
        ]
        .concat();
        for id in &missing {
            request.extend_from_slice(id);
        }
        channel.send(&request).await?;
        let reply = channel.recv().await?;
        if !reply.starts_with(b"MRS1") || reply.len() > MAX_PLAINTEXT {
            return Err(SessionError::UnexpectedMessage);
        }
        let response: Records = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit((1024 * 1024 + 16 * 1024) as u64)
            .reject_trailing_bytes()
            .deserialize(&reply[4..])
            .map_err(|e| SessionError::Serialization(e.to_string()))?;
        let mut deferred = validate_records(&response, conversation, token, &missing)?;
        let mut budget = SyncTurnBudget {
            records: record_limit,
            body_bytes: byte_limit,
        };
        budget.charge_events(&response.events)?;
        let mut missing_parents = Vec::new();
        {
            let mut source = store.lock().expect("store lock");
            source.validate_view(view)?;
            report =
                crate::eventlog::sync::network::apply_received_events(&response.events, |event| {
                    let result = source.ingest(event);
                    if let Err(crate::eventlog::LogError::MissingParents(ids)) = &result {
                        missing_parents.extend(
                            ids.iter()
                                .take(32usize.saturating_sub(missing_parents.len()))
                                .map(|id| *id.as_bytes()),
                        );
                    }
                    result
                })?;
            if applied_view_invalidated(&*source, view, &report)? {
                return Ok(Turn::More(report));
            }
        }
        if !missing_parents.is_empty() && allow_ordered {
            checkpoint.begin_ordered_repair_with_anchors(&missing_parents);
            store
                .lock()
                .expect("store lock")
                .retain_range_checkpoint(view, checkpoint)?;
            return Ok(Turn::More(report));
        }
        let unavailable: Vec<_> = response
            .unavailable
            .iter()
            .map(|id| *id.as_bytes())
            .chain(report.rejected.iter().map(|(id, _)| *id.as_bytes()))
            .collect();
        for (id, _) in &report.rejected {
            deferred.push(*id.as_bytes());
        }
        checkpoint.mark_unavailable(&unavailable);
        checkpoint.defer_missing(&deferred);
    }
    store
        .lock()
        .expect("store lock")
        .retain_range_checkpoint(view, checkpoint.clone())?;
    if !checkpoint.has_pending() {
        Ok(Turn::Complete(report))
    } else if !checkpoint.has_unblocked_pending() {
        Ok(Turn::Limited(report))
    } else {
        Ok(Turn::More(report))
    }
}

/// An accepted batch can change principal controls. This is unfinished work,
/// never a completed comparison, and the old cursor must not be retained.
/// Staleness before admission or without any accepted record remains an error.
fn applied_view_invalidated<S: NetworkSyncStore>(
    source: &S,
    view: &S::View,
    report: &ApplyReport,
) -> Result<bool, SessionError> {
    if report.applied == 0 {
        return Ok(false);
    }
    match source.validate_view(view) {
        Ok(()) => Ok(false),
        Err(crate::eventlog::LogError::StaleCursor) => Ok(true),
        Err(error) => Err(error.into()),
    }
}

fn validate_records(
    response: &Records,
    conversation: ConversationId,
    token: [u8; 32],
    requested: &[[u8; 32]],
) -> Result<Vec<[u8; 32]>, SessionError> {
    if response.scope != conversation
        || response.token != token
        || response.events.len() > 200
        || response.consumed > requested.len()
    {
        return Err(SessionError::UnexpectedMessage);
    }
    if response.status == 0 {
        return Err(SessionError::ResourceUnavailable);
    }
    if response.status > 3 || (response.status == 3 && response.consumed != requested.len()) {
        return Err(SessionError::UnexpectedMessage);
    }
    let expected: HashSet<_> = requested[..response.consumed].iter().copied().collect();
    let mut seen = HashSet::new();
    for event in &response.events {
        if event.conversation_id != conversation
            || !expected.contains(event.id.as_bytes())
            || !seen.insert(*event.id.as_bytes())
        {
            return Err(SessionError::UnexpectedMessage);
        }
    }
    let mut deferred = requested[response.consumed..].to_vec();
    for id in &response.unavailable {
        if !expected.contains(id.as_bytes()) || !seen.insert(*id.as_bytes()) {
            return Err(SessionError::UnexpectedMessage);
        }
        deferred.push(*id.as_bytes());
    }
    if seen != expected {
        return Err(SessionError::UnexpectedMessage);
    }
    Ok(deferred)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn event(seq: u64) -> crate::eventlog::Event {
        crate::eventlog::Event::new(
            &crate::identity::device::DeviceIdentity::generate(),
            ConversationId::new([7; 32]),
            seq,
            vec![],
            seq,
            0,
            EventKind::Message,
            vec![1; 32],
        )
    }
    #[test]
    fn duplicate_records_cannot_acknowledge_missing_target() {
        let a = event(1);
        let b = event(2);
        let request = [*a.id.as_bytes(), *b.id.as_bytes()];
        let response = Records {
            scope: a.conversation_id,
            token: [1; 32],
            status: 3,
            events: vec![a.clone(), a.clone()],
            unavailable: vec![],
            consumed: 2,
        };
        assert!(validate_records(&response, a.conversation_id, [1; 32], &request).is_err());
    }
    #[test]
    fn partial_body_page_defers_only_uncommitted_targets() {
        let a = event(1);
        let b = event(2);
        let request = [*a.id.as_bytes(), *b.id.as_bytes()];
        let response = Records {
            scope: a.conversation_id,
            token: [1; 32],
            status: 2,
            events: vec![a.clone()],
            unavailable: vec![],
            consumed: 1,
        };
        assert_eq!(
            validate_records(&response, a.conversation_id, [1; 32], &request).unwrap(),
            vec![*b.id.as_bytes()]
        );
    }
    #[test]
    fn unavailable_targets_stay_pending_and_scope_is_checked() {
        let a = event(1);
        let request = [*a.id.as_bytes()];
        let response = Records {
            scope: a.conversation_id,
            token: [1; 32],
            status: 3,
            events: vec![],
            unavailable: vec![a.id],
            consumed: 1,
        };
        assert_eq!(
            validate_records(&response, a.conversation_id, [1; 32], &request).unwrap(),
            request
        );
        assert!(
            validate_records(&response, ConversationId::new([8; 32]), [1; 32], &request).is_err()
        );
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
    async fn range_production_group_turns_converge_both_directions_and_refresh_pooled_view() {
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let left_id = DeviceIdentity::generate();
        let left_account = Account::generate();
        let right_id = DeviceIdentity::generate();
        let right_account = Account::generate();
        let left_proof = Announce::new_with_account(&left_id, &left_account, "Left", 9);
        let right_proof = Announce::new_with_account(&right_id, &right_account, "Right", 9);
        let (left, _, _) = crate::node::test_support::node_with_files(
            dirs[0].path(),
            left_id,
            left_account,
            &right_proof,
        );
        let (right, _, _) = crate::node::test_support::node_with_files(
            dirs[1].path(),
            right_id,
            right_account,
            &left_proof,
        );
        left.cached_peer_snapshot_async().await.unwrap();
        right.cached_peer_snapshot_async().await.unwrap();
        let group = left
            .create_channel("Range production", vec![right.identity.public()])
            .await
            .unwrap();
        left.await_channel_metadata(group).await.unwrap();
        // Bootstrap signed group authority before content transfer; the range
        // protocol must never infer membership from an advertised message ID.
        let authority = left.log.lock().unwrap().events(&group).unwrap();
        for event in authority {
            right.log.lock().unwrap().append_durable(event).unwrap();
        }
        crate::node::test_support::settle_group_metadata(&right, group);
        let left_message = left
            .append_event_with_profile_parent(group, EventKind::Message, vec![1], None)
            .unwrap();
        // More than a body window of strict parent-linked events forces repair
        // when ID-prefix discovery encounters a child before its ancestor.
        for _ in 0..201 {
            left.append_event_with_profile_parent(group, EventKind::Message, vec![5], None)
                .unwrap();
        }
        let right_message = right
            .append_event_with_profile_parent(group, EventKind::Message, vec![2], None)
            .unwrap();
        let (a, b) = tokio::io::duplex(256 * 1024);
        let server = Arc::clone(&left);
        let task = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(b, &server.identity).await.unwrap();
            let store = server.serve_sync_store(channel.peer_identity());
            let mut state = Service::default();
            while let Ok(bytes) = channel.recv().await {
                if serve(&mut channel, &store, &mut state, &bytes)
                    .await
                    .unwrap()
                    .is_none()
                {
                    panic!("range fixture must not downgrade to full Have");
                }
            }
            state.retire(&store);
        });
        let mut channel = SecureChannel::connect(a, &right.identity, Some(&left.identity.public()))
            .await
            .unwrap();
        let store = right.sync_store(channel.peer_identity());
        let mut complete = false;
        let mut concurrent_appends = Vec::new();
        for _ in 0..1024 {
            match request(&mut channel, &store, group).await.unwrap() {
                Turn::Complete(_) => {
                    complete = true;
                    break;
                }
                Turn::More(_) => {
                    let (_, id) = left
                        .append_event_with_profile_parent(group, EventKind::Message, vec![4], None)
                        .unwrap();
                    concurrent_appends.push(id);
                    tokio::task::yield_now().await;
                }
                Turn::Unsupported => panic!("production group source must negotiate ranges"),
                Turn::Limited(_) => panic!("small fixture must not hit policy limits"),
            }
        }
        assert!(
            complete,
            "a captured cut must finish despite concurrent appends"
        );
        assert!(!concurrent_appends.is_empty());
        assert!(
            right
                .log
                .lock()
                .unwrap()
                .get(&left_message.1)
                .unwrap()
                .is_some()
        );
        assert!(
            left.log
                .lock()
                .unwrap()
                .get(&right_message.1)
                .unwrap()
                .is_some()
        );
        let later = left
            .append_event_with_profile_parent(group, EventKind::Message, vec![3], None)
            .unwrap();

        complete = false;
        // The first phase appends on every bounded More turn. Its cold
        // authority replay can leave hundreds of additional records for this
        // pooled refresh, each still processed under the normal turn budget.
        for _ in 0..8192 {
            if matches!(
                request(&mut channel, &store, group).await.unwrap(),
                Turn::Complete(_)
            ) {
                complete = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(complete);
        assert!(
            right.log.lock().unwrap().get(&later.1).unwrap().is_some(),
            "a completed pooled snapshot must not hide a later append"
        );
        for id in concurrent_appends {
            assert!(
                right.log.lock().unwrap().get(&id).unwrap().is_some(),
                "the next fresh pooled snapshot must include concurrent appends"
            );
        }
        // Accepted owner controls invalidate the receiver's captured principal
        // view. The pooled protocol must yield/requalify, not close its socket
        // or pretend that this now-obsolete comparison completed.
        left.rename_channel(group, "Range production renamed")
            .await
            .unwrap();
        let rename = left
            .log
            .lock()
            .unwrap()
            .events(&group)
            .unwrap()
            .into_iter()
            .find(|event| event.kind == EventKind::ChannelRename)
            .expect("real owner rename");
        complete = false;
        for _ in 0..8192 {
            if matches!(
                request(&mut channel, &store, group).await.unwrap(),
                Turn::Complete(_)
            ) {
                complete = true;
                break;
            }
            tokio::task::yield_now().await;
        }
        assert!(
            complete,
            "an accepted control must requalify and converge on the same stream"
        );
        assert_eq!(
            right.log.lock().unwrap().get(&rename.id).unwrap(),
            Some(rename)
        );
        drop(store);
        drop(channel);
        task.await.unwrap();
    }
}

#[cfg(test)]
mod limited_frontier_tests {
    use super::*;
    use crate::eventlog::sync::range::index::RangeIndex;
    #[test]
    fn completed_checkpoint_revalidates_new_remote_ready_before_skipping_queries() {
        let local = RangeIndex::default();
        let mut remote = RangeIndex::default();
        let mut checkpoint = Checkpoint::for_remote([7; 32], [9; 32], &local, remote.revision());
        let prefix = checkpoint.next_query().unwrap();
        checkpoint
            .accept_query(
                &local,
                [7; 32],
                [8; 32],
                [9; 32],
                remote.revision(),
                remote.frame([7; 32], [8; 32], prefix),
            )
            .unwrap();
        assert!(!checkpoint.has_pending());
        assert!(checkpoint.valid_remote_binding(
            &local,
            [7; 32],
            [8; 32],
            [9; 32],
            remote.revision()
        ));
        assert!(!checkpoint.valid_remote_binding(
            &local,
            [7; 32],
            [6; 32],
            [9; 32],
            remote.revision()
        ));
        remote.insert([1; 32]);
        assert!(!checkpoint.valid_remote_binding(
            &local,
            [7; 32],
            [8; 32],
            [9; 32],
            remote.revision()
        ));
    }
    #[test]
    fn unavailable_only_frontier_is_limited_without_claiming_completion() {
        let local = RangeIndex::default();
        let mut remote = RangeIndex::default();
        remote.insert([1; 32]);
        let mut checkpoint = Checkpoint::for_remote([7; 32], [9; 32], &local, remote.revision());
        let prefix = checkpoint.next_query().unwrap();
        let missing = checkpoint
            .accept_query(
                &local,
                [7; 32],
                [8; 32],
                [9; 32],
                remote.revision(),
                remote.frame([7; 32], [8; 32], prefix),
            )
            .unwrap();
        checkpoint.mark_unavailable(&missing);
        checkpoint.defer_missing(&missing);
        assert!(checkpoint.has_pending());
        assert!(!checkpoint.has_unblocked_pending());
    }
    #[test]
    fn unavailable_target_does_not_displace_unconsumed_targets() {
        let local = RangeIndex::default();
        let mut remote = RangeIndex::default();
        remote.insert([1; 32]);
        remote.insert([2; 32]);
        let mut checkpoint = Checkpoint::for_remote([7; 32], [9; 32], &local, remote.revision());
        let prefix = checkpoint.next_query().unwrap();
        checkpoint
            .accept_query(
                &local,
                [7; 32],
                [8; 32],
                [9; 32],
                remote.revision(),
                remote.frame([7; 32], [8; 32], prefix),
            )
            .unwrap();
        checkpoint.mark_unavailable(&[[1; 32]]);
        // Body-page continuation defers its unconsumed suffix before unavailable IDs.
        checkpoint.defer_missing(&[[2; 32], [1; 32]]);
        assert!(checkpoint.has_unblocked_pending());
        assert_eq!(checkpoint.next_query().unwrap().key(), [2; 32]);
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct OrderedRecords {
    scope: ConversationId,
    token: [u8; 32],
    after: i64,
    next: i64,
    complete: bool,
    events: Vec<crate::eventlog::Event>,
}

fn validate_ordered_records(
    response: &OrderedRecords,
    conversation: ConversationId,
    token: [u8; 32],
    after: i64,
) -> Result<(), SessionError> {
    let mut seen = HashSet::new();
    if response.scope != conversation
        || response.token != token
        || response.after != after
        || response.next < after
        || ((!response.complete || !response.events.is_empty()) && response.next == after)
        || response
            .events
            .iter()
            .any(|e| e.conversation_id != conversation || !seen.insert(e.id))
    {
        return Err(SessionError::UnexpectedMessage);
    }
    Ok(())
}

#[cfg(test)]
mod ordered_wire_tests {
    use super::*;
    #[test]
    fn rejects_nonadvancing_and_wrong_binding() {
        let scope = ConversationId::new([1; 32]);
        let mut page = OrderedRecords {
            scope,
            token: [2; 32],
            after: -1,
            next: -1,
            complete: false,
            events: vec![],
        };
        assert!(validate_ordered_records(&page, scope, [2; 32], -1).is_err());
        page.next = 99;
        assert!(validate_ordered_records(&page, scope, [2; 32], -1).is_ok());
        assert!(validate_ordered_records(&page, scope, [3; 32], -1).is_err());
        assert!(validate_ordered_records(&page, scope, [2; 32], 99).is_err());
        page.complete = true;
        page.next = -2;
        assert!(validate_ordered_records(&page, scope, [2; 32], -1).is_err());
    }
}

#[cfg(test)]
mod account_delivery_tests {
    use super::*;
    use crate::{
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
    };
    use std::sync::Arc;

    #[tokio::test]
    async fn account_dm_range_turns_deliver_without_collecting_fallback() {
        account_dm_range_fixture(false).await;
    }

    #[tokio::test]
    async fn account_dm_range_continuation_survives_interleaved_exact_custody() {
        account_dm_range_fixture(true).await;
    }

    async fn account_dm_range_fixture(interleave_custody: bool) {
        let dirs = [tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap()];
        let a = DeviceIdentity::generate();
        let b = DeviceIdentity::generate();
        let sender_account = Account::generate();
        let receiver_account = Account::generate();
        let ap = Announce::new_with_account(&a, &sender_account, "sender", 9);
        let bp = Announce::new_with_account(&b, &receiver_account, "receiver", 9);
        let (sender, _, _) =
            crate::node::test_support::node_with_files(dirs[0].path(), a, sender_account, &bp);
        let (receiver, _, _) =
            crate::node::test_support::node_with_files(dirs[1].path(), b, receiver_account, &ap);
        sender.cached_peer_snapshot_async().await.unwrap();
        receiver.cached_peer_snapshot_async().await.unwrap();
        sender
            .send_to_account(&receiver.account_id(), b"bounded account DM", None)
            .await
            .unwrap();
        let scope = crate::node::conversation::dm_conversation_id(
            &sender.identity.public(),
            &receiver.identity.public(),
        );
        let events = sender.log.lock().unwrap().events(&scope).unwrap();
        assert_eq!(events.len(), 1);
        let id = events[0].id;
        let (a, b) = tokio::io::duplex(256 * 1024);
        let server = Arc::clone(&receiver);
        let task = tokio::spawn(async move {
            let mut channel = SecureChannel::accept(b, &server.identity).await.unwrap();
            let store = server.serve_sync_store(channel.peer_identity());
            let mut service = Service::default();
            while let Ok(bytes) = channel.recv().await {
                if serve(&mut channel, &store, &mut service, &bytes)
                    .await
                    .unwrap()
                    .is_none()
                {
                    assert!(
                        matches!(decode(&bytes), Ok(SyncWire::DurableHaveRequest { .. })),
                        "account delivery must not fall back to collecting history"
                    );
                    serve_wire_bytes_paged(&mut channel, &store, &bytes)
                        .await
                        .unwrap();
                }
            }
            service.retire(&store);
        });
        let mut channel =
            SecureChannel::connect(a, &sender.identity, Some(&receiver.identity.public()))
                .await
                .unwrap();
        let store = sender.sync_store(channel.peer_identity());
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for _ in 0..128 {
                match request(&mut channel, &store, scope).await? {
                    Turn::Complete(_) => return Ok::<(), SessionError>(()),
                    Turn::More(_) => {
                        if interleave_custody {
                            request_durable_have(&mut channel, scope, id).await?;
                        }
                        tokio::task::yield_now().await;
                    }
                    _ => panic!("one authorized message must fit the range budget"),
                }
            }
            panic!("bounded account delivery must finish its finite cut");
        })
        .await;
        drop(channel);
        task.await.unwrap();
        outcome.unwrap().unwrap();
        assert!(
            receiver.log.lock().unwrap().get(&id).unwrap().is_some(),
            "a completed two-way turn must transfer the sender's exact DM"
        );
    }
}

#[cfg(test)]
mod custody_readiness_tests {
    use super::*;
    use crate::eventlog::Event;
    use crate::{
        discovery::Announce,
        identity::{account::Account, device::DeviceIdentity},
    };

    #[tokio::test]
    async fn stale_range_token_does_not_qualify_unprepared_exact_custody() {
        let directory = tempfile::tempdir().unwrap();
        let peer = DeviceIdentity::generate();
        let proof = Announce::new_with_account(&peer, &Account::generate(), "peer", 9);
        let (node, _, _) = crate::node::test_support::node_with_files(
            directory.path(),
            DeviceIdentity::generate(),
            Account::generate(),
            &proof,
        );
        node.cached_peer_snapshot_async().await.unwrap();
        let conversation =
            crate::node::conversation::dm_conversation_id(&node.identity.public(), &peer.public());
        let event = Event::new(
            &peer,
            conversation,
            1,
            vec![],
            1,
            0,
            EventKind::Message,
            vec![7],
        );
        let id = event.id;
        node.log.lock().unwrap().append_durable(event).unwrap();
        let store = node.serve_sync_store(&peer.public());
        let view = {
            let mut source = store.lock().unwrap();
            source.bind_session_peer(&peer.public());
            source.open_view(&conversation).unwrap()
        };
        // A token can outlive readiness when a retained range resumes work.
        // It certifies a previous range root, not the current preparation state.
        let mut state = Service {
            view: Some((conversation, view)),
            token: Some([7; 32]),
            ..Default::default()
        };
        let (a, b) = tokio::io::duplex(65536);
        let node_public = node.identity.public();
        let (client, server) = tokio::join!(
            SecureChannel::connect(a, &peer, Some(&node_public)),
            SecureChannel::accept(b, &node.identity),
        );
        let mut client = client.unwrap();
        let mut server = server.unwrap();
        let range_state = &mut state;
        let serving_store = &store;
        let (held, served) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::join!(
                request_durable_have(&mut client, conversation, id),
                async move {
                    let bytes = server.recv().await?;
                    match serve(&mut server, serving_store, range_state, &bytes).await? {
                        Some(served) => Ok::<_, SessionError>(served),
                        None => serve_wire_bytes_paged(&mut server, serving_store, &bytes).await,
                    }
                }
            )
        })
        .await
        .expect("exact custody must finish without draining the range service");
        served.expect("an old range token must not expose an unfinished selection");
        assert!(
            held.unwrap(),
            "fresh prepared authority must qualify the retained exact message"
        );
        assert!(
            state.view.is_some(),
            "independent custody must preserve range continuation ownership"
        );
        state.retire(&store);
    }
}
