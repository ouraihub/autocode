//! Provider routing snapshot used by Pi, OMP and DSH launchers.
//!
//! The snapshot is immutable for one launch. Refreshing settings therefore
//! cannot mutate an already-running process or mix credentials between chats.

use std::collections::HashMap;

use model_provider::{BackendTarget, ConfigSource, ModelSelection, ProviderSnapshot, RoutingMode};

#[derive(Debug, Clone, PartialEq)]
pub struct RouteModel {
    pub provider_id: String,
    pub model_id: String,
    pub model_ref: String,
    pub base_url: String,
    pub routing: RoutingMode,
    pub credential_refs: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct RouteSnapshot {
    pub revision: u64,
    pub source: ConfigSource,
    pub models: HashMap<String, RouteModel>,
}

impl RouteSnapshot {
    pub fn rotate(&self, next: Self) -> Self { next }
    pub fn is_stale(&self, revision: u64) -> bool { self.revision != revision }
    pub fn from_provider(provider: &ProviderSnapshot, source: ConfigSource) -> Self {
        let base_url = provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.protocol == provider.default_protocol)
            .map(|endpoint| endpoint.base_url.clone())
            .unwrap_or_default();
        let credential_refs: Vec<String> = provider.keys.iter().filter(|key| key.enabled).map(|key| key.credential_ref.clone()).collect();
        let models = provider.models.iter().filter(|model| model.enabled).map(|model| {
            let model_ref = format!("{}/{}", provider.id, model.model_id);
            (model.model_id.clone(), RouteModel { provider_id: provider.id.clone(), model_id: model.model_id.clone(), model_ref, base_url: base_url.clone(), routing: provider.routing, credential_refs: credential_refs.clone() })
        }).collect();
        Self { revision: provider.revision, source, models }
    }

    pub fn select(&self, selection: Option<&ModelSelection>) -> Option<&RouteModel> {
        match selection {
            Some(ModelSelection::Managed { provider_id, model_id }) if provider_id.is_empty() => self.models.get(model_id),
            Some(ModelSelection::Managed { model_id, .. }) => self.models.get(model_id),
            Some(ModelSelection::Native { id }) => self.models.get(id),
            None => self.models.values().next(),
        }
    }
}

pub fn normalize_route_url(raw: &str) -> Result<String, model_provider::ValidationError> {
    model_provider::normalized_base_url(raw)
}

pub fn backend_target_for_harness(id: crate::HarnessId) -> Option<BackendTarget> {
    match id { crate::HarnessId::Pi => Some(BackendTarget::Pi), crate::HarnessId::Omp => Some(BackendTarget::Omp), crate::HarnessId::Dsh => Some(BackendTarget::Dsh), _ => None }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotation_replaces_snapshot_without_mutating_old() {
        let old = RouteSnapshot { revision: 1, ..Default::default() };
        let next = RouteSnapshot { revision: 2, ..Default::default() };
        assert_eq!(old.rotate(next.clone()).revision, 2);
        assert_eq!(old.revision, 1);
        assert!(old.is_stale(2));
    }
}
