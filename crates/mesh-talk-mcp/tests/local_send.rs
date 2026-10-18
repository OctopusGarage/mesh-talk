//! Exercise the real MCP → private IPC → grant → core acceptance boundary.
use mesh_talk_core::{discovery::Roster, identity::device::DeviceIdentity, node::Node};
use mesh_talk_integrations::{
    Destination, GrantSpec, IntegrationService,
    client::{ClientConfig, LocalClient},
    host::HeadlessHost,
    ipc::LocalServer,
    policy::{GrantRegistry, new_token, now_ms},
};
use mesh_talk_mcp::MessagingServer;
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, ServiceExt,
    model::{CallToolRequestParams, ProtocolVersion},
};
use serde_json::{Value, json};
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

fn fingerprint(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut encoded = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(encoded, "{byte:02x}").unwrap();
    }
    encoded
}

fn arguments(value: Value) -> serde_json::Map<String, Value> {
    value
        .as_object()
        .expect("tool arguments are an object")
        .clone()
}

#[tokio::test]
async fn scoped_mcp_send_reaches_real_group_once_without_read_permission() {
    tokio::time::timeout(Duration::from_secs(20), exercise_local_send())
        .await
        .expect("local messaging composition completes without external peers");
}

async fn exercise_local_send() {
    let directory = tempfile::tempdir().unwrap();
    let (dm, _messages) = tokio::sync::mpsc::unbounded_channel();
    let (channel, _channel_messages) = tokio::sync::mpsc::unbounded_channel();
    let (file, _files) = tokio::sync::mpsc::unbounded_channel();
    let node = Node::open(
        DeviceIdentity::generate(),
        Arc::new(Mutex::new(Roster::default())),
        dm,
        channel,
        file,
        &directory.path().join("events.log"),
        &directory.path().join("sent.log"),
        "test-password",
    )
    .unwrap();
    let group = node
        .create_channel("Local bot alerts", vec![])
        .await
        .unwrap();
    let private_group = node.create_channel("Not granted", vec![]).await.unwrap();
    let destination = Destination::Channel {
        id: fingerprint(group.as_bytes()),
    };
    let mut registry = GrantRegistry::open(
        &directory.path().join("grants.log"),
        "test-password",
        &node.account_id(),
    )
    .unwrap();
    let issued = registry
        .create(GrantSpec {
            client_id: "local-sdk-agent".into(),
            destinations: vec![destination.clone()],
            describe: false,
            expires_at_ms: now_ms() + 60_000,
        })
        .unwrap();
    let host = Arc::new(HeadlessHost::new(node.clone()));
    let service = Arc::new(IntegrationService::new(
        host.clone(),
        registry,
        &new_token(),
    ));
    let server = LocalServer::bind(&directory.path().join("ipc"))
        .await
        .unwrap();
    let local = LocalClient::new(ClientConfig {
        endpoint: server.endpoint(),
        token: issued.token,
    });
    let (ipc_shutdown, ipc_stopped) = tokio::sync::oneshot::channel();
    let ipc_task = tokio::spawn(server.run_until(service, async {
        let _ = ipc_stopped.await;
    }));
    let (server_io, client_io) = tokio::io::duplex(64 * 1024);
    let mcp_task = tokio::spawn(async move {
        MessagingServer::new(local)
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let client = rmcp::model::ClientConfig::default()
        .serve_with_lifecycle(
            client_io,
            ClientLifecycleMode::Discover {
                preferred_versions: vec![ProtocolVersion::V_2026_07_28],
            },
        )
        .await
        .unwrap();
    assert_eq!(
        client.peer_info().unwrap().protocol_version,
        ProtocolVersion::V_2026_07_28
    );
    let tools = client.list_all_tools().await.unwrap();
    assert!(
        tools
            .iter()
            .any(|tool| tool.name == "mesh_talk_send_message")
    );
    assert!(
        !tools
            .iter()
            .any(|tool| tool.name.contains("read") || tool.name.contains("history"))
    );
    let context = client
        .call_tool(CallToolRequestParams::new("mesh_talk_get_context"))
        .await
        .unwrap();
    let context = context.structured_content.unwrap();
    assert_eq!(context["read_access"], false);
    assert_eq!(context["can_describe"], false);
    assert_eq!(context["destinations"], json!([destination]));

    let send = json!({
        "destination": destination, "text": "Build completed through local MCP",
        "idempotency_key": "build:local:1",
    });
    let first = client
        .call_tool(
            CallToolRequestParams::new("mesh_talk_send_message")
                .with_arguments(arguments(send.clone())),
        )
        .await
        .unwrap();
    assert_ne!(first.is_error, Some(true));
    let first = first.structured_content.unwrap();
    assert_eq!(first["acceptance"], "accepted");
    assert_eq!(first["duplicate"], false);
    let history = node.channel_history(group, 10);
    assert_eq!(history.len(), 1);
    assert_eq!(history[0].text, b"Build completed through local MCP");
    assert_eq!(first["message"]["id"], history[0].id.to_hex());

    let retry = client
        .call_tool(
            CallToolRequestParams::new("mesh_talk_send_message")
                .with_arguments(arguments(send.clone())),
        )
        .await
        .unwrap();
    assert_ne!(retry.is_error, Some(true));
    let retry = retry.structured_content.unwrap();
    assert_eq!(retry["message"]["id"], first["message"]["id"]);
    assert_eq!(retry["duplicate"], true);
    assert_eq!(node.channel_history(group, 10).len(), 1);

    let mut denied = send;
    denied["destination"] = json!({"kind":"channel", "id":fingerprint(private_group.as_bytes())});
    denied["idempotency_key"] = json!("build:private:1");
    let denied = client
        .call_tool(
            CallToolRequestParams::new("mesh_talk_send_message").with_arguments(arguments(denied)),
        )
        .await
        .unwrap();
    assert_eq!(denied.is_error, Some(true));
    assert_eq!(
        denied.structured_content.unwrap()["error"]["code"],
        "SCOPE_DENIED"
    );
    assert!(node.channel_history(private_group, 10).is_empty());
    assert!(
        client
            .call_tool(CallToolRequestParams::new("mesh_talk_read_messages"))
            .await
            .is_err()
    );
    let descriptions = client
        .call_tool(CallToolRequestParams::new("mesh_talk_list_conversations"))
        .await
        .unwrap();
    assert_eq!(descriptions.is_error, Some(true));
    assert_eq!(
        descriptions.structured_content.unwrap()["error"]["code"],
        "SCOPE_DENIED"
    );

    client.cancel().await.unwrap();
    mcp_task.await.unwrap();
    ipc_shutdown.send(()).unwrap();
    ipc_task.await.unwrap().unwrap();
    host.retire().await;
}
