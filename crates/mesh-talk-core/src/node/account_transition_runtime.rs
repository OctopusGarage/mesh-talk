//! Authenticated, exact-device migration at the existing privacy admission boundary.
use super::{Node, peer_directory::AccountBindingPatch};
use crate::identity::account_transition::AccountBindingTransition;
use crate::transport::{TransportError, VerifiedPeer};
use crate::{discovery::Announce, eventlog::ConversationId};
use std::sync::atomic::Ordering;

impl Node {
    pub(in crate::node) fn own_account_transitions(&self) -> Vec<AccountBindingTransition> {
        self.account_transitions
            .lock()
            .expect("account transition lock not poisoned")
            .own_chain(&self.identity.public(), &self.account.public().ed25519_pub)
    }

    /// This is only provisional Noise admission. No application frame is admitted
    /// until the complete, pinned transition has been persisted below.
    pub(in crate::node) fn account_transition_admitted(
        &self,
        peer: &VerifiedPeer,
    ) -> Result<bool, crate::eventlog::LogError> {
        let Some(announce) = peer.announcement() else {
            return Ok(false);
        };
        let policy_allows = {
            let guard = self
                .privacy
                .state
                .read()
                .expect("privacy state lock not poisoned");
            let Some(state) = guard.as_ref() else {
                return Ok(false);
            };
            let Some(previous) = state
                .proofs
                .transition_candidate(announce, peer.account_transitions())
            else {
                return Ok(false);
            };
            !state.policy.snapshot().invisible || state.policy.allows(&previous)
        };
        Ok(policy_allows
            || self.group_member(peer.public_identity())?
            || self.group_departure_peer(peer.public_identity()))
    }

    pub(super) fn account_binding_patches(&self) -> Vec<AccountBindingPatch> {
        let (mut patches, own) = {
            let guard = self
                .privacy
                .state
                .read()
                .expect("privacy state lock not poisoned");
            let Some(state) = guard.as_ref() else {
                return Vec::new();
            };
            (state.proofs.transition_patches(), state.own.clone())
        };
        let transitions = self.own_account_transitions();
        if !transitions.is_empty() {
            patches.retain(|patch| patch.announce.public() != own.public());
            patches.push(AccountBindingPatch {
                announce: own,
                transitions,
            });
        }
        patches.sort_by_key(|patch| patch.announce.user_id.clone());
        patches
    }

    /// A shared channel scopes transport of the public proof. It never gives a
    /// relay authority to mint membership or turn historical presence online.
    pub(super) fn apply_relayed_account_transition(
        &self,
        patch: &AccountBindingPatch,
        channel: ConversationId,
    ) -> Result<bool, TransportError> {
        let projection = self
            .account_group_projection(channel)
            .map_err(|error| TransportError::Io(std::io::Error::other(error)))?;
        let Some(state) = projection
            .state
            .as_ref()
            .filter(|state| !state.is_dissolved())
        else {
            return Err(TransportError::AdmissionDenied);
        };
        for public in [self.identity.public(), patch.announce.public()] {
            if !state.is_accepted_member(&public.user_id()) || !state.members().contains(&public) {
                return Err(TransportError::AdmissionDenied);
            }
        }
        if projection.authority.current_event().is_some()
            && !projection
                .authority
                .is_member(&self.identity.public(), &self.own_group_proof())
        {
            return Err(TransportError::AdmissionDenied);
        }
        self.apply_transition_announcement(&patch.announce, &patch.transitions, true)
    }

    pub(in crate::node) fn apply_account_transition(
        &self,
        peer: &VerifiedPeer,
    ) -> Result<(), TransportError> {
        let Some(announce) = peer.announcement() else {
            return Ok(());
        };
        let group = self
            .group_member(peer.public_identity())
            .map_err(|error| TransportError::Io(std::io::Error::other(error)))?
            || self.group_departure_peer(peer.public_identity());
        self.apply_transition_announcement(announce, peer.account_transitions(), group)
            .map(|_| ())
    }

