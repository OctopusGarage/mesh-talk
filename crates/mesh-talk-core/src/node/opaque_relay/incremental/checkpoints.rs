//! Optional signed transport hints on the existing encrypted profile.
use super::*;
use bincode::Options;

fn hint_key(mailbox: ConversationId, inner: EventId) -> Vec<u8> {
    let mut key = b"wrapper-v1".to_vec();
    key.extend_from_slice(mailbox.as_bytes());
    key.extend_from_slice(inner.as_bytes());
    key
}
fn bounded<T: serde::de::DeserializeOwned>(bytes: &[u8]) -> Option<T> {
    bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(16384)
        .reject_trailing_bytes()
        .deserialize(bytes)
        .ok()
}

impl Node {
    pub(super) fn seal_mailbox_hint(&self, key: &[u8], body: Vec<u8>) -> Option<Vec<u8>> {
        let mut message = b"MeshTalkMailboxHintV1\0".to_vec();
        message.extend_from_slice(key);
        message.extend_from_slice(&body);
        let signature = self.identity.sign(&message).to_vec();
        bincode::serialize(&Sealed { body, signature }).ok()
    }
    pub(super) fn open_mailbox_hint(&self, key: &[u8], bytes: &[u8]) -> Option<Vec<u8>> {
        let sealed: Sealed = bounded(bytes)?;
        let signature: [u8; 64] = sealed.signature.try_into().ok()?;
        let mut message = b"MeshTalkMailboxHintV1\0".to_vec();
        message.extend_from_slice(key);
        message.extend_from_slice(&sealed.body);
        crate::identity::device::DeviceIdentity::verify(
            &self.identity.public().ed25519_pub,
            &message,
            &signature,
        )
        .then_some(sealed.body)
    }
    pub(super) fn mailbox_local_binding(&self) -> Result<(i64, i64), LogError> {
        let log = self.log.lock().expect("log lock");
        let repo = log.repository()?;
        Ok((repo.destructive_policy_revision()?, repo.source_revision()?))
    }
    pub(super) fn load_mailbox_receipt(
        &self,
        key: &[u8],
        conv: ConversationId,
    ) -> Result<Receipt, LogError> {
        let (epoch, source) = self.mailbox_local_binding()?;
        let cache = self.opaque_receive_cache.lock().expect("opaque cache");
        let runtime = cache.session_epoch();
        if let Ok(Some(bytes)) = cache.get_checkpoint(key) {
            if let Some(body) = self.open_mailbox_hint(key, &bytes)
                && let Some(receipt) = bounded::<Receipt>(&body)
            {
                let valid = receipt.epoch == epoch
                    && receipt.source_floor <= source
                    && receipt.source_floor >= 0
                    && receipt.pending.len() <= MAX_PENDING
                    && (!receipt.session_only || receipt.runtime == runtime)
                    && receipt.cursor.as_ref().is_none_or(|c| {
                        c.conversation == conv && c.after >= 0 && c.epoch >= 0 && c.revision >= 0
                    });
                if valid {
                    return Ok(receipt);
                }
            }
            let _ = cache.remove_checkpoint(key);
        }
        Ok(Receipt {
            cursor: None,
            pending: Vec::new(),
            epoch,
            source_floor: source,
            runtime,
            session_only: false,
        })
    }
    pub(super) fn save_mailbox_receipt(
        &self,
        p: &mut OpaqueMailboxProgress,
    ) -> Result<(), session::SessionError> {
        if state(p).saved.as_ref() == Some(&state(p).receipt) {
            return Ok(());
        }
        let (epoch, source) = self.mailbox_local_binding()?;
        if state(p).receipt.epoch != epoch {
            return Err(LogError::StaleCursor.into());
        }
        self.log.lock().expect("log lock").sync()?;
        state_mut(p).receipt.source_floor = source;
        let body = bincode::serialize(&state(p).receipt)
            .map_err(|e| session::SessionError::Serialization(e.to_string()))?;
        if let Some(sealed) = self.seal_mailbox_hint(&state(p).key, body) {
            let cache = self.opaque_receive_cache.lock().expect("opaque cache");
            if let Err(error) = cache.put_checkpoint(&state(p).key, &sealed) {
                log::debug!("mailbox checkpoint unavailable: {error}");
            }
        }
        state_mut(p).saved = Some(state(p).receipt.clone());
        Ok(())
    }
    pub(super) fn cached_wrapper_id(
        &self,
        mailbox: ConversationId,
        inner: EventId,
    ) -> Option<EventId> {
        let key = hint_key(mailbox, inner);
        let bytes = self
            .opaque_receive_cache
            .lock()
            .expect("opaque cache")
            .get_wrapper_hint(&key)
            .ok()??;
        let body = self.open_mailbox_hint(&key, &bytes)?;
        let id: [u8; 32] = body.try_into().ok()?;
        Some(EventId::new(id))
    }
    pub(in crate::node::opaque_relay) fn remember_wrapper_id(
        &self,
        mailbox: ConversationId,
        inner: EventId,
        outer: EventId,
    ) {
        let key = hint_key(mailbox, inner);
        if let Some(value) = self.seal_mailbox_hint(&key, outer.as_bytes().to_vec())
            && let Err(error) = self
                .opaque_receive_cache
                .lock()
                .expect("opaque cache")
                .put_wrapper_hint(&key, &value)
        {
            log::debug!("mailbox wrapper hint unavailable: {error}");
        }
    }
}
