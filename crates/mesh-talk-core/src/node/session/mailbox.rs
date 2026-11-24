//! Optional bounded opaque-mailbox exchange on an authenticated sync channel.
use super::*;
use crate::eventlog::Event;
use crate::postoffice::{IndexedPostOffice, MailboxCursor, MailboxDeltaPage};
#[cfg(test)]
use mesh_mailbox::rpc::{decode_frame, marker};
use mesh_mailbox::{
    rpc::{
        Frame, PAGE_IDS, RpcError, encode_frame, is_mailbox_frame, make_get_request,
        read_delta_response, read_get_response,
    },
    service::{
        DispatchError, MailboxStore, dispatch, make_probe, probe_scope, read_probe_response,
    },
};
#[cfg(test)]
use sha2::{Digest, Sha256};

impl From<RpcError> for SessionError {
    fn from(error: RpcError) -> Self {
        match error {
            RpcError::UnexpectedMessage => Self::UnexpectedMessage,
            RpcError::Serialization(message) => Self::Serialization(message),
        }
    }
}

pub(crate) async fn negotiate<IO: AsyncRead + AsyncWrite + Unpin>(
    channel: &mut SecureChannel<IO>,
    conversation: ConversationId,
) -> Result<bool, SessionError> {
    channel.send(&make_probe(conversation)?).await?;
    Ok(read_probe_response(&channel.recv().await?, conversation)?)
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
    let page = read_delta_response(&channel.recv().await?, conversation)?;
    valid_delta_page(&page, conversation, prior)
        .then_some(page)
        .ok_or(SessionError::UnexpectedMessage)
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
    channel
        .send(&make_get_request(conversation, cursor, ids)?)
        .await?;
    Ok(read_get_response(
        &channel.recv().await?,
        conversation,
        ids,
        crate::eventlog::sync::encoded_event_size,
    )?)
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

struct IndexedMailboxStore<'a>(&'a Mutex<IndexedPostOffice>);

impl MailboxStore for IndexedMailboxStore<'_> {
    type Error = crate::eventlog::LogError;

    fn delta(
        &self,
        scope: &ConversationId,
        prior: Option<&MailboxCursor>,
        limit: usize,
    ) -> Result<MailboxDeltaPage, Self::Error> {
        self.0
            .lock()
            .expect("relay store lock not poisoned")
            .mailbox_delta_page(scope, prior, limit)
    }

    fn bodies(
        &self,
        scope: &ConversationId,
        cursor: &MailboxCursor,
        ids: &[EventId],
        max_bytes: usize,
    ) -> Result<Vec<Event>, Self::Error> {
        self.0
            .lock()
            .expect("relay store lock not poisoned")
            .mailbox_delta_events(scope, cursor, ids, max_bytes)
    }
}

/// Ordinary clients retain the existing bounded handler for every legacy frame.
/// Only a successful marker exchange on this exact connection enables MTOM1.
pub(crate) async fn serve<IO: AsyncRead + AsyncWrite + Unpin + Send>(
    channel: &mut SecureChannel<IO>,
    store: &Mutex<IndexedPostOffice>,
    bytes: &[u8],
    budget: &RemoteHaveBudget,
    authorized: &mut Option<ConversationId>,
) -> Result<Served, SessionError> {
    if is_mailbox_frame(bytes) {
        let _permit = budget.try_admit()?;
        let reply =
            dispatch(&IndexedMailboxStore(store), *authorized, bytes).map_err(
                |error| match error {
                    DispatchError::Wire(error) => SessionError::from(error),
                    DispatchError::Store(error) => SessionError::from(error),
                },
            )?;
        channel.send(&reply.bytes).await?;
        return Ok(Served::Handled(reply.scope));
    }
    if let Some(scope) = probe_scope(bytes) {
        // Validate current role availability even for capability probes. This
        // reads only fixed metadata and does not trigger a fingerprint build.
        let office = store.lock().expect("relay store lock not poisoned").clone();
        use crate::eventlog::sync::network::NetworkSyncStore;
        let view = office.open_view(&scope)?;
        office.validate_view(&view)?;
        channel
            .send(&encode_frame(&Frame::Supported {
                conversation: scope,
            })?)
            .await?;
        *authorized = Some(scope);
        return Ok(Served::Handled(scope));
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
