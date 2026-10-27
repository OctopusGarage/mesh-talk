//! JSON metadata within append-only optional account-session frames.
use super::*;
pub(super) const MAX_META: usize = 8192;
pub(super) const MAX_PAGE_BYTES: usize = 12 * 1024 * 1024;
fn legacy_disabled(enabled: &bool) -> bool {
    !enabled
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::node) struct OperationContinuation {
    pub channel: String,
    pub source_device: String,
    #[serde(default, skip_serializing_if = "legacy_disabled")]
    pub legacy_leave: bool,
}
impl OperationContinuation {
    pub(super) fn key(&self) -> AccountDocumentKey {
        if self.legacy_leave {
            AccountDocumentKey::GroupLegacyOperation {
                channel: self.channel.clone(),
                source_device: self.source_device.clone(),
            }
        } else {
            AccountDocumentKey::GroupOperation {
                channel: self.channel.clone(),
                source_device: self.source_device.clone(),
            }
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::node) struct GroupFileAssetRequest {
    pub id: crate::node::AccountGroupOperationId,
    pub group: crate::eventlog::ConversationId,
    pub digest: [u8; 32],
    pub file_conv: [u8; 32],
    pub start_chunk: u32,
    #[serde(default, skip_serializing_if = "asset_execution_mode")]
    pub result: bool,
}
fn asset_execution_mode(result: &bool) -> bool {
    !*result
}
impl GroupFileAssetRequest {
    pub(in crate::node) fn valid(&self) -> bool {
        self.id.as_bytes() != &[0; 32]
            && self.group.as_bytes() != &[0; 32]
            && self.file_conv != [0; 32]
            && self.start_chunk <= crate::file::chunk_count_for(crate::node::node::MAX_FILE_SIZE)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::node) struct OperationProbe {
    pub version: u8,
    pub nonce: [u8; 32],
    pub after: Option<OperationContinuation>,
    #[serde(default)]
    pub known_digest: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<GroupFileAssetRequest>,
    #[serde(default, skip_serializing_if = "legacy_disabled")]
    pub legacy_leave: bool,
    #[serde(default, skip_serializing_if = "legacy_disabled")]
    pub personal: bool,
}
impl OperationProbe {
    pub(super) fn supported(&self) -> bool {
        self.version == 1
            && self.nonce != [0; 32]
            && self.asset.as_ref().is_none_or(|asset| {
                asset.valid() && self.after.is_none() && self.known_digest == [0; 32]
            })
            && self.after.as_ref().is_none_or(|after| {
                after.key().valid() && (!after.legacy_leave || self.legacy_leave)
            })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(in crate::node) struct OperationCapability {
    pub version: u8,
    pub nonce: [u8; 32],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supported_kinds: Option<u16>,
    #[serde(default, skip_serializing_if = "legacy_disabled")]
    pub legacy_leave: bool,
    #[serde(default, skip_serializing_if = "legacy_disabled")]
    pub personal: bool,
}
impl OperationCapability {
    fn valid(&self) -> bool {
        self.version == 1
            && self.nonce != [0; 32]
            && self.supported_kinds.is_none_or(|mask| mask & !0x3fff == 0)
    }
    pub(super) fn capabilities(&self) -> u16 {
        self.supported_kinds
            .unwrap_or_else(crate::node::AccountGroupOperationKind::legacy_capabilities)
            | if self.legacy_leave {
                crate::node::account_group::operations::legacy_wire::LEGACY_LEAVE_CAPABILITY
            } else {
                0
            }
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
            AccountDocumentDomain::PublicProfile | AccountDocumentDomain::GroupActivity => {
                request.version == 6
            }
            AccountDocumentDomain::GroupControl => request.version == 5 || request.version == 7,
            AccountDocumentDomain::DeviceHostname => request.version == 5,
            AccountDocumentDomain::GroupInventory => request.version == 4,
            AccountDocumentDomain::ChannelCustody => request.version == 3,
            AccountDocumentDomain::DmReaction | AccountDocumentDomain::DmRecall => {
                request.version == 2
            }
            _ => request.version == 1 || request.version == 2,
        }) && (request.version != 7
            || request
                .operation
                .as_ref()
                .is_some_and(|probe| probe.personal && probe.supported()))
            && hex(&request.account_id, 32)
            && request.after.as_ref().is_none_or(|k| {
                k.valid()
                    && (request.version == 7
                        || !matches!(k, AccountDocumentKey::ConversationPersonal { .. }))
                    && k.domain() == request.domain
                    && !matches!(
                        k,
                        AccountDocumentKey::GroupOperation { .. }
                            | AccountDocumentKey::GroupLegacyOperation { .. }
                    )
            })
            && request.operation.as_ref().is_none_or(|operation| {
                request.domain == AccountDocumentDomain::GroupControl
                    && operation.asset.as_ref().is_none_or(|_| {
                        operation.supported()
                            && request.after.is_none()
                            && request.known_digest == [0; 32]
                    })
                    && operation
                        .after
                        .as_ref()
                        .is_none_or(|after| !after.legacy_leave || operation.legacy_leave)
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
                    && match k {
                        AccountDocumentKey::GroupOperation { .. } => c.operation.is_some(),
                        AccountDocumentKey::GroupLegacyOperation { .. } => c
                            .operation
                            .as_ref()
                            .is_some_and(|capability| capability.legacy_leave),
                        _ => true,
                    }
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
    fn legacy_child_capability_is_additive_and_never_sets_reserved_wire_mask_bits() {
        #[derive(Deserialize)]
        struct OldCapability {
            version: u8,
            nonce: [u8; 32],
            supported_kinds: Option<u16>,
        }
        let capability = OperationCapability {
            version: 1,
            nonce: [7; 32],
            supported_kinds: Some(1 << 13),
            legacy_leave: true,
            personal: false,
        };
        let bytes = serde_json::to_vec(&capability).unwrap();
        let old: OldCapability = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(old.version, 1);
        assert_eq!(old.nonce, [7; 32]);
        assert_eq!(old.supported_kinds, Some(1 << 13));
        assert!(capability.valid());
        assert_eq!(capability.capabilities(), (1 << 13) | (1 << 14));
        let old_bytes =
            serde_json::json!({ "version": 1, "nonce": ([7u8;32]), "supported_kinds": 8192 });
        let parsed: OperationCapability = serde_json::from_value(old_bytes).unwrap();
        assert!(!parsed.legacy_leave);
        assert_eq!(parsed.capabilities(), 1 << 13);
    }
    #[test]
    fn legacy_child_continuation_requires_negotiated_flag_and_matching_completion() {
        let key = AccountDocumentKey::GroupLegacyOperation {
            channel: "a".repeat(64),
            source_device: "b".repeat(32),
        };
        let mut completion = DocumentComplete {
            enabled: true,
            domain: AccountDocumentDomain::GroupControl,
            next: Some(key),
            digest: [4; 32],
            operation: Some(OperationCapability {
                version: 1,
                nonce: [7; 32],
                supported_kinds: Some(8192),
                legacy_leave: false,
                personal: false,
            }),
        };
        assert!(!valid_complete(&serde_json::to_vec(&completion).unwrap()));
        completion.operation.as_mut().unwrap().legacy_leave = true;
        assert!(valid_complete(&serde_json::to_vec(&completion).unwrap()));
        let mut probe = OperationProbe {
            version: 1,
            nonce: [7; 32],
            after: Some(OperationContinuation {
                channel: "a".repeat(64),
                source_device: "b".repeat(32),
                legacy_leave: true,
            }),
            known_digest: [0; 32],
            asset: None,
            legacy_leave: false,
            personal: false,
        };
        assert!(!probe.supported());
        probe.legacy_leave = true;
        assert!(probe.supported());
    }
    #[test]
    fn operation_completion_retains_an_authenticated_action_mask() {
        let body = serde_json::json!({ "version": 1, "nonce": ([7u8; 32]),
            "supported_kinds": 3 });
        let capability: OperationCapability = serde_json::from_value(body.clone()).unwrap();
        assert_eq!(serde_json::to_value(capability).unwrap(), body);
    }
    #[test]
    fn operation_completion_rejects_unknown_action_bits() {
        let body = serde_json::json!({ "enabled": true, "domain": "group_control",
            "next": null, "digest": ([0u8; 32]), "operation": {
                "version": 1, "nonce": ([7u8; 32]), "supported_kinds": 16384 } });
        assert!(!valid_complete(&serde_json::to_vec(&body).unwrap()));
    }
    #[test]
    fn legacy_operation_completion_stays_byte_compatible_without_action_mask() {
        let capability = format!(
            "{{\"version\":1,\"nonce\":[{}]}}",
            std::iter::repeat_n("7", 32).collect::<Vec<_>>().join(",")
        );
        let parsed: OperationCapability = serde_json::from_str(&capability).unwrap();
        assert_eq!(serde_json::to_vec(&parsed).unwrap(), capability.as_bytes());
    }
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
    fn personal_continuation_requires_v7_without_rejecting_legacy_role_continuations() {
        let mut request = DocumentRequest {
            version: 5,
            account_id: "a".repeat(32),
            domain: AccountDocumentDomain::GroupControl,
            after: Some(AccountDocumentKey::GroupControl {
                channel: "b".repeat(64),
                source_device: "c".repeat(32),
            }),
            known_digest: [0; 32],
            operation: Some(OperationProbe {
                version: 1,
                nonce: [7; 32],
                after: None,
                known_digest: [0; 32],
                asset: None,
                legacy_leave: false,
                personal: true,
            }),
        };
        let legacy = request.encode();
        assert_eq!(DocumentRequest::decode(&legacy).unwrap().encode(), legacy);
        request.after = Some(AccountDocumentKey::ConversationPersonal {
            conversation: DocumentConversation::Account("d".repeat(32)),
            action_id: "e".repeat(64),
        });
        assert!(request.after.as_ref().unwrap().valid());
        assert!(request.operation.as_ref().unwrap().supported());
        assert!(
            DocumentRequest::decode(&request.encode()).is_none(),
            "a personal probe cannot upgrade a v5 continuation's permitted key scope",
        );
        request.version = 7;
        let personal = request.encode();
        let decoded = DocumentRequest::decode(&personal)
            .expect("v7 with supported personal capability must retain its personal continuation");
        assert_eq!(decoded.after, request.after);
        assert_eq!(decoded.encode(), personal);
    }

    #[test]
    fn execution_asset_request_keeps_its_bytes_when_result_mode_is_absent() {
        #[derive(Serialize)]
        struct LegacyAsset {
            id: crate::node::AccountGroupOperationId,
            group: crate::eventlog::ConversationId,
            digest: [u8; 32],
            file_conv: [u8; 32],
            start_chunk: u32,
        }
        let old = LegacyAsset {
            id: crate::node::AccountGroupOperationId::new([1; 32]),
            group: crate::eventlog::ConversationId::new([2; 32]),
            digest: [3; 32],
            file_conv: [4; 32],
            start_chunk: 5,
        };
        let before = serde_json::to_vec(&old).unwrap();
        let mut decoded: GroupFileAssetRequest = serde_json::from_slice(&before).unwrap();
        assert!(!decoded.result);
        assert_eq!(serde_json::to_vec(&decoded).unwrap(), before);
        decoded.result = true;
        assert!(
            serde_json::from_slice::<GroupFileAssetRequest>(&serde_json::to_vec(&decoded).unwrap())
                .unwrap()
                .result
        );
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
