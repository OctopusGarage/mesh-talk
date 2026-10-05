//! Authenticated automatic DM delivery confirmation, separate from chat messages.
//!
//! Preparing this metadata does NOT prove local plaintext durability. The host
//! must journal it with the detached receive transition and durably install the
//! received plaintext before publishing the receipt. Likewise, authenticating a
//! receipt does NOT persist its event or update delivery status. The host must
//! durably save the control event and serialize status updates with its delivery
//! store. Duplicate proofs are expected; the store owns idempotency.
//!
//! Uses the existing sealed-box crypto: metadata is encrypted and authenticated,
//! but has no forward secrecy. Contains no message text. Historical proofs must
//! come from the host's trusted directory; current privacy authorization is an
//! additional host check, never a permission granted by this module.

use super::conversation::{account_conversation_id, dm_conversation_id};
use super::delivery_store::OutgoingDelivery;
use super::dm_envelope::DmEnvelope;
use super::received_log::ReceivedEntry;
use crate::discovery::Announce;
use crate::dm::DmError;
use crate::eventlog::{ConversationId, Event, EventId, EventKind};
use crate::identity::device::{DeviceIdentity, PublicIdentity};
use bincode::Options;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const DOMAIN: &[u8; 24] = b"mesh-talk-dm-delivery-v1";
const VERSION: u8 = 1;
const MAX_RECEIPT_PLAINTEXT: usize = 512;
const MAX_RECEIPT_WIRE: usize = 1024;
const MAX_DM_PLAINTEXT: usize = 256 * 1024;
const MAX_PARENTS: usize = 1024;
/// Legacy sealed-box decoders see a fixed 32-byte ephemeral key followed
/// by an empty ciphertext vector. Strict parsers reject the trailing body;
/// permissive parsers fail AEAD on empty ciphertext without allocating it.
const RECEIPT_FRAME: [u8; 40] = {
    let mut marker = [0; 40];
    let mut index = 0;
    while index < DOMAIN.len() {
        marker[index] = DOMAIN[index];
        index += 1;
    }
    marker[24] = VERSION;
    marker
};

fn frame_receipt(sealed: Vec<u8>) -> Vec<u8> {
    let mut framed = Vec::with_capacity(RECEIPT_FRAME.len() + sealed.len());
    framed.extend_from_slice(&RECEIPT_FRAME);
    framed.extend_from_slice(&sealed);
    framed
}

/// Only Ed25519 keys determine scope, like the DM pair. Full Ed25519/X25519
/// identities are independently bound inside the receipt and by signed proofs.
pub(crate) fn delivery_conversation_id(a: &PublicIdentity, b: &PublicIdentity) -> ConversationId {
    let mut pair = [a.ed25519_pub, b.ed25519_pub];
    pair.sort();
    let mut hash = Sha256::new();
    hash.update(DOMAIN);
    hash.update(pair[0]);
    hash.update(pair[1]);
    ConversationId::new(hash.finalize().into())
}

/// Versioned, bounded metadata. Fields remain private so normal callers prepare
/// it from validated original receive metadata rather than arbitrary claims.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReceiptPayload {
    version: u8,
    domain: [u8; 24],
    original_kind: EventKind,
    original_conversation: ConversationId,
    original_event_id: EventId,
    logical_id: EventId,
    original_wall_clock: u64,
    sender_device: PublicIdentity,
    recipient_device: PublicIdentity,
    sender_account: String,
    recipient_account: String,
    confirmed_at: u64,
}

