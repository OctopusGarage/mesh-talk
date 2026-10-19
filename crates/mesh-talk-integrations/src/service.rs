//! Closed application API shared by trusted host adapters and local transports.
use crate::policy::{Grant, GrantRegistry, equal_hash, token_hash};
use crate::{Destination, GrantSpec, IntegrationError, SendMessage};
use mesh_talk_core::node::{MessageAcceptance, Node, NodeError, OutgoingMessageRequest};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
};

pub type HostFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, IntegrationError>> + Send + 'a>>;
pub type HostOperation<'a> = Box<dyn FnOnce(&Node) -> Result<Value, IntegrationError> + Send + 'a>;
/// Encloses a synchronous core commit in a current grant. Hosts additionally
/// enclose this callback in their own session/profile authority.
pub trait CommitAuthorization: Send + Sync {
    fn commit(&self, operation: &mut dyn FnMut() -> Result<(), NodeError>)
    -> Result<(), NodeError>;
}
pub trait IntegrationHost: Send + Sync {
    fn account_id(&self) -> &str;
    fn send<'a>(
        &'a self,
        request: OutgoingMessageRequest,
        authorization: Arc<dyn CommitAuthorization>,
    ) -> HostFuture<'a, MessageAcceptance>;
    fn with_node<'a>(&'a self, operation: HostOperation<'a>) -> HostFuture<'a, Value>;
    fn accept_invite(
        &self,
        _channel: mesh_talk_core::eventlog::event::ConversationId,
    ) -> HostFuture<'_, Value> {
        Box::pin(async {
            Err(IntegrationError::new(
                "UNSUPPORTED",
                "This host does not support bot enrollment",
            ))
        })
    }
}
struct GrantAuthorization {
    registry: Arc<Mutex<GrantRegistry>>,
    token: String,
    destination: Option<Destination>,
    describe: bool,
}
impl CommitAuthorization for GrantAuthorization {
    fn commit(
        &self,
        operation: &mut dyn FnMut() -> Result<(), NodeError>,
    ) -> Result<(), NodeError> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| NodeError::Authorization("grant store unavailable".into()))?;
        registry
            .authenticate(&self.token, self.destination.as_ref(), self.describe)
            .map_err(|_| NodeError::Authorization("grant revoked or expired".into()))?;
        operation()
    }
}
pub struct IntegrationService {
    host: Arc<dyn IntegrationHost>,
    registry: Arc<Mutex<GrantRegistry>>,
    admin_hash: [u8; 32],
    pending: tokio::sync::Semaphore,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StatusRequest {
    idempotency_key: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RevokeRequest {
    grant_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AcceptInvite {
    channel_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PageRequest {
    #[serde(default)]
    offset: usize,
    #[serde(default = "default_page_limit")]
    limit: usize,
}
fn default_page_limit() -> usize {
    20
}
impl PageRequest {
    fn parse(value: Value) -> Result<Self, IntegrationError> {
        let page: Self = parse(value)?;
        if !(1..=20).contains(&page.limit) {
            return Err(IntegrationError::new(
                "INVALID_INPUT",
                "Page limit must be between 1 and 20",
            ));
        }
        Ok(page)
    }
    fn next(&self, total: usize) -> Option<usize> {
        let next = self.offset.saturating_add(self.limit);
        (next < total).then_some(next)
    }
}
fn parse<T: serde::de::DeserializeOwned>(value: Value) -> Result<T, IntegrationError> {
    serde_json::from_value(value).map_err(|_| {
        IntegrationError::new("INVALID_INPUT", "Invalid integration command arguments")
    })
}
fn receipt(id: &str, destination: &Destination, acceptance: MessageAcceptance) -> Value {
    let kind = match destination {
        Destination::Account { .. } => "account_message",
        Destination::Channel { .. } => "channel_event",
    };
    json!({"operation_id":id,"message":{"kind":kind,"id":acceptance.message_id.to_hex()},"acceptance":"accepted","delivery":"pending_or_unknown","duplicate":acceptance.duplicate})
}
fn request_id(account: &str, client: &str, key: &str) -> Result<[u8; 32], IntegrationError> {
    if key.is_empty() || key.len() > 256 || key.chars().any(char::is_control) {
        return Err(IntegrationError::new(
            "INVALID_INPUT",
            "Retry key must contain 1–256 bytes without control characters",
        ));
    }
    let mut hash = Sha256::new();
    hash.update(b"mesh-talk-integration-request-v1");
    for value in [account, client, key] {
        hash.update((value.len() as u64).to_be_bytes());
        hash.update(value.as_bytes());
    }
    Ok(hash.finalize().into())
}
impl IntegrationService {
    pub fn new(host: Arc<dyn IntegrationHost>, grants: GrantRegistry, admin_token: &str) -> Self {
        Self {
            host,
            registry: Arc::new(Mutex::new(grants)),
            admin_hash: token_hash(admin_token),
            pending: tokio::sync::Semaphore::new(1000),
        }
    }
    fn authenticate(
        &self,
        token: &str,
        destination: Option<&Destination>,
        describe: bool,
    ) -> Result<Grant, IntegrationError> {
        self.registry
            .lock()
            .map_err(|_| IntegrationError::new("STORAGE_FAILURE", "Grant store unavailable"))?
            .authenticate(token, destination, describe)
    }
    fn authority(
        &self,
        token: &str,
        destination: Option<Destination>,
        describe: bool,
    ) -> Arc<dyn CommitAuthorization> {
        Arc::new(GrantAuthorization {
            registry: self.registry.clone(),
            token: token.into(),
            destination,
            describe,
        })
    }
    pub async fn handle(
        &self,
        token: &str,
        method: &str,
        params: Value,
    ) -> Result<Value, IntegrationError> {
        let _slot = self.pending.try_acquire().map_err(|_| {
            IntegrationError::new("CAPACITY_EXCEEDED", "Integration request capacity reached")
        })?;
        if token.len() != 64 {
            return Err(IntegrationError::new(
                "UNAUTHENTICATED",
                "Invalid integration credential",
            ));
        }
        if method.starts_with("grant_")
            || method == "accept_invite"
            || method == "admin_context"
            || method == "audit_list"
        {
            if !equal_hash(&token_hash(token), &self.admin_hash) {
                return Err(IntegrationError::new(
                    "SCOPE_DENIED",
                    "Administrative credential required",
                ));
            }
            return self.admin(method, params).await;
        }
        match method {
            "send_message" => {
                let command: SendMessage = parse(params)?;
                command.validate()?;
                let grant = self.authenticate(token, Some(&command.destination), false)?;
                self.registry
                    .lock()
                    .map_err(|_| {
                        IntegrationError::new("STORAGE_FAILURE", "Grant store unavailable")
                    })?
                    .charge_send(&grant)?;
                let id = request_id(
                    self.host.account_id(),
                    &grant.client_id,
                    &command.idempotency_key,
                )?;
                let request = OutgoingMessageRequest {
                    request_id: id,
                    destination: command.destination.core()?,
                    text: command.text.as_bytes().to_vec(),
                    reply_to: command.reply()?,
                    expires_at_ms: command.expires_at_ms,
                };
                let accepted = self
                    .host
                    .send(
                        request,
                        self.authority(token, Some(command.destination.clone()), false),
                    )
                    .await?;
                // A failed audit write cannot turn durable acceptance into a
                // failed send. The core mapping remains the retry authority.
                let registry = self.registry.clone();
                let client_id = grant.client_id.clone();
                let destination = command.destination.clone();
                let message_id = accepted.message_id.to_hex();
                let duplicate = accepted.duplicate;
                let audit = self
                    .host
                    .with_node(Box::new(move |_| {
                        registry
                            .lock()
                            .map_err(|_| {
                                IntegrationError::new("STORAGE_FAILURE", "Grant store unavailable")
                            })?
                            .record_send(
                                &client_id,
                                &destination,
                                hex::encode(id),
                                message_id,
                                duplicate,
                            )?;
                        Ok(Value::Null)
                    }))
                    .await;
                if let Err(error) = audit {
                    log::warn!(
                        "Integration audit persistence failed after acceptance: {}",
                        error.code
                    );
                }
                Ok(receipt(&hex::encode(id), &command.destination, accepted))
            }
            "get_send_result" => {
                let command: StatusRequest = parse(params)?;
                let grant = self.authenticate(token, None, false)?;
                let id = request_id(
                    self.host.account_id(),
                    &grant.client_id,
                    &command.idempotency_key,
                )?;
                let authority = self.authority(token, None, false);
                self.host.with_node(Box::new(move |node| {
                    // Read core snapshots before taking the grant mutex. Core send
                    // acceptance acquires the delivery lock before grant admission.
                    let destination = node.message_request_destination(&id)?;
                    let result = node.message_request_status(&id)?;
                    authority.commit(&mut || {
                        if let Some(destination) = &destination {
                            let destination = match destination {
                                mesh_talk_core::node::MessageDestination::Account(id) => Destination::Account { id: id.clone() },
                                mesh_talk_core::node::MessageDestination::Channel(id) => Destination::Channel { id: hex::encode(id.as_bytes()) },
                            };
                            if !grant.destinations.contains(&destination) {
                                return Err(NodeError::Authorization("destination no longer granted".into()));
                            }
                        }
                        Ok(())
                    })?;
                    Ok(match result {
                        Some(acceptance) => json!({"operation_id":hex::encode(id),"message_id":acceptance.message_id.to_hex(),"acceptance":"accepted","delivery":"pending_or_unknown"}),
                        None => json!({"operation_id":hex::encode(id),"acceptance":"not_accepted_or_outside_retry_window"}),
                    })
                })).await
            }
            "context" | "list_conversations" => {
                if params != json!({}) {
                    return Err(IntegrationError::new(
                        "INVALID_INPUT",
                        "This command takes no arguments",
                    ));
                }
                let describe = method == "list_conversations";
                let grant = self.authenticate(token, None, describe)?;
                let authority = self.authority(token, None, describe);
                let account_id = self.host.account_id().to_owned();
                self.host.with_node(Box::new(move |node| {
                    let value = if describe { json!({"conversations": describe_destinations(node, &grant.destinations)}) }
                        else { json!({"principal":{"kind":"account","account_id":account_id},"client_id":grant.client_id,"destinations":grant.destinations,"can_describe":grant.describe,"expires_at_ms":grant.expires_at_ms,"max_text_bytes":crate::MAX_TEXT_BYTES,"retry_window_days":7,"content_types":["text"],"read_access":false}) };
                    authority.commit(&mut || Ok(()))?;
                    Ok(value)
                })).await
            }
            _ => Err(IntegrationError::new(
                "METHOD_NOT_FOUND",
                "Unknown integration operation",
            )),
        }
    }
    async fn admin(&self, method: &str, params: Value) -> Result<Value, IntegrationError> {
        let registry = self.registry.clone();
        match method {
            "grant_create" => {
                let spec: GrantSpec = parse(params)?;
                self.host
                    .with_node(Box::new(move |_| {
                        let issued = registry
                            .lock()
                            .map_err(|_| {
                                IntegrationError::new("STORAGE_FAILURE", "Grant store unavailable")
                            })?
                            .create(spec)?;
                        // Credential is returned only to the private admin channel.
                        Ok(json!({"grant":public_grant(&issued.grant),"token":issued.token}))
                    }))
                    .await
            }
            "grant_revoke" => {
                let command: RevokeRequest = parse(params)?;
                self.host
                    .with_node(Box::new(move |_| {
                        registry
                            .lock()
                            .map_err(|_| {
                                IntegrationError::new("STORAGE_FAILURE", "Grant store unavailable")
                            })?
                            .revoke(&command.grant_id)?;
                        Ok(json!({"revoked":true}))
                    }))
                    .await
            }
            "grant_list" | "admin_context" | "audit_list" => {
                let page = PageRequest::parse(params)?;
                let context = method == "admin_context";
                let audit = method == "audit_list";
                self.host.with_node(Box::new(move |node| {
                    if context {
                        let mut channels = node.list_channels();
                        channels.sort_by_key(|channel| *channel.id.as_bytes());
                        return Ok(json!({"account_id":node.account_id(),"device_id":node.user_id(),"channels":channels.iter().skip(page.offset).take(page.limit).map(|channel| json!({"id":hex::encode(channel.id.as_bytes()),"name":short_name(&channel.name),"pending_invite":channel.pending_invite})).collect::<Vec<_>>(),"next_offset":page.next(channels.len())}));
                    }
                    if audit {
                        let audit = registry.lock().map_err(|_| IntegrationError::new("STORAGE_FAILURE", "Grant store unavailable"))?.audit()?;
                        return Ok(json!({"entries":audit.iter().rev().skip(page.offset).take(page.limit).collect::<Vec<_>>(),"next_offset":page.next(audit.len())}));
                    }
                    let grants = registry.lock().map_err(|_| IntegrationError::new("STORAGE_FAILURE", "Grant store unavailable"))?.list()?;
                    Ok(json!({"grants":grants.iter().skip(page.offset).take(page.limit).map(public_grant).collect::<Vec<_>>(),"next_offset":page.next(grants.len())}))
                })).await
            }
            "accept_invite" => {
                let command: AcceptInvite = parse(params)?;
                self.host
                    .accept_invite(mesh_talk_core::eventlog::event::ConversationId::new(
                        crate::model::decode_id(&command.channel_id)?,
                    ))
                    .await
            }
            _ => Err(IntegrationError::new(
                "METHOD_NOT_FOUND",
                "Unknown administrative operation",
            )),
        }
    }
}
fn public_grant(grant: &Grant) -> Value {
    json!({"id":grant.id,"client_id":grant.client_id,"destinations":grant.destinations,"describe":grant.describe,"expires_at_ms":grant.expires_at_ms})
}
fn short_name(name: &str) -> String {
    name.chars().take(160).collect()
}
fn describe_destinations(node: &Node, destinations: &[Destination]) -> Vec<Value> {
    let channels = node.list_channels();
    destinations.iter().map(|destination| match destination {
        Destination::Account { id } => json!({"destination":destination,"name":id}),
        Destination::Channel { id } => json!({"destination":destination,"name":short_name(channels.iter().find(|channel| hex::encode(channel.id.as_bytes()) == *id).map(|channel| channel.name.as_str()).unwrap_or(id))}),
    }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        client::{ClientConfig, LocalClient},
        host::HeadlessHost,
        ipc::LocalServer,
        policy::{new_token, now_ms},
    };
    use mesh_talk_core::{discovery::Roster, identity::device::DeviceIdentity};

    fn test_node(path: &std::path::Path) -> Arc<Node> {
        let (dm, _) = tokio::sync::mpsc::unbounded_channel();
        let (channel, _) = tokio::sync::mpsc::unbounded_channel();
        let (file, _) = tokio::sync::mpsc::unbounded_channel();
        Node::open(
            DeviceIdentity::generate(),
            Arc::new(Mutex::new(Roster::default())),
            dm,
            channel,
            file,
            &path.join("events"),
            &path.join("sent"),
            "pw",
        )
        .unwrap()
    }
    fn fixture(path: &std::path::Path, node: Arc<Node>, admin: &str) -> Arc<IntegrationService> {
        let grants = GrantRegistry::open(&path.join("grants"), "pw", &node.account_id()).unwrap();
        Arc::new(IntegrationService::new(
            Arc::new(HeadlessHost::new(node)),
            grants,
            admin,
        ))
    }
    async fn issue(
        service: &IntegrationService,
        admin: &str,
        client: &str,
        destination: &Destination,
        describe: bool,
    ) -> Value {
        service.handle(admin, "grant_create", json!({"client_id":client,"destinations":[destination],"describe":describe,"expires_at_ms":now_ms()+60000})).await.unwrap()
    }
    #[tokio::test]
    async fn scoped_group_send_retries_and_narrowed_status_are_enforced() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(dir.path());
        let channel = node.create_channel("Alerts", vec![]).await.unwrap();
        let other = node.create_channel("Private", vec![]).await.unwrap();
        let destination = Destination::Channel {
            id: hex::encode(channel.as_bytes()),
        };
        let admin = new_token();
        let service = fixture(dir.path(), node.clone(), &admin);
        let grant = issue(&service, &admin, "build-monitor", &destination, false).await;
        let token = grant["token"].as_str().unwrap();
        assert_eq!(
            service
                .handle(token, "list_conversations", json!({}))
                .await
                .unwrap_err()
                .code,
            "SCOPE_DENIED"
        );
        assert_eq!(
            service
                .handle(token, "grant_list", json!({}))
                .await
                .unwrap_err()
                .code,
            "SCOPE_DENIED"
        );
        let command =
            json!({"destination":destination,"text":"Build finished","idempotency_key":"build:42"});
        let first = service
            .handle(token, "send_message", command.clone())
            .await
            .unwrap();
        let retry = service
            .handle(token, "send_message", command.clone())
            .await
            .unwrap();
        assert_eq!(first["message"], retry["message"]);
        assert_eq!(retry["duplicate"], true);
        assert_eq!(node.channel_history(channel, 10).len(), 1);
        let mut changed = command.clone();
        changed["text"] = json!("Changed alert");
        assert_eq!(
            service
                .handle(token, "send_message", changed)
                .await
                .unwrap_err()
                .code,
            "IDEMPOTENCY_CONFLICT"
        );
        let narrowed = issue(
            &service,
            &admin,
            "build-monitor",
            &Destination::Channel {
                id: hex::encode(other.as_bytes()),
            },
            false,
        )
        .await;
        assert_eq!(
            service
                .handle(
                    narrowed["token"].as_str().unwrap(),
                    "get_send_result",
                    json!({"idempotency_key":"build:42"})
                )
                .await
                .unwrap_err()
                .code,
            "SCOPE_DENIED"
        );
        let status = service
            .handle(
                token,
                "get_send_result",
                json!({"idempotency_key":"build:42"}),
            )
            .await
            .unwrap();
        assert_eq!(status["message_id"], first["message"]["id"]);
        let audit = service
            .handle(&admin, "audit_list", json!({}))
            .await
            .unwrap();
        assert!(
            audit["entries"]
                .as_array()
                .unwrap()
                .iter()
                .any(|entry| entry["action"] == "message.retry")
        );
        let encoded = serde_json::to_string(&audit).unwrap();
        assert!(!encoded.contains("Build finished"));
        assert!(!encoded.contains(token));
    }
    #[tokio::test]
    async fn administrative_lists_stay_within_ipc_frames_and_page_all_grants() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(dir.path());
        let admin = new_token();
        let service = fixture(dir.path(), node, &admin);
        let destinations: Vec<_> = (1..=16)
            .map(|id| Destination::Channel {
                id: format!("{id:064x}"),
            })
            .collect();
        for id in 0..21 {
            service.handle(&admin,"grant_create",json!({"client_id":format!("agent-{id}"),"destinations":destinations,"describe":false,"expires_at_ms":now_ms()+60000})).await.unwrap();
        }
        let first = service
            .handle(&admin, "grant_list", json!({"offset":0,"limit":20}))
            .await
            .unwrap();
        assert_eq!(first["grants"].as_array().unwrap().len(), 20);
        assert_eq!(first["next_offset"], 20);
        assert!(serde_json::to_vec(&first).unwrap().len() < 64 * 1024);
        assert!(first["grants"][0].get("token_hash").is_none());
        let last = service
            .handle(&admin, "grant_list", json!({"offset":20,"limit":20}))
            .await
            .unwrap();
        assert_eq!(last["grants"].as_array().unwrap().len(), 1);
        assert!(last["next_offset"].is_null());
        assert_eq!(
            service
                .handle(&admin, "grant_list", json!({"limit":21}))
                .await
                .unwrap_err()
                .code,
            "INVALID_INPUT"
        );
    }
    #[tokio::test]
    async fn revocation_invalidates_an_already_captured_commit_authority() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(dir.path());
        let admin = new_token();
        let service = fixture(dir.path(), node, &admin);
        let destination = Destination::Channel {
            id: "cd".repeat(32),
        };
        let issued = issue(&service, &admin, "agent", &destination, false).await;
        let authority =
            service.authority(issued["token"].as_str().unwrap(), Some(destination), false);
        service
            .handle(
                &admin,
                "grant_revoke",
                json!({"grant_id":issued["grant"]["id"]}),
            )
            .await
            .unwrap();
        let mut mutated = false;
        assert!(
            authority
                .commit(&mut || {
                    mutated = true;
                    Ok(())
                })
                .is_err()
        );
        assert!(!mutated);
        assert_eq!(
            service
                .handle(issued["token"].as_str().unwrap(), "context", json!({}))
                .await
                .unwrap_err()
                .code,
            "UNAUTHENTICATED"
        );
    }
    #[tokio::test]
    async fn real_local_client_checks_grants_and_shutdown_closes_incomplete_frames() {
        let dir = tempfile::tempdir().unwrap();
        let node = test_node(dir.path());
        let admin = new_token();
        let service = fixture(dir.path(), node, &admin);
        let issued = issue(
            &service,
            &admin,
            "agent",
            &Destination::Channel {
                id: "cd".repeat(32),
            },
            true,
        )
        .await;
        let server = LocalServer::bind(&dir.path().join("ipc")).await.unwrap();
        let endpoint = server.endpoint();
        let client = LocalClient::new(ClientConfig {
            endpoint: endpoint.clone(),
            token: issued["token"].as_str().unwrap().into(),
        });
        let (shutdown, stop) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(server.run_until(service, async {
            let _ = stop.await;
        }));
        assert_eq!(
            client.call("context", json!({})).await.unwrap()["read_access"],
            false
        );
        let invalid = LocalClient::new(ClientConfig {
            endpoint: endpoint.clone(),
            token: new_token(),
        });
        assert_eq!(
            invalid.call("context", json!({})).await.unwrap_err().code,
            "UNAUTHENTICATED"
        );
        #[cfg(unix)]
        let _stalled = tokio::net::UnixStream::connect(&endpoint).await.unwrap();
        shutdown.send(()).unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        assert!(client.call("context", json!({})).await.is_err());
    }
}
