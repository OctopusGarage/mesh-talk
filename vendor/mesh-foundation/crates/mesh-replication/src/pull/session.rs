//! Versioned request/response adapter for an authenticated frame session.

use super::{PullError, PullErrorKind, PullLimits, Record, pull_missing, validate_batch};
use crate::{CompleteInventory, EventId, session::ReconciliationSession};

/// Upper bound for the default record profile, including its headers.
pub const MAX_RECORD_FRAME: usize = 65_519;
const REQUEST: &[u8; 5] = b"MRQ1\0";
const RESPONSE: &[u8; 5] = b"MRS1\0";
const REQUEST_HEADER: usize = REQUEST.len() + 32 + 4;
const RESPONSE_HEADER: usize = RESPONSE.len() + 32;
const RECORD_HEADER: usize = 32 + 4;

#[derive(Debug, PartialEq, Eq)]
pub enum RecordExchangeError<Io, Store = std::convert::Infallible> {
    Transport(Io),
    InvalidFrame,
    ScopeMismatch,
    Store(Store),
}

impl<Io: std::fmt::Display, Store: std::fmt::Display> std::fmt::Display
    for RecordExchangeError<Io, Store>
{
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(error) => write!(f, "record session failed: {error}"),
            Self::InvalidFrame => f.write_str("invalid record exchange frame"),
            Self::ScopeMismatch => f.write_str("record exchange scope mismatch"),
            Self::Store(error) => write!(f, "record loader failed: {error}"),
        }
    }
}

impl<Io: std::error::Error + 'static, Store: std::error::Error + 'static> std::error::Error
    for RecordExchangeError<Io, Store>
{
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Transport(error) => Some(error),
            Self::Store(error) => Some(error),
            _ => None,
        }
    }
}

/// Fetch one bounded batch of opaque bodies without committing them.
/// The caller owns authorization, verification, and persistence. Empty selections
/// return immediately after limit validation. Discard the session after an error.
pub async fn fetch_records<S: ReconciliationSession>(
    session: &mut S,
    scope: [u8; 32],
    ids: &[EventId],
    limits: PullLimits,
) -> Result<Vec<Record>, RecordExchangeError<S::Error>> {
    if !valid_limits(limits)
        || ids.len() > limits.records_per_batch
        || ids.iter().collect::<std::collections::HashSet<_>>().len() != ids.len()
    {
        return Err(RecordExchangeError::InvalidFrame);
    }
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let records = request_records(session, scope, ids, limits).await?;
    validate_batch(ids, &records, limits.bytes_per_batch)
        .map_err(|_| RecordExchangeError::InvalidFrame)?;
    Ok(records)
}

// Shared wire exchange. Existing pull operations retain their public semantic
// rejection variants in validate_batch; standalone fetch validates before return.
async fn request_records<S: ReconciliationSession>(
    session: &mut S,
    scope: [u8; 32],
    ids: &[EventId],
    limits: PullLimits,
) -> Result<Vec<Record>, RecordExchangeError<S::Error>> {
    let mut request = Vec::with_capacity(REQUEST_HEADER + ids.len() * 32);
    request.extend_from_slice(REQUEST);
    request.extend_from_slice(&scope);
    request.extend_from_slice(&(limits.bytes_per_batch as u32).to_be_bytes());
    for id in ids {
        request.extend_from_slice(id);
    }
    session
        .send(&request)
        .await
        .map_err(RecordExchangeError::Transport)?;
    let response = session
        .recv()
        .await
        .map_err(RecordExchangeError::Transport)?;
    decode_response(&response, &scope, ids.len(), limits.bytes_per_batch)
}

