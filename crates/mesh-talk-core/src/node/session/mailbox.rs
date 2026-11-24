//! Optional bounded opaque-mailbox exchange on an authenticated sync channel.
use super::*;
use crate::eventlog::Event;
use crate::postoffice::{IndexedPostOffice, MailboxCursor, MailboxDeltaPage};
use sha2::{Digest, Sha256};

const MAGIC: &[u8; 5] = b"MTOM1";
const PAGE_IDS: usize = 64;
const BODY_BUDGET: usize = MAX_PLAINTEXT - 2048;

#[derive(Debug, Serialize, Deserialize)]
enum Frame {
    Supported {
        conversation: ConversationId,
    },
    Delta {
        conversation: ConversationId,
        prior: Option<MailboxCursor>,
    },
    Page(MailboxDeltaPage),
    Get {
        conversation: ConversationId,
        cursor: MailboxCursor,
        ids: Vec<EventId>,
    },
    Events {
        conversation: ConversationId,
        events: Vec<Event>,
    },
}

// The legacy probe contains a domain-separated digest rather than a real set
// fingerprint. An ordinary fingerprint can coincide only by a SHA256 collision;
// even then a legacy matched=true response is an error, never positive support.
// Consequently old endpoints receive only a frame they already understand.
fn marker(conversation: ConversationId) -> [u8; 32] {
    let mut hash = Sha256::new();
    hash.update(b"mesh-talk-opaque-mailbox-rpc-v1-probe\0");
    hash.update(conversation.as_bytes());
    hash.finalize().into()
}

fn encode_frame(frame: &Frame) -> Result<Vec<u8>, SessionError> {
    let body = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit((MAX_PLAINTEXT - MAGIC.len()) as u64)
        .reject_trailing_bytes()
        .serialize(frame)
        .map_err(|e| SessionError::Serialization(e.to_string()))?;
    if body.len() + MAGIC.len() > MAX_PLAINTEXT {
        return Err(SessionError::Serialization(
            "oversized mailbox frame".into(),
        ));
    }
    let mut result = Vec::with_capacity(MAGIC.len() + body.len());
    result.extend_from_slice(MAGIC);
    result.extend_from_slice(&body);
    Ok(result)
}

fn decode_frame(bytes: &[u8]) -> Result<Frame, SessionError> {
    if bytes.len() > MAX_PLAINTEXT || !bytes.starts_with(MAGIC) {
        return Err(SessionError::UnexpectedMessage);
    }
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit((MAX_PLAINTEXT - MAGIC.len()) as u64)
        .reject_trailing_bytes()
        .deserialize(&bytes[MAGIC.len()..])
        .map_err(|e| SessionError::Serialization(e.to_string()))
}

pub(crate) async fn negotiate<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    conversation: ConversationId,
) -> Result<bool, SessionError> {
    channel
        .send(&encode(&SyncWire::FpRequest(FpRequest {
            conversation,
            fingerprint: marker(conversation),
        }))?)
        .await?;
    let bytes = channel.recv().await?;
    if bytes.starts_with(MAGIC) {
        return match decode_frame(&bytes)? {
            Frame::Supported { conversation: c } if c == conversation => Ok(true),
            _ => Err(SessionError::UnexpectedMessage),
        };
    }
    match decode(&bytes)? {
        SyncWire::FpResponse(FpResponse { matched: false }) => Ok(false),
        _ => Err(SessionError::UnexpectedMessage),
    }
}

pub(crate) async fn delta<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    conversation: ConversationId,
    prior: Option<&MailboxCursor>,
) -> Result<MailboxDeltaPage, SessionError> {
    channel
        .send(&encode_frame(&Frame::Delta {
            conversation,
            prior: prior.cloned(),
        })?)
        .await?;
    match decode_frame(&channel.recv().await?)? {
        Frame::Page(page) if valid_delta_page(&page, conversation, prior) => Ok(page),
        _ => Err(SessionError::UnexpectedMessage),
    }
}

fn valid_delta_page(
    page: &MailboxDeltaPage,
    scope: ConversationId,
    prior: Option<&MailboxCursor>,
) -> bool {
    if page.cursor.conversation != scope
        || page.ids.len() > PAGE_IDS
        || page.complete != page.cursor.window.is_none()
        || page.cursor.after < 0
        || page.cursor.revision < 0
        || page.cursor.epoch < 0
        || page
            .ids
            .iter()
            .collect::<std::collections::HashSet<_>>()
            .len()
            != page.ids.len()
        || page.cursor.window.is_some_and(|(upper, revision)| {
            upper < page.cursor.after || revision < page.cursor.revision
        })
    {
        return false;
    }
    if page.reset {
        return true;
    } // The owner explicitly handles source reset.
    let Some(prior) = prior else {
        return true;
    };
    if prior.store_uuid != page.cursor.store_uuid
        || prior.conversation != page.cursor.conversation
        || prior.epoch != page.cursor.epoch
        || prior.first_seen != page.cursor.first_seen
        || page.cursor.after < prior.after
        || page.cursor.revision < prior.revision
        || (!page.complete && prior == &page.cursor)
    {
        return false;
    }
    if let Some(window) = prior.window {
        if page.complete {
            return page.cursor.after == window.0 && page.cursor.revision == window.1;
        }
        return page.cursor.window == Some(window) && page.cursor.revision == prior.revision;
    }
    true
}

