//! `ProviderServices`: per-App composition root for the model-service layer.
//!
//! One instance per GPUI `App` (stored as a `Global`): subscribers lose the
//! change broadcast when the App unloads, which matches design §4.3 "no
//! process-static singleton shared across tests".
//!
//! Lifetimes:
//!
//! - `ProviderDb` is cloneable (thread-safe SQLite handle); the service
//!   owns one and hands out clones to background tasks.
//! - `GpuiCredentialBroker` is cloneable (`mpsc::Sender`); the pump `Task`
//!   is held by `ProviderServices` and dropped when the global is torn down.
//! - `ProviderService` is wrapped in `Arc` so `ProviderChange` subscribers
//!   can live on background executors without borrowing `&App`.

use std::{collections::HashMap, sync::{Arc, Mutex}};

use gpui::{App, Global, Task};
use model_provider::ProviderService;

use crate::provider_store::ProviderDb;

use super::broker::{GpuiCredentialBroker, spawn_credential_broker};

/// The App-scoped composition root. Set once by `agent_chat::init`; retrieve
/// from any context via `ProviderServices::global(cx)`.
pub struct ProviderServices {
    service: Arc<ProviderService<ProviderDb, GpuiCredentialBroker>>,
    counters: Arc<Mutex<HashMap<String, usize>>>,
    catalog: std::rc::Rc<std::cell::RefCell<super::catalog::ManagedCatalog>>,
    _catalog_task: Task<()>,
    /// Keeping the pump alive is the whole purpose of this field; it is
    /// never read but dropping it stops the broker. `allow(dead_code)`
    /// because clippy cannot see the Drop effect.
    #[allow(dead_code)]
    pump: Task<()>,
}

impl Global for ProviderServices {}

impl ProviderServices {
    /// Builds the services and wires the broker pump. Called exactly once
    /// from `agent_chat::init`.
    pub fn new(cx: &mut App) -> Self {
        let repo = ProviderDb::global(cx);
        let (broker, pump) = spawn_credential_broker(cx);
        let service = Arc::new(ProviderService::new(repo, broker));
        let catalog = std::rc::Rc::new(std::cell::RefCell::new(super::catalog::ManagedCatalog::default()));
        let service_for_task = service.clone();
        let catalog_for_task = catalog.clone();
        let mut changes = service.subscribe_changes();
        let task = cx.spawn(async move |cx| {
            loop {
                match service_for_task.list_providers().await {
                    Ok(providers) => {
                        let mut preferences = Vec::new();
                        for target in model_provider::BackendTarget::ALL {
                            match service_for_task.load_preferences(target).await {
                                Ok(saved) => preferences.push(saved),
                                Err(_) => log::error!("providers.catalog.preferences_failed target={target:?}"),
                            }
                        }
                        *catalog_for_task.borrow_mut() = super::catalog::ManagedCatalog { providers, preferences };
                        cx.update(|cx| cx.refresh_windows());
                    }
                    Err(_) => log::error!("providers.catalog.load_failed"),
                }
                if matches!(changes.recv().await, Err(tokio::sync::broadcast::error::RecvError::Closed)) { break; }
            }
        });
        Self { service, pump, counters: Arc::new(Mutex::new(HashMap::new())), catalog, _catalog_task: task }
    }

    pub fn install(cx: &mut App) {
        let services = Self::new(cx);
        cx.set_global(services);
    }

    pub fn global(cx: &App) -> &ProviderServices {
        cx.global::<ProviderServices>()
    }

    pub fn try_global(cx: &App) -> Option<&ProviderServices> {
        cx.try_global::<ProviderServices>()
    }

    pub fn service(&self) -> Arc<ProviderService<ProviderDb, GpuiCredentialBroker>> {
        self.service.clone()
    }
    pub fn models(&self) -> Vec<agent_harness::Model> { self.catalog.borrow().models() }
    pub fn default_for(&self, target: model_provider::BackendTarget) -> Option<model_provider::AgentPreferences> { self.catalog.borrow().default_for(target).cloned() }
    pub(crate) fn model_label(&self, provider_id: &str, model_id: &str) -> Option<String> {
        let catalog = self.catalog.borrow();
        let provider = catalog.providers.iter().find(|provider| provider.id == provider_id)?;
        Some(format!("{} · {model_id}", provider.name))
    }

    pub fn managed_request(&self, target: model_provider::BackendTarget, selection: Option<model_provider::ModelSelection>) -> super::managed::ManagedRequest {
        super::managed::ManagedRequest { service: self.service.clone(), counters: self.counters.clone(), target, selection }
    }
}

/// Convenience accessor matching the design-doc spelling `provider_services(cx)`.
pub fn provider_services(cx: &App) -> &ProviderServices {
    ProviderServices::global(cx)
}
