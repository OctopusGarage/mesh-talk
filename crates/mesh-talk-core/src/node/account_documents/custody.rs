//! Account-authenticated witnesses of an exact authenticated durable-have exchange.
//! The holder's claim was session authenticated; it is not a transferable holder
//! signature and does not establish recipient delivery or reading.
use super::*;
use crate::{
    eventlog::{ConversationId, EventId},
    identity::device::PublicIdentity,
    node::account_devices::AccountDevice,
};

#[derive(Serialize, Deserialize)]
struct Observation {
    observer: AccountDevice,
    holder: PublicIdentity,
    holder_certificate: Option<crate::identity::account::DeviceCertificate>,
    post_office: bool,
}

pub(super) fn valid_payload(meta: &AccountDocumentMeta, payload: &[u8]) -> bool {
    let AccountDocumentKey::ChannelCustody { holder, .. } = &meta.key else {
        return false;
    };
    if meta.tombstone {
        return payload.is_empty();
    }
    let Ok(value) = serde_json::from_slice::<Observation>(payload) else {
        return false;
    };
    value.observer.public.user_id() == meta.writer_device
        && value.observer.certificate.verify()
        && value.observer.certificate.device_ed25519_pub == value.observer.public.ed25519_pub
        && value.observer.certificate.account_ed25519_pub == meta.account_public
        && value.holder.user_id() == holder.strip_prefix("relay:").unwrap_or(holder)
        && value.holder_certificate.as_ref().is_none_or(|certificate| {
            certificate.verify() && certificate.device_ed25519_pub == value.holder.ed25519_pub
        })
        && value.post_office == holder.starts_with("relay:")
}

impl Node {
    pub(in crate::node) fn publish_channel_custody(
        &self,
        channel: ConversationId,
        event: EventId,
        holder: &PublicIdentity,
        held: bool,
        migration: bool,
        post_office: bool,
    ) -> Result<(), NodeError> {
        {
            let book = self.channels.lock().expect("channels mutex");
            if !book
                .state(&channel)
                .is_some_and(|state| self.channel_identity_matches_account(state))
            {
                return Err(invalid());
            }
        }
        let key = AccountDocumentKey::ChannelCustody {
            channel: hex::encode(channel.as_bytes()),
            event: hex::encode(event.as_bytes()),
            holder: if post_office {
                format!("relay:{}", holder.user_id())
            } else {
                holder.user_id()
            },
        };
        let holder_certificate = self
            .historical_author(&holder.ed25519_pub)
            .filter(|proof| proof.public() == *holder)
            .and_then(|proof| proof.account_cert);
        let payload = if held {
            serde_json::to_vec(&Observation {
                observer: AccountDevice {
                    public: self.identity.public(),
                    certificate: self.account.certify(&self.identity.public().ed25519_pub),
                },
                holder: holder.clone(),
                holder_certificate,
                post_office,
            })
            .map_err(|_| invalid())?
        } else {
            Vec::new()
        };
        let changed = {
            let mut store = self.account_documents.lock().expect("documents mutex");
            let previous = store.meta(&key);
            let identical = previous.as_ref().is_some_and(|meta| {
                meta.tombstone != held && (!held || meta.content_hash == content_hash(&payload))
            });
            // Replaying a legacy journal must not replace a newer observation,
            // but an identical record may repair its interrupted/missing blob.
            if migration && previous.is_some() && !identical {
                return Ok(());
            }
            let meta = if identical {
                previous.expect("identical observation exists")
            } else {
                let revision = previous
                    .map_or(0, |meta| meta.revision)
                    .checked_add(1)
                    .ok_or_else(invalid)?
                    .max(super::super::node::now_millis());
                AccountDocumentMeta::new(
                    &self.account,
                    self.user_id(),
                    key,
                    revision,
                    &payload,
                    !held,
                )
            };
            store.merge(meta, &payload).map_err(|error| {
                // The bounded custody retry/migration owners retry this write;
                // no journal confirmation is fabricated after a failed commit.
                log::debug!(target: "mesh_talk::channel", "custody observation persistence failed: {error}");
                NodeError::Log(error)
            })?
        };
        if changed {
            self.documents_changed();
            self.delivery_status_notify.notify_waiters();
        }
        Ok(())
    }

