//! JSON metadata within append-only optional account-session frames.
use super::*;
pub(super) const MAX_META: usize = 8192;
pub(super) const MAX_PAGE_BYTES: usize = 12 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::node) struct DocumentRequest {
    pub version: u8,
    pub account_id: String,
    pub domain: AccountDocumentDomain,
    pub after: Option<AccountDocumentKey>,
    pub known_digest: [u8; 32],
}
impl DocumentRequest {
    pub(in crate::node) fn decode(body: &[u8]) -> Option<Self> {
        if body.len() > MAX_META {
            return None;
        }
        let request: Self = serde_json::from_slice(body).ok()?;
        ((match request.domain {
            AccountDocumentDomain::GroupControl => request.version == 5,
            AccountDocumentDomain::GroupInventory => request.version == 4,
            AccountDocumentDomain::ChannelCustody => request.version == 3,
            AccountDocumentDomain::DmReaction | AccountDocumentDomain::DmRecall => {
                request.version == 2
            }
            _ => request.version == 1 || request.version == 2,
        }) && hex(&request.account_id, 32)
            && request
                .after
                .as_ref()
                .is_none_or(|k| k.valid() && k.domain() == request.domain))
        .then_some(request)
    }
    pub(super) fn encode(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("document request serializes")
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct DocumentComplete {
    pub enabled: bool,
    pub domain: AccountDocumentDomain,
    pub next: Option<AccountDocumentKey>,
    pub digest: [u8; 32],
}
pub(in crate::node) fn valid_complete(body: &[u8]) -> bool {
    body.len() <= MAX_META
        && serde_json::from_slice::<DocumentComplete>(body).is_ok_and(|c| {
            c.next
                .as_ref()
                .is_none_or(|k| k.valid() && k.domain() == c.domain)
        })
}
pub(in crate::node) fn valid_header(body: &[u8]) -> bool {
    body.len() <= MAX_META
        && serde_json::from_slice::<AccountDocumentMeta>(body)
            .is_ok_and(|m| m.valid(&m.account_public))
}