/// Pull from a peer that serves the default record profile. The caller owns
/// scope authorization and must verify and commit each batch as in [pull_missing].
pub async fn pull_over_session<S: ReconciliationSession, Commit>(
    session: &mut S,
    scope: [u8; 32],
    remote: &CompleteInventory,
    local: &CompleteInventory,
    limits: PullLimits,
    commit: impl FnMut(&[Record]) -> Result<(), Commit>,
) -> Result<usize, PullError<RecordExchangeError<S::Error>, Commit>> {
    if !valid_limits(limits) {
        return Err(PullError {
            committed: 0,
            kind: PullErrorKind::InvalidLimits,
        });
    }
    pull_missing(
        remote,
        local,
        limits,
        async |ids, budget| {
            request_records(
                session,
                scope,
                ids,
                PullLimits {
                    records_per_batch: limits.records_per_batch,
                    bytes_per_batch: budget,
                },
            )
            .await
        },
        commit,
    )
    .await
}

/// Bounded-turn counterpart of [pull_over_session]. Resume from fresh durable inventory.
pub async fn pull_over_session_bounded<S: ReconciliationSession, Commit>(
    session: &mut S,
    scope: [u8; 32],
    remote: &CompleteInventory,
    local: &CompleteInventory,
    limits: PullLimits,
    turn: super::PullTurnLimits,
    commit: impl FnMut(&[Record]) -> Result<(), Commit>,
) -> Result<super::PullProgress, PullError<RecordExchangeError<S::Error>, Commit>> {
    if !valid_limits(limits) {
        return Err(PullError {
            committed: 0,
            kind: PullErrorKind::InvalidLimits,
        });
    }
    super::pull_missing_bounded(
        remote,
        local,
        limits,
        turn,
        async |ids, budget| {
            request_records(
                session,
                scope,
                ids,
                PullLimits {
                    records_per_batch: limits.records_per_batch,
                    bytes_per_batch: budget,
                },
            )
            .await
        },
        commit,
    )
    .await
}

/// Serve one request for an authorized scope. The loader gets the authenticated
/// peer, requested IDs and byte budget. It must recheck current authorization
/// before returning records. After an exchange error, discard the session.
pub async fn serve_record_request<S: ReconciliationSession, Store>(
    session: &mut S,
    scope: [u8; 32],
    limits: PullLimits,
    load: impl AsyncFnOnce(&S::Peer, &[EventId], usize) -> Result<Vec<Record>, Store>,
) -> Result<(), RecordExchangeError<S::Error, Store>> {
    if !valid_limits(limits) {
        return Err(RecordExchangeError::InvalidFrame);
    }
    let request = session
        .recv()
        .await
        .map_err(RecordExchangeError::Transport)?;
    let payload = scoped_payload(&request, REQUEST, &scope)?;
    let Some((budget, payload)) = payload.split_first_chunk::<4>() else {
        return Err(RecordExchangeError::InvalidFrame);
    };
    let budget = u32::from_be_bytes(*budget) as usize;
    let (ids, remainder) = payload.as_chunks::<32>();
    if budget == 0
        || !remainder.is_empty()
        || ids.is_empty()
        || ids.len() > limits.records_per_batch
        || RESPONSE_HEADER + ids.len() * RECORD_HEADER + budget.min(MAX_RECORD_FRAME)
            > MAX_RECORD_FRAME
    {
        return Err(RecordExchangeError::InvalidFrame);
    }
    let unique: std::collections::HashSet<_> = ids.iter().collect();
    if unique.len() != ids.len() {
        return Err(RecordExchangeError::InvalidFrame);
    }
    let budget = budget.min(limits.bytes_per_batch);
    let records = load(session.peer_identity(), ids, budget)
        .await
        .map_err(RecordExchangeError::Store)?;
    if validate_batch(ids, &records, budget).is_err() {
        return Err(RecordExchangeError::InvalidFrame);
    }
    let mut response = Vec::new();
    response.extend_from_slice(RESPONSE);
    response.extend_from_slice(&scope);
    for record in records {
        response.extend_from_slice(&record.id);
        response.extend_from_slice(&(record.body.len() as u32).to_be_bytes());
        response.extend_from_slice(&record.body);
    }
    session
        .send(&response)
        .await
        .map_err(RecordExchangeError::Transport)
}