impl ReceiptPayload {
    /// `accepted` must be the actual plaintext opened from `original` by the
    /// host's detached ratchet decryption, not an imported history/backfill row.
    /// Preparation happens before the durable receive transaction; publication
    /// happens only after it. Own-account self-sync never produces a receipt.
    pub(crate) fn prepare(
        recipient_device: &PublicIdentity,
        recipient_account: &str,
        sender_proof: &Announce,
        original: &Event,
        accepted: &ReceivedEntry,
        confirmed_at: u64,
    ) -> Option<Self> {
        if !sender_proof.verify() || !valid_account(recipient_account) {
            return None;
        }
        let sender_account = sender_proof.account_id()?;
        let sender_device = sender_proof.public();
        if !valid_account(&sender_account)
            || sender_account == recipient_account
            || sender_device.ed25519_pub == recipient_device.ed25519_pub
            || !valid_signed_message(original)
            || original.author.ed25519_pub() != &sender_device.ed25519_pub
            || original.conversation_id != dm_conversation_id(&sender_device, recipient_device)
            || accepted.event_id != original.id
            || accepted.wall_clock != original.wall_clock
            || accepted.plaintext.len() > MAX_DM_PLAINTEXT
        {
            return None;
        }
        let logical_id = if original.kind == EventKind::FileManifest {
            let manifest = super::files::validated_manifest(&accepted.plaintext)?;
            if manifest.file_conv() == original.conversation_id
                || accepted.from != sender_device.user_id()
                || (accepted.conversation != original.conversation_id
                    && accepted.conversation
                        != account_conversation_id(&sender_account, recipient_account))
            {
                return None;
            }
            original.id
        } else if let Some(body) = accepted.plaintext.strip_prefix(b"MTDE1") {
            // A malformed framed envelope must never fall back to legacy routing.
            let envelope: DmEnvelope = bincode::DefaultOptions::new()
                .with_fixint_encoding()
                .with_limit(MAX_DM_PLAINTEXT as u64)
                .reject_trailing_bytes()
                .deserialize(body)
                .ok()?;
            if envelope.route.sender_account != sender_account
                || envelope.route.recipient_account != recipient_account
                || accepted.from != sender_account
                || accepted.conversation
                    != account_conversation_id(&sender_account, recipient_account)
            {
                return None;
            }
            EventId::new(envelope.msg_id)
        } else {
            if accepted.from != sender_device.user_id()
                || accepted.conversation != original.conversation_id
            {
                return None;
            }
            original.id
        };
        Some(Self {
            version: VERSION,
            domain: *DOMAIN,
            original_kind: original.kind,
            original_conversation: original.conversation_id,
            original_event_id: original.id,
            logical_id,
            original_wall_clock: original.wall_clock,
            sender_device,
            recipient_device: recipient_device.clone(),
            sender_account,
            recipient_account: recipient_account.to_owned(),
            confirmed_at,
        })
    }

    pub(crate) fn conversation(&self) -> ConversationId {
        delivery_conversation_id(&self.sender_device, &self.recipient_device)
    }

    pub(crate) fn confirmed_at(&self) -> u64 {
        self.confirmed_at
    }

    pub(crate) fn seal(&self, recipient: &DeviceIdentity) -> Result<Vec<u8>, DmError> {
        if recipient.public() != self.recipient_device {
            return Err(DmError::Encrypt);
        }
        crate::dm::seal(recipient, &self.sender_device.x25519_pub, &self.encode()?)
            .map(frame_receipt)
    }

    fn encode(&self) -> Result<Vec<u8>, DmError> {
        if !self.valid_structure() {
            return Err(DmError::Serialization("invalid delivery receipt".into()));
        }
        bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_RECEIPT_PLAINTEXT as u64)
            .serialize(self)
            .map_err(|_| DmError::Serialization("delivery receipt encoding".into()))
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() > MAX_RECEIPT_PLAINTEXT {
            return None;
        }
        let payload: Self = bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .with_limit(MAX_RECEIPT_PLAINTEXT as u64)
            .reject_trailing_bytes()
            .deserialize(bytes)
            .ok()?;
        payload.valid_structure().then_some(payload)
    }

    fn valid_structure(&self) -> bool {
        self.version == VERSION
            && self.domain == *DOMAIN
            && matches!(
                self.original_kind,
                EventKind::Message | EventKind::FileManifest
            )
            && (self.original_kind != EventKind::FileManifest
                || self.logical_id == self.original_event_id)
            && valid_account(&self.sender_account)
            && valid_account(&self.recipient_account)
            && self.sender_account != self.recipient_account
            && self.sender_device.ed25519_pub != self.recipient_device.ed25519_pub
            && self.original_conversation
                == dm_conversation_id(&self.sender_device, &self.recipient_device)
    }
}

/// A proof bound to an awaiting immutable outgoing destination. Only this module
/// can construct it. It asserts authentication, not local persistence or a new
/// state transition; exact replays return the same logical/event identifiers.
pub(crate) struct AuthenticatedReceipt {
    logical_id: EventId,
    original_event_id: EventId,
    #[cfg(test)]
    confirmed_at: u64,
}

impl AuthenticatedReceipt {
    pub(crate) fn logical_id(&self) -> EventId {
        self.logical_id
    }

    pub(crate) fn original_event_id(&self) -> EventId {
        self.original_event_id
    }

    #[cfg(test)]
    pub(crate) fn confirmed_at(&self) -> u64 {
        self.confirmed_at
    }
}

/// Authenticated metadata for an outbox lookup, NOT a delivery confirmation.
/// Bind it to the exact immutable outgoing record before updating status.
pub(crate) struct OpenedReceipt {
    payload: ReceiptPayload,
}

impl OpenedReceipt {
    pub(crate) fn logical_id(&self) -> EventId {
        self.payload.logical_id
    }

    pub(crate) fn original_event_id(&self) -> EventId {
        self.payload.original_event_id
    }

