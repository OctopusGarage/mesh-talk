use super::{Cursor, Scope};

#[derive(Clone)]
pub(super) struct Pending {
    pub token: String,
    pub after: Cursor,
    pub next: Cursor,
    pub generation: u64,
    pub profiles: ProfilePosition,
}
/// Current values are paged again after renderer replacement or a revision
/// change. Advancing this cursor requires the same renderer ACK as the page.
#[derive(Clone, Default)]
pub(super) struct ProfilePosition {
    pub revision: Option<u64>,
    pub after: Option<String>,
    pub complete: bool,
}
impl ProfilePosition {
    pub fn observe(&mut self, revision: u64) {
        if self.revision != Some(revision) {
            *self = Self {
                revision: Some(revision),
                ..Self::default()
            };
        }
    }
}
pub(super) struct Gate {
    pub scope: Scope,
    pub generation: u64,
    pub pending: Option<Pending>,
    pub last_ack: Option<Pending>,
    pub profiles: ProfilePosition,
    wake_outstanding: bool,
}
impl Gate {
    pub fn new(scope: Scope) -> Self {
        Self {
            scope,
            generation: 1,
            pending: None,
            last_ack: None,
            profiles: ProfilePosition::default(),
            wake_outstanding: false,
        }
    }
    pub fn invalidate(&mut self) -> bool {
        self.generation = self.generation.wrapping_add(1);
        if self.wake_outstanding {
            false
        } else {
            self.wake_outstanding = true;
            true
        }
    }
    pub fn emitted(&mut self, success: bool) {
        if !success {
            self.wake_outstanding = false;
        }
    }
    pub fn begin(&mut self, reset: bool) -> Result<(), &'static str> {
        if self.pending.is_some() && !reset {
            return Err("reception page is awaiting acknowledgement");
        }
        self.pending = None;
        if reset {
            self.profiles = ProfilePosition::default();
        }
        self.wake_outstanding = true;
        Ok(())
    }
    pub fn acknowledge(&mut self, pending: Pending, more: bool) -> bool {
        let dirty = more || !pending.profiles.complete || self.generation != pending.generation;
        self.pending = None;
        self.profiles = pending.profiles.clone();
        self.last_ack = Some(pending);
        self.wake_outstanding = dirty;
        dirty
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn scope() -> Scope {
        Scope {
            owner: "local".into(),
            account_id: "account".into(),
            device_id: "device".into(),
            runtime_id: "ticket".into(),
        }
    }
    fn pending(generation: u64) -> Pending {
        Pending {
            token: "page".into(),
            after: Cursor {
                epoch: "00".repeat(16),
                sequence: "0".into(),
            },
            next: Cursor {
                epoch: "00".repeat(16),
                sequence: "1".into(),
            },
            generation,
            profiles: ProfilePosition {
                complete: true,
                ..Default::default()
            },
        }
    }
    #[test]
    fn stalled_consumer_has_one_wake_and_one_page_reservation() {
        let mut gate = Gate::new(scope());
        let mut wakes = 0;
        for _ in 0..100_000 {
            wakes += usize::from(gate.invalidate());
        }
        assert_eq!(wakes, 1);
        gate.begin(false).unwrap();
        gate.pending = Some(pending(gate.generation));
        assert!(gate.begin(false).is_err());
        let observed = gate.pending.clone().unwrap();
        assert!(!gate.acknowledge(observed, false));
        assert!(gate.invalidate());
        assert!(!gate.invalidate());
    }
    #[test]
    fn changes_during_a_page_continue_drain_without_another_ipc_wake() {
        let mut gate = Gate::new(scope());
        assert!(gate.invalidate());
        let page = pending(gate.generation);
        assert!(!gate.invalidate());
        assert!(gate.acknowledge(page, false));
        assert!(!gate.invalidate());
    }
    #[test]
    fn renderer_reload_can_replace_an_unacknowledged_page_without_replacing_scope() {
        let mut gate = Gate::new(scope());
        gate.pending = Some(pending(1));
        assert!(gate.begin(false).is_err());
        gate.begin(true).unwrap();
        assert!(gate.pending.is_none());
        assert_eq!(gate.scope.runtime_id, "ticket");
    }
    #[test]
    fn profile_pages_advance_only_after_ack_and_restart_on_revision_or_renderer_change() {
        let mut gate = Gate::new(scope());
        gate.profiles.observe(10);
        let mut page = pending(gate.generation);
        page.profiles = ProfilePosition {
            revision: Some(10),
            after: Some("account-16".into()),
            complete: false,
        };
        gate.pending = Some(page.clone());
        assert!(gate.profiles.after.is_none());
        assert!(gate.acknowledge(page, false));
        assert_eq!(gate.profiles.after.as_deref(), Some("account-16"));
        gate.profiles.observe(11);
        assert!(gate.profiles.after.is_none());
        assert!(!gate.profiles.complete);
        gate.profiles.complete = true;
        gate.begin(true).unwrap();
        assert!(gate.profiles.revision.is_none());
    }
}
