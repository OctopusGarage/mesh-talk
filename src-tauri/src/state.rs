use crate::api::AppConfig;
use crate::domain::models::User;
use crate::network::runtime::NetworkRuntime;
use crate::services::auth_service::AuthService;
use crate::services::contact_service::ContactService;
use crate::services::message_service::MessageService;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub token: String,
    pub user: User,
}

#[derive(Clone, Default)]
pub struct SessionState {
    inner: Arc<Mutex<Option<SessionInfo>>>,
}

impl SessionState {
    pub fn set(&self, token: String, user: User) {
        let mut guard = self.inner.lock().unwrap();
        *guard = Some(SessionInfo { token, user });
    }

    pub fn clear(&self) {
        let mut guard = self.inner.lock().unwrap();
        *guard = None;
    }

    pub fn get(&self) -> Option<SessionInfo> {
        self.inner.lock().unwrap().clone()
    }
}

#[derive(Clone)]
pub struct AppState {
    auth_service: AuthService,
    contact_service: ContactService,
    message_service: MessageService,
    session_state: SessionState,
    app_config: Arc<AppConfig>,
    network_started: Arc<AtomicBool>,
    network_runtime: Arc<Mutex<Option<NetworkRuntime>>>,
}

impl AppState {
    pub fn new(
        auth_service: AuthService,
        contact_service: ContactService,
        message_service: MessageService,
        app_config: Arc<AppConfig>,
    ) -> Self {
        Self {
            auth_service,
            contact_service,
            message_service,
            session_state: SessionState::default(),
            app_config,
            network_started: Arc::new(AtomicBool::new(false)),
            network_runtime: Arc::new(Mutex::new(None)),
        }
    }

    pub fn auth_service(&self) -> &AuthService {
        &self.auth_service
    }

    pub fn contact_service(&self) -> &ContactService {
        &self.contact_service
    }

    pub fn message_service(&self) -> &MessageService {
        &self.message_service
    }

    pub fn session(&self) -> &SessionState {
        &self.session_state
    }

    pub fn app_config(&self) -> Arc<AppConfig> {
        Arc::clone(&self.app_config)
    }

    pub fn network_started(&self) -> bool {
        self.network_started.load(Ordering::SeqCst)
    }

    pub fn try_start_network(&self) -> bool {
        self.network_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
    }

    pub fn reset_network_started(&self) {
        self.network_started.store(false, Ordering::SeqCst);
    }

    pub fn set_network_runtime(&self, runtime: NetworkRuntime) {
        let mut guard = self.network_runtime.lock().unwrap();
        *guard = Some(runtime);
    }

    pub fn take_network_runtime(&self) -> Option<NetworkRuntime> {
        let mut guard = self.network_runtime.lock().unwrap();
        guard.take()
    }

    pub fn contact_request_service(&self) -> Arc<crate::contacts::service::ContactRequestService> {
        crate::services::contact_request_service::ContactRequestService::global().clone()
    }
}
