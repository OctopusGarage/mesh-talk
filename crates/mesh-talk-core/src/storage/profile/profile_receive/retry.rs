//! Bounded runtime retry order. Durable tickets identify admission order only.
use crate::eventlog::ConversationId;
use std::collections::VecDeque;

const MAX_SCOPES: usize = 64;
pub(super) type Cursor = (i64, [u8; 32]);

#[derive(Default)]
pub(super) struct Rotation {
    global: Cursor,
    scoped: VecDeque<(ConversationId, Cursor)>,
}
impl Rotation {
    pub(super) fn current(&self, scope: Option<ConversationId>) -> Cursor {
        scope.map_or(self.global, |scope| {
            self.scoped
                .iter()
                .find(|(key, _)| *key == scope)
                .map_or(Cursor::default(), |(_, cursor)| *cursor)
        })
    }
    pub(super) fn advance(&mut self, scope: Option<ConversationId>, cursor: Cursor) {
        let Some(scope) = scope else {
            self.global = cursor;
            return;
        };
        self.scoped.retain(|(key, _)| *key != scope);
        if self.scoped.len() == MAX_SCOPES {
            self.scoped.pop_front();
        }
        self.scoped.push_back((scope, cursor));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scoped_eviction_is_bounded_and_never_resets_global_recovery() {
        let mut rotation = Rotation::default();
        rotation.advance(None, (41, [7; 32]));
        for n in 0..256u16 {
            let mut key = [0; 32];
            key[..2].copy_from_slice(&n.to_le_bytes());
            rotation.advance(Some(ConversationId::new(key)), (i64::from(n), key));
        }
        assert_eq!(rotation.scoped.len(), MAX_SCOPES);
        assert_eq!(rotation.current(None), (41, [7; 32]));
        assert_eq!(
            rotation.current(Some(ConversationId::new([0; 32]))),
            Cursor::default()
        );
    }
}
