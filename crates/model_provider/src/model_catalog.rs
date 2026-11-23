//! Model grouping and capability hints inferred from identifiers, without I/O.

use crate::ModelType;

/// Identifier prefix before the first `-`, `/` or `:`; empty prefixes are shown as “其他”.
pub fn model_group(model_id: &str) -> &str {
    model_id.split(['-', '/', ':']).next().unwrap_or_default()
}

/// Capability hints inferred from an identifier. Reasoning is never guessed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelHints {
    pub context_window: Option<u64>,
    pub max_output_tokens: Option<u64>,
    pub supports_vision: bool,
    pub supports_tools: bool,
}

const TYPES: &[(&[&str], ModelType)] = &[
    (&["embed"], ModelType::Embedding),
    (&["rerank"], ModelType::Rerank),
    (&["tts", "speech"], ModelType::Tts),
    (&["whisper", "stt", "transcri"], ModelType::Stt),
    (&["dall-e", "image", "flux", "imagen"], ModelType::Image),
    (&["video", "sora", "veo"], ModelType::Video),
];

fn inferred_type(lowercase_id: &str) -> ModelType {
    TYPES.iter()
        .find(|(words, _)| words.iter().any(|word| lowercase_id.contains(word)))
        .map_or(ModelType::Chat, |&(_, model_type)| model_type)
}

/// First matching type keyword wins; identifiers without one are chat models.
pub fn model_type(model_id: &str) -> ModelType {
    inferred_type(&model_id.to_ascii_lowercase())
}

const FAMILIES: &[(&str, u64, u64, bool)] = &[
    ("claude", 200_000, 8_192, true),
    ("gpt-3.5", 16_385, 4_096, false),
    ("gpt", 128_000, 16_384, true),
    ("gemini", 1_048_576, 8_192, true),
    ("deepseek", 128_000, 8_192, false),
    ("qwen", 131_072, 8_192, false),
    ("glm", 128_000, 4_096, false),
    ("kimi", 131_072, 8_192, false),
    ("moonshot-v1-8k", 8_192, 4_096, false),
    ("moonshot-v1-32k", 32_768, 4_096, false),
    ("moonshot", 131_072, 4_096, false),
];

/// Known family limits and vision/tool hints, matching the reference catalog.
pub fn model_hints(model_id: &str) -> ModelHints {
    let id = model_id.to_ascii_lowercase();
    let chat = inferred_type(&id) == ModelType::Chat;
    let name = id.rsplit('/').next().unwrap_or_default();
    let marked = ["vision", "-vl", "vl-", "4v"].iter().any(|marker| name.contains(marker));
    let family = FAMILIES.iter()
        .find(|(prefix, ..)| name.starts_with(prefix))
        .map(|&(_, context, output, vision)| (context, output, vision))
        .or_else(|| {
            let mut chars = name.chars();
            (chars.next() == Some('o') && chars.next().is_some_and(|character| character.is_ascii_digit()))
                .then_some((200_000, 100_000, true))
        });
    match family {
        Some((context, output, vision)) if chat => ModelHints {
            context_window: Some(context),
            max_output_tokens: Some(output),
            supports_vision: vision || marked,
            supports_tools: true,
        },
        _ => ModelHints {
            context_window: None,
            max_output_tokens: None,
            supports_vision: chat && marked,
            supports_tools: chat,
        },
    }
}
