//! Real signed discovery and encrypted socket delivery, coupled to registered policy IPC.
//! No roster insertion, fake received event, or transport mock is used here.
use super::*;
use mesh_talk_core::node::{NodeRuntime, ReceivedDm};
use std::net::UdpSocket;
use std::path::Path;
use std::sync::mpsc::{Receiver, channel};
use std::time::{Duration, Instant};

fn start(root: &Path, owner: &str, name: &str, port: u16) -> (NodeRuntime, Receiver<ReceivedDm>) {
    let (tx, rx) = channel();
    let runtime = tauri::async_runtime::block_on(NodeRuntime::start(
        root,
        owner,
        name,
        "fixture-password",
        port,
        move |dm| {
            let _ = tx.send(dm);
        },
        |_| {},
        |_| {},
        |_| {},
        |_| {},
    ))
    .unwrap();
    (runtime, rx)
}

fn until(description: &str, check: impl Fn() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while !check() {
        assert!(Instant::now() < deadline, "Timed out: {description}");
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn announce(sender: &NodeRuntime, receiver_port: u16) {
    let announcement = sender
        .handle()
        .signed_announce(sender.display_name(), sender.listen_tcp_port());
    assert!(
        announcement.verify(),
        "Fixture must use an authentic signed announcement"
    );
    let bytes = mesh_talk_core::discovery::announce::encode(&announcement);
    UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .send_to(&bytes, (std::net::Ipv4Addr::LOCALHOST, receiver_port))
        .unwrap();
}

fn send(sender: &mesh_talk_core::node::Node, account: &str, text: &[u8]) {
    tauri::async_runtime::block_on(async {
        tokio::time::timeout(
            Duration::from_secs(15),
            sender.send_to_account(account, text, None),
        )
        .await
        .expect("Timed out during encrypted send")
        .expect("Encrypted send failed");
    });
}

#[test]
fn registered_hide_keeps_real_multi_device_delivery_and_restart_history() {
    let root = tempfile::tempdir().unwrap();
    // Reserve distinct ports together, then release them for the actual discovery loops.
    let a_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let b_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let c_socket = UdpSocket::bind("127.0.0.1:0").unwrap();
    let (a_port, b_port, c_port) = (
        a_socket.local_addr().unwrap().port(),
        b_socket.local_addr().unwrap().port(),
        c_socket.local_addr().unwrap().port(),
    );
    drop((a_socket, b_socket, c_socket));
    let (alice, alice_rx) = start(root.path(), "alice", "Alice", a_port);
    let (bob, bob_rx) = start(root.path(), "bob", "Bob", b_port);
    let alice_account = alice.account_id().to_owned();
    let bob_account = bob.account_id().to_owned();
    let alice_handle = alice.handle();
    let bob_handle = bob.handle();
    announce(&alice, b_port);
    announce(&bob, a_port);
    until("mutual signed discovery", || {
        alice.peer_public(bob.user_id()).is_some() && bob.peer_public(alice.user_id()).is_some()
    });
    send(
        &bob_handle,
        &alice_account,
        b"existing conversation before hide",
    );
    assert_eq!(
        alice_rx.recv_timeout(Duration::from_secs(15)).unwrap().text,
        b"existing conversation before hide"
    );
    let history_before_hide = alice_handle.account_history(&bob_account, 20);
    let peers_before_hide: Vec<_> = alice
        .peers()
        .iter()
        .map(|peer| (peer.public.user_id(), peer.account_id.clone()))
        .collect();

    let auth = crate::services::auth_service::AuthService::new(Arc::new(
        mesh_talk_core::identity::manager::IdentityManager::new(
            mesh_talk_core::storage::file_manager::FileManager::new(root.path().to_owned()),
        ),
    ));
    let state = crate::state::AppState::new(auth);
    let session = state.session();
    let alice_user = crate::services::user::User {
        user_id: "alice".into(),
        account_login_name: None,
        name: "alice".into(),
        display_name: "Alice".into(),
        address: String::new(),
        created_at: 0,
        last_seen: 0,
        is_online: true,
    };
    session.set(
        "fixture-token".into(),
        alice_user.clone(),
        "fixture-password".into(),
    );
    let nodes = crate::chat_commands::NodeState::empty();
    *nodes.0.blocking_write() = Some(alice);
    let app = tauri::test::mock_builder()
        .manage(state.clone())
        .manage(nodes.clone())
        .manage(HiddenContactsState::new(root.path().to_owned()))
        .invoke_handler(tauri::generate_handler![
            get_hidden_contacts,
            set_contact_hidden,
            delete_contact,
            crate::owner_commands::owner_personal_conversation_states,
            crate::owner_commands::owner_delete_left_channel,
            crate::chat_commands::list_channels,
            crate::chat_commands::search
        ])
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap();
    let webview = tauri::WebviewWindowBuilder::new(&app, "main", Default::default())
        .build()
        .unwrap();
    let invoke_raw = |command: &str, body: serde_json::Value| {
        tauri::test::get_ipc_response(
            &webview,
            tauri::webview::InvokeRequest {
                cmd: command.into(),
                callback: tauri::ipc::CallbackFn(0),
                error: tauri::ipc::CallbackFn(1),
                url: if cfg!(windows) {
                    "http://tauri.localhost"
                } else {
                    "tauri://localhost"
                }
                .parse()
                .unwrap(),
                body: tauri::ipc::InvokeBody::Json(body),
                headers: Default::default(),
                invoke_key: tauri::test::INVOKE_KEY.into(),
            },
        )
    };
    let invoke = |command: &str, body: serde_json::Value| {
        invoke_raw(command, body).map(|body| body.deserialize::<HiddenContactsSnapshot>().unwrap())
    };
    let hits = invoke_raw(
        "search",
        serde_json::json!({ "query": "existing conversation before hide" }),
    )
    .unwrap()
    .deserialize::<serde_json::Value>()
    .unwrap();
    assert!(hits.as_array().unwrap().iter().any(|hit| {
        hit["account_id"] == bob_account
            && hit["text"] == "existing conversation before hide"
            && hit["is_channel"] == false
    }));
    // Online deletion with explicit keep-history is a real core personal
    // control, independent from presence and from manual contact hiding.
    let deleted_online = invoke(
        "delete_contact",
        serde_json::json!({
            "owner":"alice", "expectedAccount":alice_account, "account":bob_account, "clearHistory":false,
        }),
    )
    .unwrap();
    assert_eq!(deleted_online.deleted, vec![bob_account.clone()]);
    let personal_rows = invoke_raw(
        "owner_personal_conversation_states",
        serde_json::json!({"owner":"alice", "expectedAccount":alice_account}),
    )
    .unwrap()
    .deserialize::<serde_json::Value>()
    .unwrap();
    let personal_row = personal_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["conversation"]["id"] == bob_account)
        .unwrap();
    assert_eq!(personal_row["conversation"]["kind"], "account");
    assert_eq!(personal_row["hidden"], true);
    assert!(personal_row["history_floor"].is_null());
    assert_eq!(personal_row["local_cleanup_complete"], true);
    assert_eq!(
        alice_handle.account_history(&bob_account, 20),
        history_before_hide
    );
    announce(&bob, a_port);
    assert_eq!(
        invoke("get_hidden_contacts", serde_json::json!({"owner":"alice"}))
            .unwrap()
            .deleted,
        vec![bob_account.clone()],
        "signed presence must not reopen a personal deletion"
    );
    assert!(
        invoke(
            "set_contact_hidden",
            serde_json::json!({
                "owner":"alice", "account":bob_account, "hidden":false, "name":"",
            })
        )
        .is_err(),
        "a shared reopen requires a caller-captured root"
    );
    assert!(invoke("set_contact_hidden", serde_json::json!({
        "owner":"alice", "expectedAccount":"00".repeat(16), "account":bob_account, "hidden":false, "name":"",
    })).is_err(), "a stale-root restore cannot mutate this account");
    assert!(
        alice_handle
            .conversation_personal_state(&mesh_talk_core::node::DocumentConversation::Account(
                bob_account.clone()
            ))
            .hidden
    );
    assert_eq!(
        alice_handle.account_history(&bob_account, 20),
        history_before_hide
    );
    let restored_personal = invoke(
        "set_contact_hidden",
        serde_json::json!({
            "owner":"alice", "expectedAccount":alice_account, "account":bob_account, "hidden":false, "name":"",
        }),
    )
    .unwrap();
    assert!(restored_personal.deleted.is_empty());
    assert_eq!(
        alice_handle.account_history(&bob_account, 20),
        history_before_hide
    );

    let hidden = invoke(
        "set_contact_hidden",
        serde_json::json!({
            "owner": "alice", "account": bob_account, "hidden": true, "name": "Bob"
        }),
    )
    .unwrap();
    assert_eq!(hidden.contacts.len(), 1);
    assert_eq!(hidden.contacts[0].account_id, bob_account);
    assert!(
        nodes
            .0
            .blocking_write()
            .as_ref()
            .unwrap()
            .peer_public(bob.user_id())
            .is_some()
    );
    let peers_after_hide: Vec<_> = nodes
        .0
        .blocking_write()
        .as_ref()
        .unwrap()
        .peers()
        .iter()
        .map(|peer| (peer.public.user_id(), peer.account_id.clone()))
        .collect();
    assert_eq!(
        peers_after_hide, peers_before_hide,
        "Hiding must not mutate the authentic raw roster"
    );
    assert_eq!(
        alice_handle.account_history(&bob_account, 20),
        history_before_hide,
        "Hiding must preserve the existing conversation"
    );

    // A newly added device has a separate identity but the same genuine account key.
    // This is fixture provisioning, not a mock certificate or a roster update.
    let second_dir = root.path().join("accounts/bob-device-two");
    std::fs::create_dir_all(&second_dir).unwrap();
    std::fs::copy(
        root.path().join("accounts/bob/account.keystore"),
        second_dir.join("account.keystore"),
    )
    .unwrap();
    let (bob_two, bob_two_rx) = start(root.path(), "bob-device-two", "Bob laptop", c_port);
    assert_eq!(bob_two.account_id(), bob_account);
    assert_ne!(bob_two.user_id(), bob.user_id());
    {
        let guard = nodes.0.blocking_write();
        let alice = guard.as_ref().unwrap();
        announce(alice, c_port);
        announce(&bob_two, a_port);
    }
    until("new device authentic discovery", || {
        nodes
            .0
            .blocking_write()
            .as_ref()
            .unwrap()
            .peer_public(bob_two.user_id())
            .is_some()
            && bob_two
                .peer_public(alice_handle.user_id().as_str())
                .is_some()
    });
    let policy = invoke(
        "get_hidden_contacts",
        serde_json::json!({ "owner": "alice" }),
    )
    .unwrap();
    assert_eq!(
        policy.contacts, hidden.contacts,
        "New devices do not change account-level policy"
    );

    send(&bob_handle, &alice_account, b"inbound while hidden");
    let received = alice_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    assert_eq!(received.from, bob.user_id());
    assert_eq!(received.text, b"inbound while hidden");
    send(&alice_handle, &bob_account, b"outbound while hidden");
    assert_eq!(
        bob_rx.recv_timeout(Duration::from_secs(15)).unwrap().text,
        b"outbound while hidden"
    );
    send(
        &bob_two.handle(),
        &alice_account,
        b"second device inbound while hidden",
    );
    let second_received = alice_rx.recv_timeout(Duration::from_secs(15)).unwrap();
    assert_eq!(second_received.from, bob_two.user_id());
    assert_eq!(second_received.text, b"second device inbound while hidden");
    assert_eq!(
        bob_two_rx
            .recv_timeout(Duration::from_secs(15))
            .unwrap()
            .text,
        b"outbound while hidden"
    );
    until("durable account history contains both directions", || {
        let history = alice_handle.account_history(&bob_account, 20);
        history
            .iter()
            .any(|e| !e.from_me && e.text == b"inbound while hidden")
            && history
                .iter()
                .any(|e| e.from_me && e.text == b"outbound while hidden")
    });

    // Standalone group removal is a real public-member departure followed by
    // guarded personal cleanup. Canonical membership remains available to core.
    let group = tauri::async_runtime::block_on(bob_handle.create_channel(
        "Personal removal native fixture",
        vec![bob.peer_public(&alice_handle.user_id()).unwrap()],
    ))
    .unwrap();
    until("real group membership reaches Alice", || {
        alice_handle
            .list_channels()
            .iter()
            .any(|row| row.id == group)
    });
    let group_id = hex::encode(group.as_bytes());
    let delete_body =
        serde_json::json!({"owner":"alice", "expectedAccount":alice_account, "channelId":group_id});
    assert!(
        invoke_raw("owner_delete_left_channel", delete_body.clone()).is_err(),
        "active membership must reject personal deletion"
    );
    tauri::async_runtime::block_on(
        bob_handle.send_channel_message(group, b"synthetic group history before personal deletion"),
    )
    .unwrap();
    until("original group message reaches Alice", || {
        !alice_handle.channel_history(group, 20).is_empty()
    });
    // Public access diagnostics contain only role, execution and signed scope
    // identifiers. Terminal public DTOs intentionally omit cleanup-only sources.
    let trace_access = |phase: &str| {
        eprintln!(
            "native personal group {phase} access={:?}",
            alice_handle.account_group_access(group)
        );
    };
    trace_access("before_leave");
    tauri::async_runtime::block_on(alice_handle.leave_channel(group)).unwrap();
    trace_access("after_leave");
    let deleted = invoke_raw("owner_delete_left_channel", delete_body);
    if deleted.is_err() {
        trace_access("delete_error");
    }
    assert!(
        deleted
            .unwrap()
            .deserialize::<serde_json::Value>()
            .unwrap()
            .is_null()
    );
    assert!(alice_handle.channel_history(group, 20).is_empty());
    assert!(
        alice_handle
            .list_channels()
            .iter()
            .any(|row| row.id == group),
        "raw core membership/proof inventory must remain intact"
    );
    let visible_groups = invoke_raw("list_channels", serde_json::json!({}))
        .unwrap()
        .deserialize::<serde_json::Value>()
        .unwrap();
    assert!(
        visible_groups
            .as_array()
            .unwrap()
            .iter()
            .all(|row| row["channel_id"] != group_id)
    );
    let personal_rows = invoke_raw(
        "owner_personal_conversation_states",
        serde_json::json!({"owner":"alice", "expectedAccount":alice_account}),
    )
    .unwrap()
    .deserialize::<serde_json::Value>()
    .unwrap();
    let group_row = personal_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| {
            row["conversation"]["kind"] == "channel" && row["conversation"]["id"] == group_id
        })
        .expect("snapshot must retain the closed group control");
    assert_eq!(group_row["hidden"], true);
    assert_eq!(group_row["local_cleanup_complete"], true);
    assert!(group_row["history_floor"].is_string());

    // Drop every handle before opening stores again, exactly as logout/relaunch does.
    let before_restart = alice_handle.account_history(&bob_account, 20);
    let old_node = Arc::downgrade(&alice_handle);
    nodes.0.blocking_write().take();
    drop(alice_handle);
    until("old runtime tasks release their node and stores", || {
        old_node.upgrade().is_none()
    });
    drop((bob, bob_two, bob_handle));
    let (restarted, _) = start(root.path(), "alice", "Alice", a_port);
    assert_eq!(restarted.account_id(), alice_account);
    assert_eq!(restarted.account_history(&bob_account, 20), before_restart);
    assert_eq!(
        HiddenContactsState::new(root.path().to_owned())
            .get(Some("alice"), "alice")
            .unwrap()
            .contacts,
        hidden.contacts
    );
    // Keep this offline runtime for the deletion assertion after offline restoration.
    // Check isolation while Alice's policy is nonempty, not after restoring it.
    let mut other = alice_user.clone();
    other.user_id = "other-local-user".into();
    session.set("other-token".into(), other, "fixture-password".into());
    assert!(
        invoke(
            "get_hidden_contacts",
            serde_json::json!({ "owner": "alice" })
        )
        .is_err()
    );
    assert!(
        invoke(
            "get_hidden_contacts",
            serde_json::json!({ "owner": "other-local-user" })
        )
        .unwrap()
        .contacts
        .is_empty()
    );
    assert_eq!(
        HiddenContactsState::new(root.path().to_owned())
            .get(Some("alice"), "alice")
            .unwrap()
            .contacts,
        hidden.contacts
    );
    session.set(
        "fixture-token".into(),
        alice_user,
        "fixture-password".into(),
    );
    // Offline restoration is real authenticated IPC, not dependent on an active node.
    let restored = invoke(
        "set_contact_hidden",
        serde_json::json!({
            "owner": "alice", "account": bob_account, "hidden": false, "name": ""
        }),
    )
    .unwrap();
    assert!(restored.contacts.is_empty());
    assert!(
        HiddenContactsState::new(root.path().to_owned())
            .get(Some("alice"), "alice")
            .unwrap()
            .contacts
            .is_empty()
    );
    assert!(crate::chat_commands::online_account_ids(restarted.peers()).is_empty());
    let deletion_handle = restarted.handle();
    let deletion_weak = Arc::downgrade(&deletion_handle);
    *nodes.0.blocking_write() = Some(restarted);
    let deleted = invoke(
        "delete_contact",
        serde_json::json!({ "owner": "alice", "expectedAccount":alice_account, "account": bob_account }),
    )
    .unwrap();
    assert_eq!(deleted.deleted, vec![bob_account.clone()]);
    assert!(deletion_handle.account_history(&bob_account, 20).is_empty());
    let personal_rows = invoke_raw(
        "owner_personal_conversation_states",
        serde_json::json!({"owner":"alice", "expectedAccount":alice_account}),
    )
    .unwrap()
    .deserialize::<serde_json::Value>()
    .unwrap();
    let personal_row = personal_rows
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["conversation"]["id"] == bob_account)
        .unwrap();
    assert_eq!(personal_row["hidden"], true);
    assert!(
        personal_row["history_floor"]
            .as_str()
            .is_some_and(|floor| floor.split(':').count() == 3)
    );
    assert_eq!(personal_row["local_cleanup_complete"], true);
    nodes.0.blocking_write().take();
    drop(deletion_handle);
    until("deleted runtime releases stores", || {
        deletion_weak.upgrade().is_none()
    });
    let (after_delete, _) = start(root.path(), "alice", "Alice", a_port);
    assert!(
        after_delete.account_history(&bob_account, 20).is_empty(),
        "deleted history must not return after restart"
    );
    assert!(
        HiddenContactsState::new(root.path().to_owned())
            .get(Some("alice"), "alice")
            .unwrap()
            .deleted
            .is_empty(),
        "new account controls must not leave an unscoped legacy suppression"
    );
    let personal = after_delete.handle().conversation_personal_state(
        &mesh_talk_core::node::DocumentConversation::Account(bob_account),
    );
    assert!(
        personal.known
            && personal.hidden
            && personal.clear_history
            && personal.local_cleanup_complete
    );
    assert!(personal.floor.is_some());
}
