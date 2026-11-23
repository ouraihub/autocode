use agent_harness::ReasoningLevel;
use model_provider::{AgentPreferences, BackendTarget, ModelSelection, ModelType, ProviderSnapshot, ThinkingLevel};

#[derive(Default)]
pub struct ManagedCatalog {
    pub providers: Vec<ProviderSnapshot>,
    pub preferences: Vec<AgentPreferences>,
}

impl ManagedCatalog {
    pub fn default_for(&self, target: BackendTarget) -> Option<&AgentPreferences> {
        self.preferences.iter().find(|preferences| preferences.target == target)
    }
    pub fn models(&self) -> Vec<agent_harness::Model> {
        self.providers.iter().filter(|provider| provider.enabled).flat_map(|provider| {
            provider.models.iter().filter(|model| model.enabled && model.model_type == ModelType::Chat).map(move |model| agent_harness::Model {
                id: managed_id(&provider.id, &model.model_id), label: format!("{} · {}", provider.name, if model.name.is_empty() { &model.model_id } else { &model.name }),
                description: Some("Configured model service".into()),
                reasoning_levels: if model.supports_reasoning {
                    ThinkingLevel::ALL.into_iter().filter(|level| model.efforts.contains(level)).map(reasoning_level).collect()
                } else { Vec::new() },
                options: Vec::new(),
            })
        }).collect()
    }
}

fn reasoning_level(level: ThinkingLevel) -> ReasoningLevel {
    match level {
        ThinkingLevel::Off => ReasoningLevel::Off,
        ThinkingLevel::Minimal => ReasoningLevel::Minimal,
        ThinkingLevel::Low => ReasoningLevel::Low,
        ThinkingLevel::Medium => ReasoningLevel::Medium,
        ThinkingLevel::High => ReasoningLevel::High,
        ThinkingLevel::Xhigh => ReasoningLevel::XHigh,
        ThinkingLevel::Max => ReasoningLevel::Max,
    }
}

pub fn managed_id(provider: &str, model: &str) -> String {
    serde_json::to_string(&ModelSelection::Managed { provider_id: provider.into(), model_id: model.into() }).expect("model selection serializes")
}
pub fn selection(id: &str) -> Option<ModelSelection> {
    match serde_json::from_str(id).ok()? { value @ ModelSelection::Managed { .. } => Some(value), _ => None }
}

#[cfg(test)]
mod tests {
    use super::*;
    use model_provider::{AuthMode, Protocol, ProviderModel, RoutingMode};

    #[test]
    fn chat_catalog_excludes_non_chat_and_exposes_only_declared_reasoning() {
        let model = ProviderModel {
            model_id: "chat".into(), model_type: ModelType::Chat, name: "Chat".into(),
            protocols: vec![Protocol::OpenaiChat], context_window: None, max_output_tokens: None,
            supports_vision: false, supports_tools: true, supports_reasoning: true,
            efforts: vec![ThinkingLevel::Max, ThinkingLevel::Off, ThinkingLevel::Low], enabled: true,
        };
        let mut embedding = model.clone();
        embedding.model_id = "embedding".into();
        embedding.model_type = ModelType::Embedding;
        let mut plain = model.clone();
        plain.model_id = "plain".into();
        plain.supports_reasoning = false;
        plain.efforts.clear();
        let mut disabled = model.clone();
        disabled.model_id = "disabled".into();
        disabled.enabled = false;
        let catalog = ManagedCatalog {
            providers: vec![ProviderSnapshot {
                id: "provider".into(), preset_id: None, revision: 1, name: "Provider".into(), enabled: true,
                routing: RoutingMode::Direct, auth: AuthMode::None, default_protocol: Protocol::OpenaiChat,
                endpoints: vec![], models: vec![model, embedding, plain, disabled], keys: vec![], headers: vec![],
                sort: 0, created_at: 0, updated_at: 0,
            }],
            preferences: vec![],
        };
        let models = catalog.models();
        assert_eq!(models.iter().map(|model| model.id.as_str()).collect::<Vec<_>>(), vec![
            managed_id("provider", "chat"), managed_id("provider", "plain"),
        ]);
        assert_eq!(models[0].reasoning_levels, vec![ReasoningLevel::Off, ReasoningLevel::Low, ReasoningLevel::Max]);
        assert!(models[1].reasoning_levels.is_empty());
    }
}
