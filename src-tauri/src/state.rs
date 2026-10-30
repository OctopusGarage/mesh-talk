use crate::services::auth_service::AuthService;
use std::sync::{Arc, Mutex};

#[cfg(test)]
type FileProgressHook = Arc<Mutex<Option<Box<dyn FnOnce() + Send + Sync>>>>;
#[cfg(test)]
type HostOperationGate = Arc<
    Mutex<
        Option<(
            tokio::sync::oneshot::Sender<()>,
            tokio::sync::oneshot::Receiver<()>,
        )>,
    >,
>;

pub(crate) use crate::desktop_session::SessionLease;
pub use crate::desktop_session::{SessionInfo, SessionState};

/// Managed Tauri state: the auth service + the current in-memory session. (The legacy
/// contact/message/network services were retired with the legacy stack; the
/// node runs out of [`crate::chat_commands::NodeState`].)
#[derive(Clone)]
pub struct AppState {
    pub(crate) enrollment: crate::device_enrollment::EnrollmentState,
    pub(crate) drafts: Arc<Mutex<Option<crate::drafts::DraftStorage>>>,
    app_lock: crate::app_lock::AppLock,
    #[cfg(test)]
    pub(crate) rename_hot_gate: HostOperationGate,
    #[cfg(test)]
    pub(crate) logout_retirement_gate: HostOperationGate,
    #[cfg(test)]
    pub(crate) auto_admission_gate: HostOperationGate,
    #[cfg(test)]
    pub(crate) owner_command_captured: Arc<Mutex<Option<std::sync::mpsc::Sender<&'static str>>>>,
    #[cfg(test)]
    pub(crate) owner_file_progress_hook: FileProgressHook,
    pub(crate) integrations: crate::integrations::local_access::DesktopIntegrations,
    auth_service: AuthService,
    badge_revision: Arc<Mutex<BadgeRevision>>,
    pub(crate) auth_operation: Arc<tokio::sync::Mutex<()>>,
    /// Serializes credential read/modify/write work without occupying session
    /// or app-lock authority during key derivation and filesystem staging.
    pub(crate) credential_storage: Arc<tokio::sync::Mutex<()>>,
}

impl AppState {
    pub fn new(auth_service: AuthService) -> Self {
        Self {
            enrollment: crate::device_enrollment::EnrollmentState::default(),
            drafts: Arc::new(Mutex::new(None)),
            app_lock: crate::app_lock::AppLock::default(),
            #[cfg(test)]
            rename_hot_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            logout_retirement_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            auto_admission_gate: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            owner_command_captured: Arc::new(Mutex::new(None)),
            #[cfg(test)]
            owner_file_progress_hook: Arc::new(Mutex::new(None)),
            integrations: crate::integrations::local_access::DesktopIntegrations::default(),
            auth_service,
            badge_revision: Arc::new(Mutex::new(BadgeRevision::default())),
            auth_operation: Arc::new(tokio::sync::Mutex::new(())),
            credential_storage: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn auth_service(&self) -> &AuthService {
        &self.auth_service
    }

    pub fn app_lock(&self) -> &crate::app_lock::AppLock {
        &self.app_lock
    }

    pub fn session(&self) -> &SessionState {
        self.auth_service.session()
    }

    pub(crate) fn admit_badge_revision(&self, generation: u64, revision: u64) -> bool {
        self.badge_revision
            .lock()
            .is_ok_and(|mut badge| badge.admit(generation, revision))
    }
}

#[derive(Default)]
struct BadgeRevision {
    generation: u64,
    latest: u64,
}

impl BadgeRevision {
    fn admit(&mut self, generation: u64, revision: u64) -> bool {
        if self.generation != generation {
            self.generation = generation;
            self.latest = 0;
        }
        if revision <= self.latest {
            return false;
        }
        self.latest = revision;
        true
    }
}

#[cfg(test)]
mod badge_revision_tests {
    use super::BadgeRevision;

    #[test]
    fn older_same_session_badge_updates_cannot_repaint_after_read() {
        let mut badge = BadgeRevision::default();
        assert!(badge.admit(1, 100));
        assert!(badge.admit(1, 102));
        assert!(!badge.admit(1, 101));
        assert!(badge.admit(2, 1));
    }
}