    fn apply_transition_announcement(
        &self,
        announce: &Announce,
        transitions: &[AccountBindingTransition],
        group: bool,
    ) -> Result<bool, TransportError> {
        let mut guard = self
            .privacy
            .state
            .write()
            .expect("privacy state lock not poisoned");
        let Some(state) = guard.as_mut() else {
            return Ok(false);
        };
        if state.proofs.accepts_binding(announce) {
            return Ok(false);
        }
        let previous = state
            .proofs
            .transition_candidate(announce, transitions)
            .ok_or(TransportError::AdmissionDenied)?;
        let policy = state.policy.snapshot();
        let prior_grant = policy
            .allowed_accounts
            .iter()
            .find(|entry| entry.id == previous)
            .cloned();
        if policy.invisible && prior_grant.is_none() && !group {
            return Err(TransportError::AdmissionDenied);
        }
        // The proof itself authorizes this grant; retaining the previous grant
        // makes a failed subsequent proof write safe and retryable. Neither old
        // conversation history nor safety-number verification is moved.
        if let Some(grant) = prior_grant {
            state
                .policy
                .grant(
                    &announce
                        .account_id()
                        .ok_or(TransportError::AdmissionDenied)?,
                    &announce.name,
                    grant.source,
                )
                .map_err(|_| TransportError::AdmissionDenied)?;
        }
        state
            .proofs
            .record_transition(announce, transitions)
            .map_err(|_| TransportError::AdmissionDenied)?;
        self.privacy.generation.fetch_add(1, Ordering::SeqCst);
        drop(guard);
        self.roster
            .lock()
            .expect("roster lock not poisoned")
            .authorize_verified_account_transition(
                &announce.public(),
                announce
                    .account_cert
                    .as_ref()
                    .ok_or(TransportError::AdmissionDenied)?,
            );
        self.note_ui_change();
        self.channel_recovery_notify.notify_one();
        self.delivery_notify.notify_one();
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        discovery::{Announce, Roster, visibility::DiscoveryVisibility},
        identity::{account::Account, device::DeviceIdentity},
    };
    use std::{
        path::Path,
        sync::{Arc, Mutex},
        time::Duration,
    };
    use tokio::{
        net::{TcpListener, TcpStream},
        sync::mpsc,
    };

    fn open(path: &Path, device: &DeviceIdentity, account: &Account) -> Arc<Node> {
        std::fs::create_dir_all(path).unwrap();
        let (tx, _) = mpsc::unbounded_channel();
        let (ctx, _) = mpsc::unbounded_channel();
        let (ftx, _) = mpsc::unbounded_channel();
        let (ed, x) = device.secret_bytes();
        let node = Node::open_with_account(
            DeviceIdentity::from_secret_bytes(ed, x),
            Account::from_secret_bytes(account.secret_bytes()),
            Arc::new(Mutex::new(Roster::default())),
            tx,
            ctx,
            ftx,
            &path.join("messages.log"),
            &path.join("sent.log"),
            "pw",
        )
        .unwrap();
        node.configure_privacy(
            path,
            "pw",
            &node.signed_announce("Peer", 1234),
            Arc::new(DiscoveryVisibility::new(true)),
        )
        .unwrap();
        node
    }

    #[tokio::test]
    async fn upgraded_recipient_repairs_persistent_prelink_binding_and_group_count_over_noise() {
        let root = tempfile::tempdir().unwrap();
        let device = DeviceIdentity::generate();
        let old = Account::generate();
        let current = Account::generate();
        let recipient_device = DeviceIdentity::generate();
        let recipient_account = Account::generate();
        let path = root.path().join("recipient");
        let recipient = open(&path, &recipient_device, &recipient_account);
        let before = Announce::new_with_account(&device, &old, "Before", 1234);
        recipient
            .privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record(&before)
            .unwrap();
        let sibling = DeviceIdentity::generate();
        let sibling_proof = Announce::new_with_account(&sibling, &current, "Sibling", 1234);
        recipient
            .privacy
            .state
            .write()
            .unwrap()
            .as_mut()
            .unwrap()
            .proofs
            .record(&sibling_proof)
            .unwrap();
        let members = vec![recipient_device.public(), device.public(), sibling.public()];
        assert_eq!(
            recipient.project_channel_member_accounts(&members).0.len(),
            3
        );
        drop(recipient);
        // Simulates an offline or older recipient: only the old durable pin exists
        // when the upgraded runtime starts, and no manual acceptance is performed.
        let recipient = open(&path, &recipient_device, &recipient_account);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let recipient_task = recipient.clone();
        let accepted = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = recipient_task.privacy_accept(stream).await.unwrap();
            assert_eq!(channel.recv().await.unwrap(), b"after authenticated repair");
        });
        let after = Announce::new_with_account(&device, &current, "After", 1234);
        let transition = AccountBindingTransition::create(&old, &current, &device, None);
        let mut channel = crate::transport::SecureChannel::connect_with_transitions(
            TcpStream::connect(address).await.unwrap(),
            &device,
            Some(&recipient.identity.public()),
            Some(&after),
            true,
            &[transition],
        )
        .await
        .unwrap();
        channel.send(b"after authenticated repair").await.unwrap();
        tokio::time::timeout(Duration::from_secs(3), accepted)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            recipient.project_channel_member_accounts(&members).0.len(),
            2
        );
        let state = recipient.privacy.state.read().unwrap();
        assert!(state.as_ref().unwrap().proofs.accepts_binding(&after));
        assert!(!state.as_ref().unwrap().proofs.accepts_binding(&before));
        drop(state);
        drop(recipient);
        let reopened = open(&path, &recipient_device, &recipient_account);
        assert_eq!(
            reopened.project_channel_member_accounts(&members).0.len(),
            2
        );
        assert!(
            reopened
                .account_binding_changes(&current.account_id())
                .is_empty()
        );
    }
}
