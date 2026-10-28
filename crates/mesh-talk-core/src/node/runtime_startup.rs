//! Concrete startup ownership. Completion covers the root construction future
//! and every admitted blocking child, even when the awaiting host is canceled.
use super::{
    runtime::{RuntimeConfig, RuntimeEvents, RuntimeNetworkOptions},
    runtime_work::{RuntimeWork, WorkPermit},
};
use crate::storage::profile::ProfileStorageLease;
use std::sync::Arc;

pub struct RuntimeStartup<'a> {
    pub(super) config: RuntimeConfig<'a>,
    pub(super) events: RuntimeEvents,
    pub(super) network: RuntimeNetworkOptions,
    pub(super) network_cost: super::NetworkCost,
    pub(super) lease: Option<ProfileStorageLease>,
    pub(super) work: Arc<RuntimeWork>,
    pub(super) root: WorkPermit,
}

impl<'a> RuntimeStartup<'a> {
    pub fn new(config: RuntimeConfig<'a>, events: RuntimeEvents) -> Self {
        let work = Arc::new(RuntimeWork::default());
        let root = work.admit().expect("fresh startup admission");
        Self {
            config,
            events,
            network: RuntimeNetworkOptions::default(),
            network_cost: super::NetworkCost::Unknown,
            lease: None,
            work,
            root,
        }
    }
    pub fn with_network(mut self, network: RuntimeNetworkOptions) -> Self {
        self.network = network;
        self
    }
    pub fn with_network_cost(mut self, cost: super::NetworkCost) -> Self {
        self.network_cost = cost;
        self
    }
    pub fn with_lease(mut self, lease: ProfileStorageLease) -> Self {
        self.lease = Some(lease);
        self
    }
    pub fn completion(&self) -> StartupCompletion {
        StartupCompletion(self.work.clone())
    }
}

/// A host may retain its own replacement guard until this completion resolves.
/// It carries no identity, keystore, connection, callback or runtime authority.
pub struct StartupCompletion(Arc<RuntimeWork>);
impl StartupCompletion {
    pub async fn wait(self) {
        self.0.drain().await;
    }
}
