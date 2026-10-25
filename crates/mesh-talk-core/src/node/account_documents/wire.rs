//! JSON metadata within append-only optional account-session frames.
use super::*;
pub(super) const MAX_META: usize = 8192;
pub(super) const MAX_PAGE_BYTES: usize = 12 * 1024 * 1024;
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::node) struct OperationContinuation {
    pub channel: String,
    pub source_device: String,
}
impl OperationContinuation {
    pub(super) fn key(&self) -> AccountDocumentKey {
        AccountDocumentKey::GroupOperation {
            channel: self.channel.clone(),
            source_device: self.source_device.clone(),
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::node) struct OperationProbe {
    pub version: u8,
    pub nonce: [u8; 32],
    pub after: Option<OperationContinuation>,
    #[serde(default)]
    pub known_digest: [u8; 32],
}
impl OperationProbe {
    pub(super) fn supported(&self) -> bool {
        self.version == 1
            && self.nonce != [0; 32]
            && self.after.as_ref().is_none_or(|after| after.key().valid())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::node) struct OperationCapability {
    pub version: u8,
    pub nonce: [u8; 32],
}
impl OperationCapability {
    fn valid(&self) -> bool {
        self.version == 1 && self.nonce != [0; 32]
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::node) struct DocumentRequest {
    pub version: u8,
    pub account_id: String,
    pub domain: AccountDocumentDomain,
    pub after: Option<AccountDocumentKey>,
    pub known_digest: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationProbe>,
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
            && request.after.as_ref().is_none_or(|k| {
                k.valid()
                    && k.domain() == request.domain
                    && !matches!(k, AccountDocumentKey::GroupOperation { .. })
            })
            && request.operation.as_ref().is_none_or(|operation| {
                request.domain == AccountDocumentDomain::GroupControl
                    && operation
                        .after
                        .as_ref()
                        .is_none_or(|after| after.key().valid())
            }))
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationCapability>,
}
pub(in crate::node) fn valid_complete(body: &[u8]) -> bool {
    body.len() <= MAX_META
        && serde_json::from_slice::<DocumentComplete>(body).is_ok_and(|c| {
            c.operation.as_ref().is_none_or(|operation| {
                c.domain == AccountDocumentDomain::GroupControl && operation.valid()
            }) && c.next.as_ref().is_none_or(|k| {
                k.valid()
                    && k.domain() == c.domain
                    && (!matches!(k, AccountDocumentKey::GroupOperation { .. })
                        || c.operation.is_some())
            })
        })
}
pub(in crate::node) fn valid_header(body: &[u8]) -> bool {
    body.len() <= MAX_META
        && serde_json::from_slice::<AccountDocumentMeta>(body)
            .is_ok_and(|m| m.valid(&m.account_public))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn operation_probe_cannot_extend_an_optional_history_domain() {
        let body = serde_json::json!({
            "version":1, "account_id":"a".repeat(32), "domain":"draft",
            "after":null, "known_digest":([0u8;32]),
            "operation":{"version":1,"nonce":([7u8;32]),"after":null},
        });
        assert!(DocumentRequest::decode(&serde_json::to_vec(&body).unwrap()).is_none());
    }
    #[test]
    fn malformed_operation_continuations_are_rejected_before_exchange() {
        let body = serde_json::json!({
            "version":5, "account_id":"a".repeat(32), "domain":"group_control",
            "after":null, "known_digest":([0u8;32]),
            "operation":{"version":1,"nonce":([7u8;32]),"after":{"channel":"bad","source_device":"b".repeat(32)}},
        });
        assert!(DocumentRequest::decode(&serde_json::to_vec(&body).unwrap()).is_none());
    }
    #[test]
    fn legacy_request_and_completion_json_bytes_round_trip_without_extension_fields() {
        for (version, domain) in [
            (1, "draft"),
            (2, "dm_reaction"),
            (3, "channel_custody"),
            (4, "group_inventory"),
        ] {
            let body = format!(
                "{{\"version\":{version},\"account_id\":\"{}\",\"domain\":\"{domain}\",\"after\":null,\"known_digest\":[{}]}}",
                "a".repeat(32),
                std::iter::repeat_n("0", 32).collect::<Vec<_>>().join(",")
            );
            assert_eq!(
                DocumentRequest::decode(body.as_bytes()).unwrap().encode(),
                body.as_bytes()
            );
            let body = format!(
                "{{\"enabled\":true,\"domain\":\"{domain}\",\"next\":null,\"digest\":[{}]}}",
                std::iter::repeat_n("0", 32).collect::<Vec<_>>().join(",")
            );
            assert!(valid_complete(body.as_bytes()));
            assert_eq!(
                serde_json::to_vec(&serde_json::from_str::<DocumentComplete>(&body).unwrap())
                    .unwrap(),
                body.as_bytes()
            );
        }
    }
}