    /// Migrate/retry a bounded portion of the existing local journal automatically.
    pub(in crate::node) fn migrate_channel_custody(&self, after: &mut Option<(EventId, String)>) {
        let rows = self
            .channel_custody
            .lock()
            .expect("custody mutex")
            .observations_after(after.as_ref());
        if rows.is_empty() {
            *after = None;
            return;
        }
        for ((event, holder), held) in rows {
            let selected = self.log.lock().expect("log mutex").get(&event);
            let selected = match selected {
                Ok(selected) => selected,
                Err(error) => {
                    log::warn!("custody migration deferred: {error}");
                    return;
                }
            };
            *after = Some((event, holder.clone()));
            let channel = selected.map(|event| event.conversation_id);
            let Some(channel) = channel else {
                continue;
            };
            let post_office = holder.starts_with("relay:");
            let public = if let Some(device) = holder.strip_prefix("relay:") {
                self.historical_peer_proofs()
                    .into_iter()
                    .find(|proof| proof.user_id == device && proof.post_office && proof.verify())
                    .map(|proof| proof.public())
            } else {
                self.channels
                    .lock()
                    .expect("channels mutex")
                    .state(&channel)
                    .and_then(|state| {
                        state
                            .members()
                            .iter()
                            .find(|member| member.user_id() == holder)
                            .cloned()
                    })
            };
            if let Some(public) = public {
                let _ =
                    self.publish_channel_custody(channel, event, &public, held, true, post_office);
            }
        }
    }

    pub(in crate::node) fn channel_custody_holder_binding(
        &self,
        key: &AccountDocumentKey,
    ) -> Option<(PublicIdentity, Option<String>)> {
        let document = self.account_document(key).ok()??;
        let value: Observation = serde_json::from_slice(document.payload()).ok()?;
        let account = value
            .holder_certificate
            .filter(|certificate| {
                certificate.verify() && certificate.device_ed25519_pub == value.holder.ed25519_pub
            })
            .map(|certificate| certificate.account_id());
        Some((value.holder, account))
    }

    pub(in crate::node) fn channel_custody_observations(
        &self,
        channel: ConversationId,
        event: EventId,
    ) -> Vec<AccountDocumentMeta> {
        let channel = hex::encode(channel.as_bytes());
        let event = hex::encode(event.as_bytes());
        let start = AccountDocumentKey::ChannelCustody {
            channel: channel.clone(),
            event: event.clone(),
            holder: String::new(),
        };
        self.account_documents
            .lock()
            .expect("documents mutex")
            .list_custody(&start, &channel, &event)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_custody_witness_binds_observer_holder_account_and_role() {
        let account = Account::generate();
        let observer = DeviceIdentity::generate();
        let holder = DeviceIdentity::generate();
        let holder_account = Account::generate();
        let mut value = Observation {
            observer: AccountDevice {
                public: observer.public(),
                certificate: account.certify(&observer.public().ed25519_pub),
            },
            holder: holder.public(),
            holder_certificate: Some(holder_account.certify(&holder.public().ed25519_pub)),
            post_office: false,
        };
        let key = AccountDocumentKey::ChannelCustody {
            channel: "1".repeat(64),
            event: "2".repeat(64),
            holder: holder.public().user_id(),
        };
        let payload = serde_json::to_vec(&value).unwrap();
        let meta = AccountDocumentMeta::new(
            &account,
            observer.public().user_id(),
            key.clone(),
            1,
            &payload,
            false,
        );
        assert!(valid_payload(&meta, &payload));
        let foreign = Account::generate();
        let foreign_meta = AccountDocumentMeta::new(
            &foreign,
            observer.public().user_id(),
            key.clone(),
            1,
            &payload,
            false,
        );
        assert!(!valid_payload(&foreign_meta, &payload));
        value.holder_certificate = Some(holder_account.certify(&observer.public().ed25519_pub));
        assert!(!valid_payload(&meta, &serde_json::to_vec(&value).unwrap()));
        value.holder_certificate = None;
        value.post_office = true;
        assert!(
            !valid_payload(&meta, &serde_json::to_vec(&value).unwrap()),
            "member and relay namespaces cannot be confused"
        );
        let relay = AccountDocumentKey::ChannelCustody {
            channel: "1".repeat(64),
            event: "2".repeat(64),
            holder: format!("relay:{}", holder.public().user_id()),
        };
        let payload = serde_json::to_vec(&value).unwrap();
        let relay_meta = AccountDocumentMeta::new(
            &account,
            observer.public().user_id(),
            relay.clone(),
            2,
            &payload,
            false,
        );
        assert!(valid_payload(&relay_meta, &payload));
        let tombstone =
            AccountDocumentMeta::new(&account, observer.public().user_id(), relay, 3, &[], true);
        assert!(valid_payload(&tombstone, &[]));
        let dir = tempfile::tempdir().unwrap();
        let mut store =
            store::AccountDocumentStore::open(dir.path(), "pw", &account.public().ed25519_pub)
                .unwrap();
        assert!(store.merge(relay_meta.clone(), &payload).unwrap());
        assert!(store.merge(tombstone.clone(), &[]).unwrap());
        assert!(
            !store.merge(relay_meta, &payload).unwrap(),
            "relay tombstones also reject stale positive replay"
        );
        assert_eq!(store.meta(&tombstone.key).unwrap(), tombstone);
    }
}
