//! Real signed discovery and encrypted socket delivery, coupled to registered policy IPC.
//! No roster insertion, fake received event, or transport mock is used here.
use super::*;
use mesh_talk_core::node::{NodeRuntime, ReceivedDm};
use std::net::UdpSocket;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver};
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
    *nodes.0.blocking_lock() = Some(alice);
    let app = tauri::test::mock_builder()
        .manage(state.clone())
        .manage(nodes.clone())
        .manage(HiddenContactsState::new(root.path().to_owned()))
        .invoke_handler(tauri::generate_handler![
            get_hidden_contacts,
            set_contact_hidden,
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
    let hidden = invoke(
        "set_contact_hidden",
        serde_json::json!({
            "owner": "alice", "account": bob_account, "hidden": true, "name": "Bob"
        }),
    )
    .unwrap();
    assert_eq!(hidden.contacts.len(), 1);
    assert_eq!(hidden.contacts[0].account_id, bob_account);
    assert!(nodes
        .0
        .blocking_lock()
        .as_ref()
        .unwrap()
        .peer_public(bob.user_id())
        .is_some());
    let peers_after_hide: Vec<_> = nodes
        .0
        .blocking_lock()
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
        let guard = nodes.0.blocking_lock();
        let alice = guard.as_ref().unwrap();
        announce(alice, c_port);
        announce(&bob_two, a_port);
    }
    until("new device authentic discovery", || {
        nodes
            .0
            .blocking_lock()
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

    // Drop every handle before opening stores again, exactly as logout/relaunch does.
    let before_restart = alice_handle.account_history(&bob_account, 20);
    let old_node = Arc::downgrade(&alice_handle);
    nodes.0.blocking_lock().take();
    drop(alice_handle);
    until("old runtime tasks release their node and stores", || {
        old_node.upgrade().is_none()
    });
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
    drop(restarted);
    // Check isolation while Alice's policy is nonempty, not after restoring it.
    let mut other = alice_user.clone();
    other.user_id = "other-local-user".into();
    session.set("other-token".into(), other, "fixture-password".into());
    assert!(invoke(
        "get_hidden_contacts",
        serde_json::json!({ "owner": "alice" })
    )
    .is_err());
    assert!(invoke(
        "get_hidden_contacts",
        serde_json::json!({ "owner": "other-local-user" })
    )
    .unwrap()
    .contacts
    .is_empty());
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
    assert!(HiddenContactsState::new(root.path().to_owned())
        .get(Some("alice"), "alice")
        .unwrap()
        .contacts
        .is_empty());
}
