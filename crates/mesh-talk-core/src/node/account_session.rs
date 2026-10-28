//! Negotiated, exclusive sibling exchanges over the shared bounded TCP pool.
//! Capability discovery rides the first request without another request round trip.
use super::Node;
use super::account_sync_wire::{AccountHistoryFrame, AccountSessionRequest, AccountSyncVersion};
use super::node::NodeError;
use super::session_pool::{InboundReuse, SessionProtocol, retryable_transport_error};
use crate::discovery::PeerRecord;
use crate::transport::{SecureChannel, TransportError};
use rand_core::RngCore;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Duration;
use tokio::net::TcpStream;

#[cfg(test)]
pub(in crate::node) type AccountSessionTestHook = dyn Fn(
        crate::identity::device::PublicIdentity,
        AccountHistoryFrame,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>
    + Send
    + Sync;

#[cfg(test)]
pub(in crate::node) type AccountClientTestHook =
    dyn Fn(crate::identity::device::PublicIdentity, String) + Send + Sync;

#[cfg(test)]
pub(in crate::node) fn client_trace_outcome<T>(result: &Result<T, NodeError>) -> &'static str {
    match result {
        Ok(_) => "ok",
        Err(error) if super::account_sync::local_sync_capacity_busy(error) => "capacity_busy",
        Err(NodeError::Channel(message)) if message == "account session negotiation timed out" => {
            "negotiation_timeout"
        }
        Err(NodeError::Authorization(_)) => "authorization",
        Err(NodeError::Session(super::session::SessionError::Transport(_))) => "transport",
        Err(NodeError::Log(_)) => "storage",
        Err(_) => "invalid_or_interrupted",
    }
}

const NEGOTIATION_TIMEOUT: Duration = Duration::from_secs(5);
const SESSION_IDLE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_SESSION_REQUESTS: usize = 10_000;

