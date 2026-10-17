//! Shared account-aware loopback fixture. Receive policy keeps production defaults;
//! tests that need the legacy automatic-transfer behavior opt in explicitly.

use super::{Node, ReceivedDm, ReceivedFile};
use crate::discovery::{Announce, Roster};
use crate::identity::{account::Account, device::DeviceIdentity};
use std::{
    net::{IpAddr, Ipv4Addr},
    path::Path,
    sync::{Arc, Mutex},
};
use tokio::sync::mpsc;

pub(super) fn node_with_files(
    dir: &Path,
    identity: DeviceIdentity,
    account: Account,
    proof: &Announce,
) -> (
    Arc<Node>,
    mpsc::UnboundedReceiver<ReceivedDm>,
    mpsc::UnboundedReceiver<ReceivedFile>,
) {
    let roster = Arc::new(Mutex::new(Roster::default()));
    roster
        .lock()
        .unwrap()
        .update(proof, IpAddr::V4(Ipv4Addr::LOCALHOST), &identity.user_id());
    let (tx, rx) = mpsc::unbounded_channel();
    let (ch, _) = mpsc::unbounded_channel();
    let (file, files) = mpsc::unbounded_channel();
    let node = Node::open_with_account(
        identity,
        account,
        roster,
        tx,
        ch,
        file,
        &dir.join("events.log"),
        &dir.join("sent.log"),
        "pw",
    )
    .unwrap();
    (node, rx, files)
}
