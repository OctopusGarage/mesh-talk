//! Real custody evidence must not wait behind an acknowledged history response.
use super::*;
use crate::node::account_sync_wire::AccountHistoryFrame;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;
use tokio::sync::{Semaphore, oneshot};

struct ReleaseHistoryOnDrop(Arc<Semaphore>);
impl Drop for ReleaseHistoryOnDrop {
    fn drop(&mut self) {
        self.0.close();
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn external_custody_reaches_sibling_while_authenticated_history_is_held() {
    let dir = tempfile::tempdir().unwrap();
    let account = Account::generate();
    let source = node(&dir.path().join("source"), &account);
    let sibling = node(&dir.path().join("sibling"), &account);
    let external = node(&dir.path().join("external"), &Account::generate());
    let genesis = Event::new(
        &source.identity,
        new_channel_id(),
        1,
        vec![],
        1,
        1,
        EventKind::MembershipChange,
        ChannelMeta {
            name: "Held history custody".into(),
            members: vec![
                source.identity.public(),
                sibling.identity.public(),
                external.identity.public(),
            ],
            epoch: 0,
            owner: source.user_id(),
        }
        .encode(),
    );
    let group = genesis.conversation_id;
    for node in [&source, &sibling, &external] {
        install(node, &genesis);
        assert!(node.device_sync_policy().history);
    }
    link(&source, &sibling);
    link(&sibling, &source);
    let mut addresses = Vec::new();
    let mut listeners = Vec::new();
    for (name, node) in [
        ("source", &source),
        ("sibling", &sibling),
        ("external", &external),
    ] {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        configure(node, &dir.path().join(name), address.port());
        addresses.push(address);
        listeners.push(listener);
    }
    source.roster.lock().unwrap().update(
        &sibling.signed_announce("Custody sibling", addresses[1].port()),
        addresses[1].ip(),
        &source.user_id(),
    );
    sibling.roster.lock().unwrap().update(
        &source.signed_announce("Custody source", addresses[0].port()),
        addresses[0].ip(),
        &sibling.user_id(),
    );
    // No real external handshake/cache exists before the held history page.
    // The signed physical membership is real; only its advertised route is down.
    for node in [&source, &sibling] {
        node.roster.lock().unwrap().update(
            &external.signed_announce("Offline custody member", 9),
            addresses[2].ip(),
            &node.user_id(),
        );
    }
    let external_listener = listeners.pop().unwrap();
    let sibling_listener = listeners.pop().unwrap();
    let source_listener = listeners.pop().unwrap();
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn(source.clone().run_accept_loop(source_listener));
    servers.spawn(sibling.clone().run_accept_loop(sibling_listener));
    source
        .send_channel_message(group, b"original custody target")
        .await
        .unwrap();
    let event = source.channel_history(group, 10)[0].id;
    tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if sibling
                .channel_history(group, 10)
                .iter()
                .any(|row| row.id == event && row.from_me)
                && sibling.channel_custody_count(group, event) == Some(0)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the real outgoing copy, entitlement and own-sibling-only evidence must bootstrap");
    assert_eq!(source.channel_custody_count(group, event), Some(0));
    assert!(external.log.lock().unwrap().get(&event).is_none());

    let release = Arc::new(Semaphore::new(0));
    let _release_on_drop = ReleaseHistoryOnDrop(release.clone());
    let (arrived, held) = oneshot::channel();
    let arrived = Arc::new(Mutex::new(Some(arrived)));
    let history_requests = Arc::new(AtomicUsize::new(0));
    let exact_sibling = sibling.identity.public();
    *source.account_session_test_hook.lock().unwrap() = Some(Arc::new({
        let release = release.clone();
        let history_requests = history_requests.clone();
        move |peer, frame| {
            let release = release.clone();
            let arrived = arrived.clone();
            let history_requests = history_requests.clone();
            let exact_sibling = exact_sibling.clone();
            Box::pin(async move {
                if peer != exact_sibling || !matches!(frame, AccountHistoryFrame::Request { .. }) {
                    return;
                }
                history_requests.fetch_add(1, Ordering::SeqCst);
                let signal = arrived.lock().unwrap().take();
                if let Some(signal) = signal {
                    let _ = signal.send(());
                    // SessionReady was sent; only this history response waits.
                    let _ = release.acquire().await;
                }
            })
        }
    }));
    source.note_account_local_change();
    super::cold_account_setup(held)
        .await
        .expect("the next real authenticated History request must reach the gate")
        .unwrap();

    servers.spawn(external.clone().run_accept_loop(external_listener));
    source.roster.lock().unwrap().update(
        &external.signed_announce("Online custody member", addresses[2].port()),
        addresses[2].ip(),
        &source.user_id(),
    );
    external.roster.lock().unwrap().update(
        &source.signed_announce("Custody source", addresses[0].port()),
        addresses[0].ip(),
        &external.user_id(),
    );
    // Keep the sibling's external route unreachable: only a private document
    // from the original source may carry this actual external possession proof.
    let custody_key = crate::node::AccountDocumentKey::ChannelCustody {
        channel: hex::encode(group.as_bytes()),
        event: event.to_hex(),
        holder: external.user_id(),
    };
    let converged = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if source.channel_custody_count(group, event) == Some(1)
                && sibling.channel_custody_count(group, event) == Some(1)
                && source.account_document_meta(&custody_key)
                    == sibling.account_document_meta(&custody_key)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let source_count = source.channel_custody_count(group, event);
    let external_has_original = external.log.lock().unwrap().get(&event).is_some();
    let source_meta = source.account_document_meta(&custody_key);
    release.close();
    source.note_account_local_change();
    let resumed = tokio::time::timeout(Duration::from_secs(8), async {
        loop {
            if history_requests.load(Ordering::SeqCst) >= 2
                && sibling.channel_custody_count(group, event) == Some(1)
                && source.account_document_meta(&custody_key)
                    == sibling.account_document_meta(&custody_key)
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    *source.account_session_test_hook.lock().unwrap() = None;
    for node in [&source, &sibling, &external] {
        node.runtime_work.close();
    }
    servers.shutdown().await;
    for node in [&source, &sibling, &external] {
        tokio::time::timeout(Duration::from_secs(5), node.runtime_work.drain())
            .await
            .expect("released history fixture must drain owned work");
    }
    assert!(
        resumed.is_ok(),
        "history and custody must resume after release"
    );
    assert_eq!(
        source_count,
        Some(1),
        "the real custody retry producer must authenticate external durable possession while history is held"
    );
    assert!(external_has_original);
    assert!(source_meta.is_some());
    assert!(
        converged.is_ok(),
        "external custody must converge while history is held"
    );
}
