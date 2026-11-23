use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, Result};
use futures::io::AsyncReadExt;
use http_client::{AsyncBody, HttpClient, HttpRequestExt, RequestBuilderExt};
use model_provider::{normalized_base_url, AuthMode, CredentialStore, Protocol, ProviderModel, ProviderService, ProviderSnapshot};

/// The outcome of checking one enabled credential, or one unauthenticated provider.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyCheck {
    pub key_id: Option<String>,
    pub label: String,
    pub count: Option<usize>,
    pub error: Option<String>,
}

const MAX_BODY: usize = 2 * 1024 * 1024;

/// Checks every enabled key sequentially, retaining failures alongside successes.
pub async fn check_provider(
    service: &ProviderService<impl model_provider::ProviderRepository, impl CredentialStore>,
    http: Arc<dyn HttpClient>,
    provider: &ProviderSnapshot,
    protocol: Protocol,
) -> Vec<KeyCheck> {
    let mut checks = Vec::new();
    if provider.auth == AuthMode::ApiKey {
        for key in provider.keys.iter().filter(|key| key.enabled) {
            let result = discover_with_key(service, http.clone(), provider, protocol, Some(key)).await;
            let (count, error) = match result {
                Ok(models) => (Some(models.len()), None),
                Err(error) => (None, Some(error.to_string())),
            };
            checks.push(KeyCheck {
                key_id: Some(key.id.clone()),
                label: key.label.clone().unwrap_or_else(|| key.masked.clone()),
                count,
                error,
            });
        }
        if checks.is_empty() {
            checks.push(KeyCheck { key_id: None, label: String::new(), count: None, error: Some("provider has no enabled API key".into()) });
        }
    } else {
        let result = discover_with_key(service, http, provider, protocol, None).await;
        let (count, error) = match result {
            Ok(models) => (Some(models.len()), None),
            Err(error) => (None, Some(error.to_string())),
        };
        checks.push(KeyCheck { key_id: None, label: String::new(), count, error });
    }
    checks
}

/// Builds the model-list URL used by discovery and its settings preview.
///
/// # Errors
/// Returns a safe error when the provider endpoint is invalid.
pub fn models_url(base: &str) -> Result<String> {
    let base = normalized_base_url(base).map_err(|_| anyhow!("provider endpoint is invalid"))?;
    let base = base.trim_end_matches('/');
    Ok(if base.ends_with("/models") {
        base.to_owned()
    } else if base.ends_with("/v1") {
        format!("{base}/models")
    } else {
        format!("{base}/v1/models")
    })
}

pub async fn discover_models(
    service: &ProviderService<impl model_provider::ProviderRepository, impl CredentialStore>,
    http: Arc<dyn HttpClient>,
    provider: &ProviderSnapshot,
    protocol: Protocol,
) -> Result<Vec<ProviderModel>> {
    let key = provider.keys.iter().find(|key| key.enabled);
    discover_with_key(service, http, provider, protocol, key).await
}