pub(in crate::node) struct AccountExchange {
    channel: SecureChannel<TcpStream>,
    reusable: bool,
}
impl Deref for AccountExchange {
    type Target = SecureChannel<TcpStream>;
    fn deref(&self) -> &Self::Target {
        &self.channel
    }
}
impl DerefMut for AccountExchange {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.channel
    }
}
impl AccountExchange {
    pub(in crate::node) fn reusable(&self) -> bool {
        self.reusable
    }
    /// Only a fully validated, completed response may return to the pool.
    pub(in crate::node) fn recycle(self, node: &Node) {
        if self.reusable {
            node.recycle_account_channel(self.channel);
        }
    }
}
fn transport(error: TransportError) -> NodeError {
    NodeError::Session(super::session::SessionError::Transport(error))
}
async fn send(
    channel: &mut SecureChannel<TcpStream>,
    version: AccountSyncVersion,
    frame: &AccountHistoryFrame,
) -> Result<(), TransportError> {
    let bytes = frame
        .encode_version(version)
        .ok_or_else(|| TransportError::Serialization("invalid account session request".into()))?;
    channel.send(&bytes).await
}
impl Node {
    #[cfg(test)]
    pub(in crate::node) fn trace_account_client(
        &self,
        peer: &crate::identity::device::PublicIdentity,
        event: impl FnOnce() -> String,
    ) {
        let hook = self.account_client_test_hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(peer.clone(), event());
        }
    }

    pub(in crate::node) async fn begin_account_exchange(
        &self,
        peer: &PeerRecord,
        version: AccountSyncVersion,
        request: AccountHistoryFrame,
    ) -> Result<AccountExchange, NodeError> {
        #[cfg(test)]
        let trace = {
            let enabled = self.account_client_test_hook.lock().unwrap().is_some();
            enabled.then(|| {
                let domain = match &request {
                    AccountHistoryFrame::DocumentsRequest { body } => {
                        super::account_documents::wire::DocumentRequest::decode(body)
                            .map(|request| request.domain)
                    }
                    _ => None,
                };
                self.trace_account_client(&peer.public, || {
                    format!("negotiation_begin domain={domain:?}")
                });
                (std::time::Instant::now(), domain)
            })
        };
        if peer.account_id.as_deref() != Some(self.account_id().as_str())
            || !self.certified_sibling(&peer.public)
        {
            #[cfg(test)]
            if let Some(trace) = trace {
                self.trace_account_client(&peer.public, || {
                    format!(
                        "negotiation_end domain={:?} elapsed_ms={} outcome=authorization",
                        trace.1,
                        trace.0.elapsed().as_millis()
                    )
                });
            }
            return Err(NodeError::Authorization(
                "account session requires a certified sibling".into(),
            ));
        }
        let result = tokio::time::timeout(
            NEGOTIATION_TIMEOUT,
            self.begin_account_exchange_inner(peer, version, request),
        )
        .await
        .map_err(|_| NodeError::Channel("account session negotiation timed out".into()))
        .and_then(|result| result);
        #[cfg(test)]
        if let Some(trace) = trace {
            self.trace_account_client(&peer.public, || {
                format!(
                    "negotiation_end domain={:?} elapsed_ms={} outcome={}",
                    trace.1,
                    trace.0.elapsed().as_millis(),
                    client_trace_outcome(&result)
                )
            });
        }
        result
    }
    async fn begin_account_exchange_inner(
        &self,
        peer: &PeerRecord,
        version: AccountSyncVersion,
        request: AccountHistoryFrame,
    ) -> Result<AccountExchange, NodeError> {
        if version == AccountSyncVersion::V1 {
            let mut channel = self
                .privacy_dial(peer.addr, &peer.public)
                .await
                .map_err(transport)?;
            send(&mut channel, version, &request)
                .await
                .map_err(transport)?;
            return Ok(AccountExchange {
                channel,
                reusable: false,
            });
        }
        let body = AccountSessionRequest::from_frame(&request)
            .ok_or_else(|| NodeError::Channel("invalid reusable account request".into()))?;
        let optional_request = match &body {
            AccountSessionRequest::AttachmentChecked { .. }
            | AccountSessionRequest::FileMessageIdentities { .. } => true,
            AccountSessionRequest::Documents { body } => {
                super::account_documents::wire::DocumentRequest::decode(body)
                    .is_some_and(|request| request.version > 1)
            }
            _ => false,
        };
        let authentication_priority = match &body {
            AccountSessionRequest::Documents { body }
                if super::account_documents::wire::DocumentRequest::decode(body).is_some_and(
                    |request| request.domain == super::AccountDocumentDomain::GroupControl,
                ) =>
            {
                super::session_pool::AuthenticationPriority::Control
            }
            _ => super::session_pool::AuthenticationPriority::Ordinary,
        };
        for fresh in [false, true] {
            #[cfg(test)]
            self.trace_account_client(&peer.public, || {
                format!("session_checkout_begin fresh={fresh}")
            });
            let pooled = if fresh {
                None
            } else {
                self.checkout_reusable_channel(SessionProtocol::Account, peer.addr, &peer.public)
                    .await
                    .map_err(transport)?
            };
            let reused = pooled.is_some();
            #[cfg(test)]
            self.trace_account_client(&peer.public, || {
                format!("session_checkout_end reused={reused}")
            });
            let mut channel = match pooled {
                Some(channel) => channel,
                None => self
                    .privacy_dial_priority(peer.addr, &peer.public, authentication_priority)
                    .await
                    .map_err(transport)?,
            };
            #[cfg(test)]
            self.trace_account_client(&peer.public, || "session_channel_ready".into());
            let key = self.peer_endpoint_key(
                channel
                    .tcp_peer_addr()
                    .map_err(|error| transport(TransportError::Io(error)))?,
                &peer.public,
            );
            if !reused && !self.sessions.account_capability_due(&key) {
                send(&mut channel, version, &request)
                    .await
                    .map_err(transport)?;
                return Ok(AccountExchange {
                    channel,
                    reusable: false,
                });
            }
            let mut request_id = [0; 16];
            while request_id == [0; 16] {
                rand_core::OsRng.fill_bytes(&mut request_id);
            }
            let opening = async {
                #[cfg(test)]
                self.trace_account_client(&peer.public, || "session_request_send_begin".into());
                send(
                    &mut channel,
                    AccountSyncVersion::V2,
                    &AccountHistoryFrame::SessionRequest {
                        request_id,
                        request: body.clone(),
                    },
                )
                .await?;
                #[cfg(test)]
                self.trace_account_client(&peer.public, || {
                    "session_request_send_end_ready_wait".into()
                });
                channel.recv().await
            }
            .await;
            #[cfg(test)]
            self.trace_account_client(&peer.public, || {
                format!("session_ready_wait_end ok={}", opening.is_ok())
            });
            match opening {
                Ok(bytes) => {
                    if !matches!(AccountHistoryFrame::decode_versioned(&bytes), Some((AccountSyncVersion::V2, AccountHistoryFrame::SessionReady { request_id: echoed })) if echoed == request_id)
                    {
                        return Err(NodeError::Channel(
                            "invalid account session acknowledgement".into(),
                        ));
                    }
                    self.sessions.note_account_supported(&key);
                    return Ok(AccountExchange {
                        channel,
                        reusable: true,
                    });
                }
                Err(error) if retryable_transport_error(&error) && reused => {
                    // A liveness check can race a remote close. Start a fresh
                    // negotiation once, dropping any partially consumed stream.
                    drop(channel);
                }
                Err(error) if retryable_transport_error(&error) => {
                    if optional_request {
                        // An unknown appended request says nothing about the
                        // peer's support for the older reusable session owner.
                        return Err(transport(error));
                    }
                    // Old peers close on the appended optional tag. Only an
                    // authenticated pre-ack close permits legacy fallback.
                    drop(channel);
                    self.sessions.note_account_unsupported(key);
                    let mut channel = self
                        .privacy_dial_priority(peer.addr, &peer.public, authentication_priority)
                        .await
                        .map_err(transport)?;
                    send(&mut channel, version, &request)
                        .await
                        .map_err(transport)?;
                    return Ok(AccountExchange {
                        channel,
                        reusable: false,
                    });
                }
                Err(error) => return Err(transport(error)),
            }
        }
        Err(NodeError::Channel("reused account session closed".into()))
    }
    pub(in crate::node) async fn serve_account_session(
        self: &Arc<Self>,
        mut channel: SecureChannel<TcpStream>,
        mut request_id: [u8; 16],
        mut request: AccountSessionRequest,
    ) {
        for _ in 0..MAX_SESSION_REQUESTS {
            let frame = request.into_frame();
            let account_id = match &frame {
                AccountHistoryFrame::Request { account_id, .. }
                | AccountHistoryFrame::AttachmentRequest { account_id, .. }
                | AccountHistoryFrame::Wake { account_id, .. } => account_id,
                AccountHistoryFrame::AttachmentRequestChecked { account_id, .. }
                | AccountHistoryFrame::FileMessageIdentitiesRequest { account_id, .. } => {
                    account_id
                }
                AccountHistoryFrame::DocumentsRequest { body } => {
                    // The document owner repeats exact account authorization.
                    if super::account_documents::wire::DocumentRequest::decode(body).is_none() {
                        return;
                    }
                    // Own account is borrowed only until this admission block ends.
                    &self.account_id()
                }
                _ => return,
            };
            {
                let _operation = self.privacy.gate.read().await;
                if account_id != &self.account_id()
                    || !channel.io_admitted()
                    || !self.certified_sibling(channel.peer_identity())
                    || !self.account_allowed(channel.peer_identity(), channel.peer_announcement())
                {
                    return;
                }
            }
            if !matches!(
                tokio::time::timeout(
                    NEGOTIATION_TIMEOUT,
                    send(
                        &mut channel,
                        AccountSyncVersion::V2,
                        &AccountHistoryFrame::SessionReady { request_id }
                    )
                )
                .await,
                Ok(Ok(()))
            ) {
                return;
            }
            // Observe only acknowledged, authenticated exchanges. The hook is
            // instance-scoped and never holds a store mutex or disclosure guard.
            #[cfg(test)]
            {
                let hook = self.account_session_test_hook.lock().unwrap().clone();
                if let Some(hook) = hook {
                    hook(channel.peer_identity().clone(), frame.clone()).await;
                }
            }
            let complete = match frame {
                AccountHistoryFrame::FileMessageIdentitiesRequest {
                    account_id,
                    requests,
                } => {
                    self.serve_account_file_message_identities(&mut channel, &account_id, requests)
                        .await
                }
                AccountHistoryFrame::AttachmentRequestChecked {
                    account_id,
                    file_conv,
                } => {
                    self.serve_account_attachment_checked(
                        &mut channel,
                        &account_id,
                        crate::eventlog::event::ConversationId::new(file_conv),
                    )
                    .await
                }
                AccountHistoryFrame::Request {
                    account_id,
                    cursor,
                    known_digest,
                    wanted,
                    ..
                } => {
                    self.serve_account_history(
                        &mut channel,
                        AccountSyncVersion::V2,
                        &account_id,
                        cursor,
                        known_digest,
                        wanted,
                    )
                    .await
                }
                AccountHistoryFrame::AttachmentRequest {
                    account_id,
                    file_conv,
                    ..
                } => {
                    self.serve_account_attachment(
                        &mut channel,
                        AccountSyncVersion::V2,
                        &account_id,
                        crate::eventlog::event::ConversationId::new(file_conv),
                    )
                    .await
                }
                AccountHistoryFrame::DocumentsRequest { body } => {
                    let Some(request) =
                        super::account_documents::wire::DocumentRequest::decode(&body)
                    else {
                        return;
                    };
                    self.serve_account_documents(&mut channel, request).await
                }
                AccountHistoryFrame::Wake {
                    account_id,
                    epoch,
                    revision,
                } => {
                    self.serve_account_wake(
                        &mut channel,
                        AccountSyncVersion::V2,
                        &account_id,
                        epoch,
                        revision,
                    )
                    .await
                }
                _ => return,
            };
            if !complete {
                return;
            }
            let bytes = match tokio::time::timeout(
                SESSION_IDLE_TIMEOUT,
                self.recv_inbound_reuse(&mut channel),
            )
            .await
            {
                Ok(Ok(InboundReuse::Frame(bytes))) => bytes,
                Ok(Ok(InboundReuse::Retired(retirement))) => {
                    // Keep pressure coalesced until the actual socket/permits
                    // have been released by their owning channel.
                    drop(channel);
                    drop(retirement);
                    return;
                }
                _ => return,
            };
            match AccountHistoryFrame::decode_versioned(&bytes) {
                Some((
                    AccountSyncVersion::V2,
                    AccountHistoryFrame::SessionRequest {
                        request_id: next_id,
                        request: next,
                    },
                )) => {
                    request_id = next_id;
                    request = next;
                }
                _ => return,
            }
        }
    }
}
