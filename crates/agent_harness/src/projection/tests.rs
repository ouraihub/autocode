use super::*;
use model_provider::{EndpointSnapshot, Protocol, RoutingMode};

fn provider() -> ProviderSnapshot {
    ProviderSnapshot {
        id: "metadata".into(), preset_id: None, revision: 1, name: "Metadata".into(), enabled: true,
        routing: RoutingMode::Direct, auth: AuthMode::None, default_protocol: Protocol::OpenaiResponses,
        endpoints: vec![EndpointSnapshot { protocol: Protocol::OpenaiResponses, base_url: "https://provider.test/v1".into() }],
        models: vec![ProviderModel {
            model_id: "reasoning-model".into(), model_type: ModelType::Chat, name: "Reasoning model".into(),
            protocols: vec![Protocol::OpenaiResponses], context_window: Some(200_000), max_output_tokens: Some(8_192),
            supports_vision: true, supports_tools: true, supports_reasoning: true,
            efforts: vec![ThinkingLevel::Off, ThinkingLevel::High], enabled: true,
        }],
        keys: vec![], headers: vec![], sort: 0, created_at: 0, updated_at: 0,
    }
}

#[test]
fn pi_registration_marks_undeclared_thinking_levels_unsupported() {
    let config = engine_config(BackendTarget::Pi, &provider(), "reasoning-model", &[]).unwrap();
    let start = config.find("models: ").unwrap() + "models: ".len();
    let end = config.rfind(']').unwrap() + 1;
    let models: serde_json::Value = serde_json::from_str(&config[start..end]).unwrap();
    assert_eq!(models[0]["thinkingLevelMap"], json!({
        "off":"off", "minimal":null, "low":null, "medium":null, "high":"high", "xhigh":null, "max":null
    }));
    assert_eq!(models[0]["contextWindow"], 200_000);
}

#[test]
fn dsh_registration_uses_declared_efforts_and_false_for_non_reasoning() {
    let mut provider = provider();
    let config = engine_config(BackendTarget::Dsh, &provider, "reasoning-model", &[]).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    let model = &config[0]["config"]["providers"]["autocode-metadata"]["models"][0];
    assert_eq!(model["reasoningEfforts"], json!({"off":null, "high":"high"}));
    assert!(model.get("thinkingLevelMap").is_none());
    provider.models[0].supports_reasoning = false;
    provider.models[0].efforts.clear();
    let config = engine_config(BackendTarget::Dsh, &provider, "reasoning-model", &[]).unwrap();
    let config: serde_json::Value = serde_json::from_str(&config).unwrap();
    assert_eq!(config[0]["config"]["providers"]["autocode-metadata"]["models"][0]["reasoningEfforts"], false);
}

#[test]
fn non_chat_models_cannot_launch() {
    let mut provider = provider();
    provider.models[0].model_type = ModelType::Embedding;
    for target in [BackendTarget::Pi, BackendTarget::Omp, BackendTarget::Dsh] {
        assert!(engine_config(target, &provider, "reasoning-model", &[]).is_err());
    }
}
