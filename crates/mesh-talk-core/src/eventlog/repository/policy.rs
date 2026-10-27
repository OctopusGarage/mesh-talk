//! Narrow role-policy identities and exact scope lookup, never body reads.
use super::*;
#[derive(Clone, Copy, PartialEq, Eq)]
struct Binding {
    store_uuid: [u8; 16],
    domain: EventDomainId,
    conversation: ConversationId,
    epoch: i64,
}
pub(crate) struct RepositoryPolicyView {
    binding: Binding,
    _lease: RepositoryOwner,
}
impl EventRepository {
    pub(crate) fn event_conversation_on(
        &self,
        c: &Connection,
        id: &EventId,
    ) -> Result<Option<ConversationId>, LogError> {
        let value: Option<Vec<u8>> = c
            .query_row(
                "SELECT conversation FROM retained_events WHERE domain=?1 AND id=?2",
                params![self.domain.0.as_slice(), id.as_bytes().as_slice()],
                |r| r.get(0),
            )
            .optional()?;
        value
            .map(|v| {
                Ok(ConversationId::new(
                    v.try_into().map_err(|_| corrupt("invalid event scope"))?,
                ))
            })
            .transpose()
    }
    pub(crate) fn policy_view_on(
        &self,
        c: &Connection,
        conv: &ConversationId,
    ) -> Result<RepositoryPolicyView, LogError> {
        self.require_retirement_connection_on(c)?;
        self.require_active_on(c, conv)?;
        Ok(RepositoryPolicyView {
            binding: Binding {
                store_uuid: self.store_uuid,
                domain: self.domain,
                conversation: *conv,
                epoch: self.network_epoch_on(c, conv)?,
            },
            _lease: self.database.clone(),
        })
    }
    pub(crate) fn validate_policy_view_on(
        &self,
        c: &Connection,
        view: &RepositoryPolicyView,
        conv: &ConversationId,
    ) -> Result<(), LogError> {
        self.require_retirement_connection_on(c)?;
        let expected = Binding {
            store_uuid: self.store_uuid,
            domain: self.domain,
            conversation: *conv,
            epoch: self.network_epoch_on(c, conv)?,
        };
        if expected != view.binding || self.scope_on(c, conv)?.is_some_and(|s| s.lifecycle == 1) {
            return Err(LogError::StaleCursor);
        }
        Ok(())
    }
}
