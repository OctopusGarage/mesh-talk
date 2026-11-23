//! Optional bounded opaque-mailbox exchange on an authenticated sync channel.
use super::*;
use crate::eventlog::Event;
use crate::postoffice::{IndexedPostOffice, MailboxCursor, MailboxDeltaPage};
#[cfg(test)]
use mesh_mailbox::rpc::{decode_frame, marker};
use mesh_mailbox::{
    rpc::{
        Frame, RpcError, encode_frame, is_mailbox_frame, make_get_request, read_delta_response,
        read_get_response,
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
    Ok(read_delta_response(&channel.recv().await?, conversation)?)
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
