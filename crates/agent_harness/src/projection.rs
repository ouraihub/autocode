//! Engine-native provider registration. Only environment variable names are written to disk.
use model_provider::{AuthMode, BackendTarget, ModelType, ProviderModel, ProviderSnapshot, ThinkingLevel};
use serde_json::json;

pub fn engine_config(target: BackendTarget, provider: &ProviderSnapshot, model_id: &str, headers: &[(String, String)]) -> Result<String, String> {
    let model = provider.models.iter().find(|model| model.enabled && model.model_id == model_id).ok_or("Configured model is unavailable")?;
    if model.model_type != ModelType::Chat { return Err("Only chat models can be used by an agent".into()); }
    let endpoint = provider.endpoints.iter().find(|endpoint| endpoint.protocol == provider.default_protocol).ok_or("Configured endpoint is unavailable")?;
    if !model.protocols.contains(&provider.default_protocol) { return Err("Model does not support the configured protocol".into()); }
    let route = format!("autocode-{}", provider.id);
    let model = projected_model(target, model);
    let quote = |text: &str| serde_json::to_string(text).map_err(|_| "Could not encode provider metadata".to_string());
    let key = if provider.auth == AuthMode::None { "" } else if target == BackendTarget::Pi { "$AUTOCODE_MANAGED_API_KEY" } else { "AUTOCODE_MANAGED_API_KEY" };
    match target {
        BackendTarget::Pi | BackendTarget::Omp => {
            let header_fields = headers.iter().map(|(name, variable)| {
                Ok(format!("{}: {}", quote(name)?, if target == BackendTarget::Pi { quote(&format!("${{{variable}}}"))? } else { format!("process.env[{}] ?? \"\"", quote(variable)?) }))
            }).collect::<Result<Vec<_>, String>>()?.join(",");
            let auth = if target == BackendTarget::Omp { ", auth: \"apiKey\", authHeader: true" } else { "" };
            Ok(format!("export default function (pi) {{ pi.registerProvider({}, {{ name: {}, baseUrl: {}, api: {}, apiKey: {}, headers: {{{header_fields}}}, models: [{model}]{auth} }}); }}\n",
                quote(&route)?, quote(&provider.name)?, quote(&endpoint.base_url)?, quote(provider.default_protocol.engine_api())?, quote(key)?))
        }
        BackendTarget::Dsh => {
            if !headers.is_empty() { return Err("DSH headers require Gateway mode".into()); }
            // JSON is a YAML subset; using it preserves every user-authored identifier safely.
            let acp = json!({"id":"acp", "config":{"provider":route, "model":model_id}});
            let provider_value = json!({"displayName":provider.name,"api":provider.default_protocol.engine_api(),"baseURL":endpoint.base_url,
                "apiKeyEnv":key,"models":[model]});
            let mut providers = serde_json::Map::new(); providers.insert(route, provider_value);
            serde_json::to_string(&json!([{"id":"llm-pi-ai","config":{"providers":providers}}, acp])).map_err(|_| "Could not encode DSH configuration".into())
        }
    }
}

fn projected_model(target: BackendTarget, model: &ProviderModel) -> serde_json::Value {
    let input = if model.supports_vision { &["text", "image"][..] } else { &["text"][..] };
    let mut value = json!({ "id": model.model_id, "name": if model.name.is_empty() { &model.model_id } else { &model.name },
        "input": input, "contextWindow": model.context_window.unwrap_or(128_000),
        "maxTokens": model.max_output_tokens.unwrap_or(8192) });
    match target {
        BackendTarget::Pi => {
            value["reasoning"] = json!(model.supports_reasoning);
            value["cost"] = json!({ "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 });
            if model.supports_reasoning {
                let levels = ThinkingLevel::ALL.into_iter().map(|level| {
                    (level.as_str().to_owned(), if model.efforts.contains(&level) { json!(level) } else { serde_json::Value::Null })
                }).collect::<serde_json::Map<_, _>>();
                value["thinkingLevelMap"] = levels.into();
            }
        }
        BackendTarget::Omp => {
            value["reasoning"] = json!(model.supports_reasoning);
            // OMP infers thinking mode/range from identity and api. A bare range disables its effort paths.
        }
        BackendTarget::Dsh => {
            value["reasoningEfforts"] = if model.supports_reasoning {
                ThinkingLevel::ALL.into_iter().filter(|level| model.efforts.contains(level)).map(|level| {
                    (level.as_str().to_owned(), if level == ThinkingLevel::Off { serde_json::Value::Null } else { json!(level) })
                }).collect::<serde_json::Map<_, _>>().into()
            } else {
                serde_json::Value::Bool(false)
            };
        }
    }
    value
}

#[cfg(test)]
mod tests;
