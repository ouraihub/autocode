use crate::{ModelType, ProviderModel, ThinkingLevel, clamp_effort, model_group, model_hints, model_type};

#[test]
fn legacy_model_json_defaults_metadata_and_new_metadata_round_trips() {
    let mut model: ProviderModel = serde_json::from_str(r#"{
        "model_id":"legacy","name":"Legacy","protocols":["openai-chat"],
        "supports_reasoning":true,"enabled":true
    }"#).unwrap();
    assert_eq!(model.model_type, ModelType::Chat);
    assert!(model.efforts.is_empty());
    model.model_type = ModelType::Unknown;
    model.efforts = vec![ThinkingLevel::Off, ThinkingLevel::High, ThinkingLevel::Max];
    let encoded = serde_json::to_value(&model).unwrap();
    assert_eq!(encoded["model_type"], "unknown");
    assert_eq!(encoded["efforts"], serde_json::json!(["off", "high", "max"]));
    assert_eq!(serde_json::from_value::<ProviderModel>(encoded).unwrap(), model);
}

#[test]
fn identifier_hints_match_reference_without_guessing_reasoning() {
    assert_eq!(model_group("Qwen/Qwen3"), "Qwen");
    assert_eq!(model_group("-unnamed"), "");
    assert_eq!(model_type("SPEECH-embedding"), ModelType::Embedding);
    assert_eq!(model_type("whisper-large"), ModelType::Stt);
    assert_eq!(model_type("some-flux-model"), ModelType::Image);
    let chat = model_hints("vendor/gpt-3.5-turbo");
    assert_eq!(chat.context_window, Some(16_385));
    assert_eq!(chat.max_output_tokens, Some(4_096));
    assert!(chat.supports_tools);
    assert!(!chat.supports_vision);
    let image = model_hints("gpt-image-1");
    assert_eq!(image.context_window, None);
    assert!(!image.supports_tools);
    assert!(!image.supports_vision);
    assert!(model_hints("vendor/qwen-vl-max").supports_vision);
    assert_eq!(model_hints("o3").context_window, Some(200_000));
    assert_eq!(model_hints("omni-unknown").context_window, None);
}

#[test]
fn saved_effort_is_clamped_to_declared_levels_only() {
    let levels = [ThinkingLevel::High, ThinkingLevel::Off, ThinkingLevel::Low];
    assert_eq!(clamp_effort(Some(ThinkingLevel::Medium), &levels), Some(ThinkingLevel::Low));
    assert_eq!(clamp_effort(None, &levels), None);
    assert_eq!(clamp_effort(Some(ThinkingLevel::High), &[]), None);
    assert_eq!(clamp_effort(Some(ThinkingLevel::Low), &[ThinkingLevel::High]), None);
}