    pub(crate) fn authenticate(&self, message: &OutgoingDelivery) -> Option<AuthenticatedReceipt> {
        let payload = &self.payload;
        if message.sender_account != payload.sender_account
            || message.recipient_account.as_deref() != Some(payload.recipient_account.as_str())
            || (payload.original_kind == EventKind::Message
                && message.logical_id != payload.logical_id)
        {
            return None;
        }
        let destination = message.destinations.iter().find(|destination| {
            destination.receipt_eligible
                && destination.device == payload.recipient_device
                && destination.account.as_deref() == Some(payload.recipient_account.as_str())
                && destination.event.id == payload.original_event_id
        })?;
        let original = &destination.event;
        if !valid_signed_message(original)
            || original.author.ed25519_pub() != &payload.sender_device.ed25519_pub
            || original.conversation_id != payload.original_conversation
            || original.kind != payload.original_kind
            || original.wall_clock != message.wall_clock
            || original.wall_clock != payload.original_wall_clock
        {
            return None;
        }
        // Raw sends use their immutable event id; account sends have one logical
        // history route shared by their per-device ciphertexts.
        if message.conversation == original.conversation_id {
            if message.logical_id != original.id || message.destinations.len() != 1 {
                return None;
            }
        } else if message.conversation
            != account_conversation_id(&payload.sender_account, &payload.recipient_account)
        {
            return None;
        }
        Some(AuthenticatedReceipt {
            logical_id: message.logical_id,
            original_event_id: payload.original_event_id,
            #[cfg(test)]
            confirmed_at: payload.confirmed_at,
        })
    }
}

/// The caller supplies its own actual account and a trusted historical/current
/// target proof, and separately checks current privacy authorization. Full proof
/// equality rejects account rebinding and X25519 substitution. A post office
/// transporting the event never becomes the confirming principal.
pub(crate) fn open_receipt(
    sender: &DeviceIdentity,
    sender_account: &str,
    target_proof: &Announce,
    event: &Event,
) -> Option<OpenedReceipt> {
    if !valid_account(sender_account)
        || !target_proof.verify()
        || event.kind != EventKind::Message
        || !valid_sealed_wire(&event.ciphertext)
        || event.parents.len() > MAX_PARENTS
        || !event.is_canonical()
        || !event.verify_integrity()
        || !event.verify_signature()
    {
        return None;
    }
    let target_account = target_proof.account_id()?;
    let target = target_proof.public();
    let own_device = sender.public();
    if !valid_account(&target_account)
        || target_account == sender_account
        || event.author.ed25519_pub() != &target.ed25519_pub
        || event.conversation_id != delivery_conversation_id(&own_device, &target)
    {
        return None;
    }
    let wire = event.ciphertext.strip_prefix(&RECEIPT_FRAME)?;
    let plaintext = crate::dm::open(sender, &target.x25519_pub, wire).ok()?;
    let payload = ReceiptPayload::decode(&plaintext)?;
    if payload.sender_device != own_device
        || payload.recipient_device != target
        || payload.sender_account != sender_account
        || payload.recipient_account != target_account
        || payload.confirmed_at != event.wall_clock
    {
        return None;
    }
    Some(OpenedReceipt { payload })
}

fn valid_account(account: &str) -> bool {
    account.len() == 32
        && account
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Parse the existing sealed-box type under a small byte budget before its
/// general-purpose opener. This preserves its strict, fixed-int wire format
/// after its receipt-only marker, without changing ordinary DM crypto.
pub(in crate::node) fn valid_sealed_wire(wire: &[u8]) -> bool {
    if wire.len() > MAX_RECEIPT_WIRE {
        return false;
    }
    let Some(wire) = wire.strip_prefix(&RECEIPT_FRAME) else {
        return false;
    };
    let Ok(envelope) = bincode::DefaultOptions::new()
        .with_fixint_encoding()
        .with_limit(MAX_RECEIPT_WIRE as u64)
        .reject_trailing_bytes()
        .deserialize::<crate::dm::SealedEnvelope>(wire)
    else {
        return false;
    };
    // AES-GCM has a 16-byte authentication tag. The opened metadata has its own
    // tighter 512-byte budget and strict decoder.
    (16..=MAX_RECEIPT_PLAINTEXT + 16).contains(&envelope.ciphertext.len())
}

fn valid_signed_message(event: &Event) -> bool {
    matches!(event.kind, EventKind::Message | EventKind::FileManifest)
        && event.ciphertext.len() <= MAX_DM_PLAINTEXT
        && event.parents.len() <= MAX_PARENTS
        && event.is_canonical()
        && event.verify_integrity()
        && event.verify_signature()
}

#[cfg(test)]
#[path = "delivery_receipt_tests.rs"]
mod tests;