async fn discover_with_key(
    service: &ProviderService<impl model_provider::ProviderRepository, impl CredentialStore>,
    http: Arc<dyn HttpClient>,
    provider: &ProviderSnapshot,
    protocol: Protocol,
    key: Option<&model_provider::KeySummary>,
) -> Result<Vec<ProviderModel>> {
    let endpoint = provider
        .endpoints
        .iter()
        .find(|endpoint| endpoint.protocol == protocol)
        .ok_or_else(|| anyhow!("provider has no endpoint for this protocol"))?;
    let url = models_url(&endpoint.base_url)?;

    let mut request = http_client::Request::builder()
        .method(http_client::http::Method::GET)
        .uri(url.as_str())
        .timeout(Duration::from_secs(20));
    let mut headers = Vec::new();
    if provider.auth == AuthMode::ApiKey {
        let key = key.ok_or_else(|| anyhow!("provider has no enabled API key"))?;
        let secret = service.credentials().get(&key.credential_ref).await
            .map_err(|_| anyhow!("provider API key is unavailable"))?
            .ok_or_else(|| anyhow!("provider API key is unavailable"))?;
        let (name, value) = if protocol == Protocol::AnthropicMessages {
            (http_client::http::HeaderName::from_static("x-api-key"), http_client::http::HeaderValue::from_str(secret.expose()))
        } else {
            (http_client::http::HeaderName::from_static("authorization"), http_client::http::HeaderValue::from_str(&format!("Bearer {}", secret.expose())))
        };
        headers.push((name, value.map_err(|_| anyhow!("provider API key is invalid"))?));
    }
    if protocol == Protocol::AnthropicMessages {
        headers.push((http_client::http::HeaderName::from_static("anthropic-version"), http_client::http::HeaderValue::from_static("2023-06-01")));
    }
    for header in provider.headers.iter().filter(|header| header.enabled) {
        let secret = service
            .credentials()
            .get(&header.credential_ref)
            .await
            .map_err(|_| anyhow!("provider header is unavailable"))?
            .ok_or_else(|| anyhow!("provider header is unavailable"))?;
        let name = http_client::http::HeaderName::try_from(header.name.as_str())
            .map_err(|_| anyhow!("provider header name is invalid"))?;
        let value = http_client::http::HeaderValue::from_str(secret.expose())
            .map_err(|_| anyhow!("provider header value is invalid"))?;
        headers.push((name, value));
    }
    let custom = http_client::CustomHeaders::new(headers);
    request = request.extra_headers(&custom);
    let response = http.send(request.body(AsyncBody::empty()).map_err(|_| anyhow!("could not build provider request"))?).await
        .map_err(|_| anyhow!("provider connection failed"))?;
    let status = response.status();
    if !status.is_success() {
        return Err(anyhow!("provider returned HTTP status {}", status.as_u16()));
    }
    let mut body = Vec::new();
    response.into_body().take((MAX_BODY + 1) as u64).read_to_end(&mut body).await
        .map_err(|_| anyhow!("provider response could not be read"))?;
    if body.len() > MAX_BODY {
        return Err(anyhow!("provider response is too large"));
    }
    let value: serde_json::Value = serde_json::from_slice(&body)
        .map_err(|_| anyhow!("provider response is not valid JSON"))?;
    let data = value.get("data").and_then(|value| value.as_array())
        .ok_or_else(|| anyhow!("provider response has no model list"))?;
    Ok(data.iter().filter_map(|model| {
        let id = model.get("id")?.as_str()?.to_owned();
        let name = model.get("display_name").or_else(|| model.get("name")).and_then(|name| name.as_str()).unwrap_or(&id).to_owned();
        let hints = model_provider::model_hints(&id);
        Some(ProviderModel {
            model_type: model_provider::model_type(&id),
            model_id: id,
            name,
            protocols: vec![protocol],
            context_window: model.get("context_window").and_then(serde_json::Value::as_u64).filter(|value| *value > 0).or(hints.context_window),
            max_output_tokens: hints.max_output_tokens,
            supports_vision: hints.supports_vision,
            supports_tools: hints.supports_tools,
            supports_reasoning: false,
            efforts: Vec::new(),
            enabled: true,
        })
    }).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider_store::ProviderDb;
    use model_provider::{EndpointSnapshot, HeaderSummary, InMemoryVault, KeySummary, RoutingMode};

    #[gpui::test]
    async fn saved_provider_probe_uses_vault_auth_and_parses_models(_: &mut gpui::TestAppContext) {
        let db = ProviderDb::open_test_db("provider_probe_auth").await;
        let vault = InMemoryVault::new();
        vault.put("key-ref", model_provider::Secret::new("test-only-key")).await.unwrap();
        vault.put("header-ref", model_provider::Secret::new("private-header")).await.unwrap();
        let service = ProviderService::new(db, vault);
        for (protocol, endpoint, expected) in [
            (Protocol::OpenaiChat, "https://provider.test/", "https://provider.test/v1/models"),
            (Protocol::OpenaiResponses, "https://provider.test/v1/", "https://provider.test/v1/models"),
            (Protocol::AnthropicMessages, "https://provider.test/v1/models", "https://provider.test/v1/models"),
        ] {
            let provider = ProviderSnapshot {
                id: "probe".into(), preset_id: None, revision: 1, name: "Probe".into(), enabled: true,
                routing: RoutingMode::Direct, auth: AuthMode::ApiKey, default_protocol: protocol,
                endpoints: vec![EndpointSnapshot { protocol, base_url: endpoint.into() }], models: vec![],
                keys: vec![KeySummary { id: "k".into(), label: None, masked: "***".into(), credential_ref: "key-ref".into(), enabled: true, sort: 0 }],
                headers: vec![HeaderSummary { id: "h".into(), name: "X-Private".into(), masked: "***".into(), credential_ref: "header-ref".into(), enabled: true }],
                sort: 0, created_at: 0, updated_at: 0,
            };
            let http = http_client::FakeHttpClient::create(move |request| async move {
                assert_eq!(request.uri().to_string(), expected);
                let values = request.headers().iter().map(|(name, value)| (name.as_str(), value.to_str().unwrap())).collect::<Vec<_>>();
                assert!(values.contains(&("x-private", "private-header")));
                if protocol == Protocol::AnthropicMessages {
                    assert!(values.contains(&("x-api-key", "test-only-key")));
                } else {
                    assert!(values.contains(&("authorization", "Bearer test-only-key")));
                }
                Ok(http_client::Response::builder().status(200).body(r#"{"data":[{"id":"m1","display_name":"Model One"},{"id":"vendor/gpt-4o","context_window":250000},{"id":"text-embedding-3"}]}"#.into()).unwrap())
            });
            let models = discover_models(&service, http, &provider, protocol).await.unwrap();
            assert_eq!(models[0].model_id, "m1");
            assert_eq!(models[0].name, "Model One");
            assert_eq!(models[0].model_type, model_provider::ModelType::Chat);
            assert!(models[0].supports_tools);
            assert_eq!(models[1].context_window, Some(250_000));
            assert!(models[1].supports_vision);
            assert!(!models[1].supports_reasoning);
            assert!(models[1].efforts.is_empty());
            assert_eq!(models[2].model_type, model_provider::ModelType::Embedding);
            assert!(!models[2].supports_tools);
            assert_eq!(models[2].context_window, None);
        }
    }

    #[gpui::test]
    async fn checks_every_enabled_key_and_unauthenticated_provider(_: &mut gpui::TestAppContext) {
        let db = ProviderDb::open_test_db("provider_probe_all_keys").await;
        let vault = InMemoryVault::new();
        vault.put("bad-ref", model_provider::Secret::new("bad-key")).await.unwrap();
        vault.put("good-ref", model_provider::Secret::new("good-key")).await.unwrap();
        let service = ProviderService::new(db, vault);
        let mut provider = ProviderSnapshot {
            id: "probe".into(), preset_id: None, revision: 1, name: "Probe".into(), enabled: true,
            routing: RoutingMode::Direct, auth: AuthMode::ApiKey, default_protocol: Protocol::OpenaiChat,
            endpoints: vec![EndpointSnapshot { protocol: Protocol::OpenaiChat, base_url: "https://provider.test".into() }],
            models: vec![], headers: vec![], sort: 0, created_at: 0, updated_at: 0,
            keys: vec![
                KeySummary { id: "bad".into(), label: Some("First".into()), masked: "***1".into(), credential_ref: "bad-ref".into(), enabled: true, sort: 0 },
                KeySummary { id: "good".into(), label: None, masked: "***2".into(), credential_ref: "good-ref".into(), enabled: true, sort: 1 },
                KeySummary { id: "disabled".into(), label: None, masked: "***3".into(), credential_ref: "missing-ref".into(), enabled: false, sort: 2 },
            ],
        };
        let requests = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = requests.clone();
        let http = http_client::FakeHttpClient::create(move |request| {
            let auth = request.headers().get("authorization").map(|value| value.to_str().unwrap().to_owned());
            observed.lock().unwrap().push(auth.clone());
            async move {
                let (status, body) = if auth.as_deref() == Some("Bearer bad-key") {
                    (401, "private upstream bad-key details")
                } else {
                    (200, r#"{"data":[{"id":"model-one"}]}"#)
                };
                Ok(http_client::Response::builder().status(status).body(body.into()).unwrap())
            }
        });
        let checks = check_provider(&service, http.clone(), &provider, Protocol::OpenaiChat).await;
        assert_eq!(checks, vec![
            KeyCheck { key_id: Some("bad".into()), label: "First".into(), count: None, error: Some("provider returned HTTP status 401".into()) },
            KeyCheck { key_id: Some("good".into()), label: "***2".into(), count: Some(1), error: None },
        ]);
        assert_eq!(*requests.lock().unwrap(), vec![Some("Bearer bad-key".to_owned()), Some("Bearer good-key".to_owned())]);

        provider.auth = AuthMode::None;
        provider.keys.clear();
        let checks = check_provider(&service, http.clone(), &provider, Protocol::OpenaiChat).await;
        assert_eq!(checks, vec![KeyCheck { key_id: None, label: String::new(), count: Some(1), error: None }]);
        assert_eq!(*requests.lock().unwrap(), vec![Some("Bearer bad-key".to_owned()), Some("Bearer good-key".to_owned()), None]);

        provider.auth = AuthMode::ApiKey;
        let checks = check_provider(&service, http, &provider, Protocol::OpenaiChat).await;
        assert_eq!(checks, vec![KeyCheck { key_id: None, label: String::new(), count: None, error: Some("provider has no enabled API key".into()) }]);
        assert_eq!(requests.lock().unwrap().len(), 3);
    }
}