pub(crate) async fn get<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    conversation: ConversationId,
    cursor: &MailboxCursor,
    ids: &[EventId],
) -> Result<Vec<Event>, SessionError> {
    if ids.len() > PAGE_IDS || cursor.conversation != conversation {
        return Err(SessionError::UnexpectedMessage);
    }
    channel
        .send(&encode_frame(&Frame::Get {
            conversation,
            cursor: cursor.clone(),
            ids: ids.to_vec(),
        })?)
        .await?;
    match decode_frame(&channel.recv().await?)? {
        Frame::Events {
            conversation: c,
            events,
        } if c == conversation
            && events.len() == ids.len()
            && events
                .iter()
                .zip(ids)
                .all(|(event, id)| event.id == *id && event.conversation_id == conversation)
            && events
                .iter()
                .try_fold(0usize, |sum, event| {
                    sum.checked_add(crate::eventlog::sync::encoded_event_size(event))
                })
                .is_some_and(|bytes| bytes <= BODY_BUDGET) =>
        {
            Ok(events)
        }
        _ => Err(SessionError::UnexpectedMessage),
    }
}

pub(crate) async fn upload<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    event: Event,
) -> Result<bool, SessionError> {
    let conversation = event.conversation_id;
    let id = event.id;
    channel
        .send(&encode(&SyncWire::Followup(SyncFollowup {
            conversation,
            events: vec![event],
        }))?)
        .await?;
    request_durable_have(channel, conversation, id).await
}

/// Ordinary clients retain the existing bounded handler for every legacy frame.
/// Only a successful marker exchange on this exact connection enables MTOM1.
pub(crate) async fn serve<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<IndexedPostOffice>,
    bytes: &[u8],
    budget: &RemoteHaveBudget,
    authorized: &mut Option<ConversationId>,
) -> Result<Served, SessionError> {
    if bytes.starts_with(MAGIC) {
        let _permit = budget.try_admit()?;
        let (conversation, response) = match decode_frame(bytes)? {
            Frame::Delta {
                conversation,
                prior,
            } if *authorized == Some(conversation) => {
                let page = store
                    .lock()
                    .expect("relay store lock not poisoned")
                    .mailbox_delta_page(&conversation, prior.as_ref(), PAGE_IDS)?;
                (conversation, Frame::Page(page))
            }
            Frame::Get {
                conversation,
                cursor,
                ids,
            } if *authorized == Some(conversation)
                && cursor.conversation == conversation
                && ids.len() <= PAGE_IDS =>
            {
                let events = store
                    .lock()
                    .expect("relay store lock not poisoned")
                    .mailbox_delta_events(&conversation, &cursor, &ids, BODY_BUDGET)?;
                (
                    conversation,
                    Frame::Events {
                        conversation,
                        events,
                    },
                )
            }
            _ => return Err(SessionError::UnexpectedMessage),
        };
        channel.send(&encode_frame(&response)?).await?;
        return Ok(Served::Handled(conversation));
    }
    if let Ok(SyncWire::FpRequest(request)) = decode(bytes)
        && request.fingerprint == marker(request.conversation)
    {
        // Validate current role availability even for capability probes. This
        // reads only fixed metadata and does not trigger a fingerprint build.
        let office = store.lock().expect("relay store lock not poisoned").clone();
        use crate::eventlog::sync::network::NetworkSyncStore;
        let view = office.open_view(&request.conversation)?;
        office.validate_view(&view)?;
        channel
            .send(&encode_frame(&Frame::Supported {
                conversation: request.conversation,
            })?)
            .await?;
        *authorized = Some(request.conversation);
        return Ok(Served::Handled(request.conversation));
    }
    serve_wire_bytes_budgeted(channel, store, bytes, budget).await
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod delta_transition_tests {
    use super::*;
    fn cursor() -> MailboxCursor {
        MailboxCursor {
            store_uuid: [1; 16],
            conversation: ConversationId::new([2; 32]),
            epoch: 0,
            first_seen: None,
            revision: 0,
            after: 64,
            window: Some((201, 1)),
        }
    }
    #[test]
    fn mailbox_partial_window_is_pinned_until_the_exact_terminal_watermark() {
        let prior = cursor();
        let mut page = MailboxDeltaPage {
            cursor: prior.clone(),
            ids: vec![],
            complete: false,
            reset: false,
        };
        assert!(!valid_delta_page(&page, prior.conversation, Some(&prior)));
        page.cursor.after = 128;
        assert!(valid_delta_page(&page, prior.conversation, Some(&prior)));
        page.cursor.window = Some((300, 2));
        assert!(!valid_delta_page(&page, prior.conversation, Some(&prior)));
        page.cursor.window = None;
        page.complete = true;
        page.cursor.revision = 1;
        assert!(!valid_delta_page(&page, prior.conversation, Some(&prior)));
        page.cursor.after = 201;
        assert!(valid_delta_page(&page, prior.conversation, Some(&prior)));
    }
    #[test]
    fn mailbox_duplicates_require_rejection_but_explicit_reset_is_preserved() {
        let prior = cursor();
        let mut next = prior.clone();
        next.after = 128;
        let id = EventId::new([3; 32]);
        let mut page = MailboxDeltaPage {
            cursor: next,
            ids: vec![id, id],
            complete: false,
            reset: false,
        };
        assert!(!valid_delta_page(&page, prior.conversation, Some(&prior)));
        page.ids.clear();
        page.cursor.store_uuid = [4; 16];
        assert!(!valid_delta_page(&page, prior.conversation, Some(&prior)));
        page.reset = true;
        assert!(valid_delta_page(&page, prior.conversation, Some(&prior)));
    }
}