fn valid_limits(limits: PullLimits) -> bool {
    (1..=(MAX_RECORD_FRAME - REQUEST_HEADER) / 32).contains(&limits.records_per_batch)
        && (1..=MAX_RECORD_FRAME
            .saturating_sub(RESPONSE_HEADER + limits.records_per_batch * RECORD_HEADER))
            .contains(&limits.bytes_per_batch)
}

fn scoped_payload<'a, Io, Store>(
    bytes: &'a [u8],
    tag: &[u8; 5],
    scope: &[u8; 32],
) -> Result<&'a [u8], RecordExchangeError<Io, Store>> {
    if bytes.len() > MAX_RECORD_FRAME {
        return Err(RecordExchangeError::InvalidFrame);
    }
    let Some((actual, payload)) = bytes
        .strip_prefix(tag)
        .and_then(|bytes| bytes.split_first_chunk::<32>())
    else {
        return Err(RecordExchangeError::InvalidFrame);
    };
    if actual != scope {
        return Err(RecordExchangeError::ScopeMismatch);
    }
    Ok(payload)
}

fn decode_response<Io>(
    bytes: &[u8],
    scope: &[u8; 32],
    count: usize,
    mut budget: usize,
) -> Result<Vec<Record>, RecordExchangeError<Io>> {
    let mut payload = scoped_payload(bytes, RESPONSE, scope)?;
    let mut records = Vec::new();
    while !payload.is_empty() {
        let Some((id, offset)) = payload
            .split_first_chunk::<32>()
            .filter(|_| records.len() < count)
        else {
            return Err(RecordExchangeError::InvalidFrame);
        };
        let Some((len, offset)) = offset.split_first_chunk::<4>() else {
            return Err(RecordExchangeError::InvalidFrame);
        };
        let len = u32::from_be_bytes(*len) as usize;
        budget = budget
            .checked_sub(len)
            .ok_or(RecordExchangeError::InvalidFrame)?;
        let (body, next) = offset
            .split_at_checked(len)
            .ok_or(RecordExchangeError::InvalidFrame)?;
        records.push(Record {
            id: *id,
            body: body.to_vec(),
        });
        payload = next;
    }
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    struct SingleRequest(Option<Vec<u8>>);

    impl ReconciliationSession for SingleRequest {
        type Peer = ();
        type Error = &'static str;

        fn peer_identity(&self) -> &Self::Peer {
            &()
        }

        async fn send(&mut self, _: &[u8]) -> Result<(), Self::Error> {
            panic!("invalid requests cannot produce a response")
        }

        async fn recv(&mut self) -> Result<Vec<u8>, Self::Error> {
            self.0.take().ok_or("closed")
        }
    }

    #[tokio::test]
    async fn foreign_scope_is_rejected_before_the_loader() {
        let mut request = REQUEST.to_vec();
        request.extend_from_slice(&[2; 32]);
        request.extend_from_slice(&1u32.to_be_bytes());
        request.extend_from_slice(&[3; 32]);
        let result = serve_record_request(
            &mut SingleRequest(Some(request)),
            [1; 32],
            PullLimits {
                records_per_batch: 1,
                bytes_per_batch: 1,
            },
            async |_: &(), _: &[[u8; 32]], _| -> Result<Vec<Record>, ()> {
                panic!("foreign request cannot reach the loader")
            },
        )
        .await;
        assert_eq!(result, Err(RecordExchangeError::ScopeMismatch));
    }

    #[test]
    fn rejects_oversized_or_truncated_body_before_allocating_it() {
        let scope = [2; 32];
        let mut response = RESPONSE.to_vec();
        response.extend_from_slice(&scope);
        response.extend_from_slice(&[3; 32]);
        response.extend_from_slice(&3u32.to_be_bytes());
        response.extend_from_slice(&[1, 2]);
        for budget in [1, 3] {
            assert!(matches!(
                decode_response::<()>(&response, &scope, 1, budget),
                Err(RecordExchangeError::InvalidFrame)
            ));
        }
    }
}
