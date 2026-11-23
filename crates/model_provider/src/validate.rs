//! Input validation rules used before anything is written to the repository
//! or sent over the wire.
//!
//! The rules match `docs/03-详细设计.md` §3.4. Tests at the bottom of this file
//! exercise each rule with one representative success and failure case.

use std::collections::BTreeSet;

use url::Url;

use crate::types::{AuthMode, EndpointInput, HeaderInput, Protocol, ProviderDraft, ProviderInput, ProviderModel};

const MAX_NAME_LEN: usize = 64;
const MAX_HEADER_VALUE_LEN: usize = 2048;

/// Header names the user is not allowed to set — autocode owns these, and letting a
/// user override them would either break routing or leak credentials.
const FORBIDDEN_HEADERS: &[&str] = &[
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "authorization",
    "x-api-key",
    "upgrade",
];
const FORBIDDEN_HEADER_PREFIX: &str = "proxy-";

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ValidationError {
    #[error("provider name must be between 1 and {MAX_NAME_LEN} characters after trimming")]
    NameLength,
    #[error("at least one endpoint is required")]
    NoEndpoints,
    #[error("default protocol {0:?} is not configured on any endpoint")]
    DefaultProtocolMissing(Protocol),
    #[error("protocol {0:?} appears on more than one endpoint")]
    DuplicateEndpoint(Protocol),
    #[error("base URL must be http:// or https://")]
    BaseUrlScheme,
    #[error("base URL is not a valid absolute URL")]
    BaseUrlShape,
    #[error("base URL must not contain a query string or fragment")]
    BaseUrlQueryFragment,
    #[error("header name {0:?} is reserved by autocode")]
    HeaderReserved(String),
    #[error("header name must be a non-empty HTTP token")]
    HeaderNameShape,
    #[error("header name {0:?} is declared more than once on the same line")]
    HeaderDuplicate(String),
    #[error("header value must not contain CR, LF or NUL characters")]
    HeaderValueControl,
    #[error("header value must be at most {MAX_HEADER_VALUE_LEN} bytes")]
    HeaderValueLength,
    #[error("ApiKey auth requires at least one enabled key")]
    ApiKeyMissing,
    #[error("model {0:?} references a protocol that is not configured")]
    ModelProtocolMissing(String),
    #[error("model {0:?} has no protocols")]
    ModelProtocolsEmpty(String),
    #[error("model id must be non-empty")]
    ModelIdEmpty,
    #[error("model id {0:?} is declared more than once")]
    ModelIdDuplicate(String),
    #[error("model {0:?} declares efforts without reasoning support")]
    ModelEffortsWithoutReasoning(String),
    #[error("model {0:?} declares a reasoning level more than once")]
    ModelEffortDuplicate(String),
}

/// Returns `Ok` only if `input` is safe to persist as a Provider record.
pub fn provider(input: &ProviderInput) -> Result<(), ValidationError> {
    name(&input.name)?;
    endpoints(&input.endpoints, input.default_protocol)?;
    if input.auth == AuthMode::ApiKey && !input.keys.iter().any(|k| k.enabled) {
        return Err(ValidationError::ApiKeyMissing);
    }
    let configured: BTreeSet<Protocol> =
        input.endpoints.iter().map(|e| e.protocol).collect();
    models(&input.models, &configured)?;
    Ok(())
}

/// Validates metadata before repository writes. Keys are committed separately.
pub fn draft(input: &ProviderDraft) -> Result<(), ValidationError> {
    name(&input.name)?;
    endpoint_fields(
        input.endpoints.iter().map(|endpoint| (endpoint.protocol, endpoint.base_url.as_str())),
        input.default_protocol,
    )?;
    let configured = input.endpoints.iter().map(|endpoint| endpoint.protocol).collect();
    models(&input.models, &configured)
}


fn name(name: &str) -> Result<(), ValidationError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_NAME_LEN {
        return Err(ValidationError::NameLength);
    }
    Ok(())
}

fn endpoints(
    endpoints: &[EndpointInput],
    default: Protocol,
) -> Result<(), ValidationError> {
    endpoint_fields(
        endpoints.iter().map(|endpoint| (endpoint.protocol, endpoint.base_url.as_str())),
        default,
    )?;
    for endpoint in endpoints {
        headers(&endpoint.headers)?;
    }
    Ok(())
}

fn endpoint_fields<'a>(
    endpoints: impl Iterator<Item = (Protocol, &'a str)>,
    default: Protocol,
) -> Result<(), ValidationError> {
    let mut seen = BTreeSet::new();
    for (protocol, url) in endpoints {
        if !seen.insert(protocol) {
            return Err(ValidationError::DuplicateEndpoint(protocol));
        }
        base_url(url)?;
    }
    if seen.is_empty() {
        return Err(ValidationError::NoEndpoints);
    }
    if !seen.contains(&default) {
        return Err(ValidationError::DefaultProtocolMissing(default));
    }
    Ok(())
}

/// Validates and canonicalizes a provider endpoint for routing and persistence.
pub fn normalized_base_url(raw: &str) -> Result<String, ValidationError> {
    let mut parsed = base_url(raw)?;
    while parsed.path().ends_with('/') && parsed.path() != "/" {
        let trimmed = parsed.path().trim_end_matches('/').to_owned();
        parsed.set_path(&trimmed);
    }
    if parsed.path() == "/" { parsed.set_path(""); }
    Ok(parsed.to_string())
}

/// Parses `raw` as an absolute http(s) URL. The caller keeps the trimmed string
/// form for storage; this function is validation only.
pub fn base_url(raw: &str) -> Result<Url, ValidationError> {
    let parsed = Url::parse(raw.trim()).map_err(|_| ValidationError::BaseUrlShape)?;
    match parsed.scheme() {
        "http" | "https" => {}
        _ => return Err(ValidationError::BaseUrlScheme),
    }
    if !parsed.has_host() { return Err(ValidationError::BaseUrlShape); }
    if parsed.query().is_some() || parsed.fragment().is_some() { return Err(ValidationError::BaseUrlQueryFragment); }
    Ok(parsed)
}

fn headers(headers: &[HeaderInput]) -> Result<(), ValidationError> {
    let mut seen = BTreeSet::new();
    for header in headers {
        header_name(&header.name)?;
        let lower = header.name.to_ascii_lowercase();
        if !seen.insert(lower.clone()) {
            return Err(ValidationError::HeaderDuplicate(header.name.clone()));
        }
        header_value(&header.value)?;
    }
    Ok(())
}

/// Validates an HTTP header name and rejects headers reserved by autocode.
pub fn header_name(name: &str) -> Result<(), ValidationError> {
    if name.is_empty() {
        return Err(ValidationError::HeaderNameShape);
    }
    // RFC 7230 token: no whitespace, no CTLs, no separators. Checking the small
    // set below is enough for user-authored headers; the HTTP client rejects
    // anything we miss here.
    let bad = |b: char| {
        b.is_whitespace()
            || b.is_control()
            || matches!(
                b,
                '(' | ')'
                    | ','
                    | '/'
                    | ':'
                    | ';'
                    | '<'
                    | '='
                    | '>'
                    | '?'
                    | '@'
                    | '['
                    | '\\'
                    | ']'
                    | '{'
                    | '}'
                    | '"'
            )
    };
    if name.chars().any(bad) {
        return Err(ValidationError::HeaderNameShape);
    }
    let lower = name.to_ascii_lowercase();
    if FORBIDDEN_HEADERS.contains(&lower.as_str()) || lower.starts_with(FORBIDDEN_HEADER_PREFIX) {
        return Err(ValidationError::HeaderReserved(name.to_owned()));
    }
    Ok(())
}

/// Validates a header value before it is persisted or sent upstream.
pub fn header_value(value: &str) -> Result<(), ValidationError> {
    if value.len() > MAX_HEADER_VALUE_LEN {
        return Err(ValidationError::HeaderValueLength);
    }
    if value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
        return Err(ValidationError::HeaderValueControl);
    }
    Ok(())
}

fn models(
    models: &[ProviderModel],
    configured: &BTreeSet<Protocol>,
) -> Result<(), ValidationError> {
    let mut ids = BTreeSet::new();
    for model in models {
        if model.model_id.trim().is_empty() {
            return Err(ValidationError::ModelIdEmpty);
        }
        if !ids.insert(model.model_id.clone()) {
            return Err(ValidationError::ModelIdDuplicate(model.model_id.clone()));
        }
        if model.protocols.is_empty() {
            return Err(ValidationError::ModelProtocolsEmpty(model.model_id.clone()));
        }
        if !model.supports_reasoning && !model.efforts.is_empty() {
            return Err(ValidationError::ModelEffortsWithoutReasoning(model.model_id.clone()));
        }
        if model.efforts.iter().enumerate().any(|(index, effort)| model.efforts[..index].contains(effort)) {
            return Err(ValidationError::ModelEffortDuplicate(model.model_id.clone()));
        }
        for protocol in &model.protocols {
            if !configured.contains(protocol) {
                return Err(ValidationError::ModelProtocolMissing(model.model_id.clone()));
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{KeyInput, ProviderModel, RoutingMode, Secret};

    fn endpoint(protocol: Protocol, url: &str) -> EndpointInput {
        EndpointInput {
            protocol,
            base_url: url.into(),
            headers: vec![],
        }
    }

    fn basic_input() -> ProviderInput {
        ProviderInput {
            name: "七牛云".into(),
            auth: AuthMode::ApiKey,
            routing: RoutingMode::Direct,
            endpoints: vec![endpoint(Protocol::OpenaiChat, "https://api.example.com/v1")],
            default_protocol: Protocol::OpenaiChat,
            keys: vec![KeyInput {
                label: Some("main".into()),
                secret: Secret::new("sk-test"),
                enabled: true,
            }],
            models: vec![ProviderModel {
                model_id: "m-a".into(),
                model_type: crate::ModelType::Chat,
                name: "Model A".into(),
                protocols: vec![Protocol::OpenaiChat],
                context_window: Some(128_000),
                max_output_tokens: Some(8_192),
                supports_vision: false,
                supports_tools: true,
                supports_reasoning: true,
                efforts: Vec::new(),
                enabled: true,
            }],
            enabled: true,
        }
    }



    #[test]
    fn basic_input_passes_validation() {
        provider(&basic_input()).unwrap();
    }

    #[test]
    fn empty_name_is_rejected() {
        let mut input = basic_input();
        input.name = "   ".into();
        assert_eq!(provider(&input), Err(ValidationError::NameLength));
    }

    #[test]
    fn name_above_sixty_four_chars_is_rejected() {
        let mut input = basic_input();
        input.name = "a".repeat(65);
        assert_eq!(provider(&input), Err(ValidationError::NameLength));
    }

    #[test]
    fn default_protocol_must_be_configured() {
        let mut input = basic_input();
        input.default_protocol = Protocol::AnthropicMessages;
        assert_eq!(
            provider(&input),
            Err(ValidationError::DefaultProtocolMissing(Protocol::AnthropicMessages))
        );
    }

    #[test]
    fn duplicate_endpoint_protocol_is_rejected() {
        let mut input = basic_input();
        input
            .endpoints
            .push(endpoint(Protocol::OpenaiChat, "https://other.example.com"));
        assert_eq!(
            provider(&input),
            Err(ValidationError::DuplicateEndpoint(Protocol::OpenaiChat))
        );
    }

    #[test]
    fn non_http_base_url_is_rejected() {
        let mut input = basic_input();
        input.endpoints[0].base_url = "file:///tmp/api".into();
        assert_eq!(provider(&input), Err(ValidationError::BaseUrlScheme));
    }

    #[test]
    fn base_url_with_query_is_rejected() {
        let mut input = basic_input();
        input.endpoints[0].base_url = "https://api.example.com/v1?debug=1".into();
        assert_eq!(provider(&input), Err(ValidationError::BaseUrlQueryFragment));
    }

    #[test]
    fn missing_host_is_rejected() {
        assert_eq!(base_url("http://"), Err(ValidationError::BaseUrlShape));
    }

    #[test]
    fn reserved_header_is_rejected() {
        let mut input = basic_input();
        input.endpoints[0].headers.push(HeaderInput {
            name: "Authorization".into(),
            value: "Bearer leak".into(),
            secret: true,
        });
        assert_eq!(
            provider(&input),
            Err(ValidationError::HeaderReserved("Authorization".into()))
        );
    }

    #[test]
    fn proxy_header_prefix_is_rejected() {
        let mut input = basic_input();
        input.endpoints[0].headers.push(HeaderInput {
            name: "Proxy-Authorization".into(),
            value: "Bearer leak".into(),
            secret: true,
        });
        assert_eq!(
            provider(&input),
            Err(ValidationError::HeaderReserved("Proxy-Authorization".into()))
        );
    }

    #[test]
    fn header_name_control_is_rejected() {
        let mut input = basic_input();
        input.endpoints[0].headers.push(HeaderInput {
            name: "Bad Name".into(),
            value: "value".into(),
            secret: false,
        });
        assert_eq!(provider(&input), Err(ValidationError::HeaderNameShape));
    }

    #[test]
    fn duplicate_header_name_case_insensitive_is_rejected() {
        let mut input = basic_input();
        input.endpoints[0].headers.push(HeaderInput {
            name: "X-Trace".into(),
            value: "a".into(),
            secret: false,
        });
        input.endpoints[0].headers.push(HeaderInput {
            name: "x-trace".into(),
            value: "b".into(),
            secret: false,
        });
        assert_eq!(
            provider(&input),
            Err(ValidationError::HeaderDuplicate("x-trace".into()))
        );
    }

    #[test]
    fn header_value_crlf_is_rejected() {
        let mut input = basic_input();
        input.endpoints[0].headers.push(HeaderInput {
            name: "X-Leak".into(),
            value: "line1\r\ninjected: true".into(),
            secret: false,
        });
        assert_eq!(provider(&input), Err(ValidationError::HeaderValueControl));
    }

    #[test]
    fn header_value_too_long_is_rejected() {
        let mut input = basic_input();
        input.endpoints[0].headers.push(HeaderInput {
            name: "X-Big".into(),
            value: "x".repeat(MAX_HEADER_VALUE_LEN + 1),
            secret: false,
        });
        assert_eq!(provider(&input), Err(ValidationError::HeaderValueLength));
    }

    #[test]
    fn api_key_mode_requires_enabled_key() {
        let mut input = basic_input();
        input.keys[0].enabled = false;
        assert_eq!(provider(&input), Err(ValidationError::ApiKeyMissing));
    }

    #[test]
    fn auth_none_may_have_no_keys() {
        let mut input = basic_input();
        input.auth = AuthMode::None;
        input.keys.clear();
        provider(&input).unwrap();
    }

    #[test]
    fn model_protocol_must_be_configured() {
        let mut input = basic_input();
        input.models[0].protocols = vec![Protocol::AnthropicMessages];
        assert_eq!(
            provider(&input),
            Err(ValidationError::ModelProtocolMissing("m-a".into()))
        );
    }

    #[test]
    fn model_with_no_protocols_is_rejected() {
        let mut input = basic_input();
        input.models[0].protocols.clear();
        assert_eq!(
            provider(&input),
            Err(ValidationError::ModelProtocolsEmpty("m-a".into()))
        );
    }

    #[test]
    fn duplicate_model_id_is_rejected() {
        let mut input = basic_input();
        let extra = input.models[0].clone();
        input.models.push(extra);
        assert_eq!(
            provider(&input),
            Err(ValidationError::ModelIdDuplicate("m-a".into()))
        );
    }

    #[test]
    fn removing_a_models_only_endpoint_is_rejected() {
        let mut input = basic_input();
        input.endpoints = vec![endpoint(Protocol::AnthropicMessages, "https://api.example.com")];
        input.default_protocol = Protocol::AnthropicMessages;
        assert_eq!(provider(&input), Err(ValidationError::ModelProtocolMissing("m-a".into())));
    }

    #[test]
    fn removing_an_endpoint_with_model_migration_is_allowed() {
        let mut input = basic_input();
        input.endpoints = vec![endpoint(Protocol::AnthropicMessages, "https://api.example.com")];
        input.default_protocol = Protocol::AnthropicMessages;
        input.models[0].protocols = vec![Protocol::AnthropicMessages];
        assert_eq!(provider(&input), Ok(()));
    }


    #[test]
    fn reasoning_efforts_require_reasoning_and_unique_levels() {
        let mut input = basic_input();
        input.models[0].efforts = vec![crate::ThinkingLevel::Low, crate::ThinkingLevel::High];
        assert_eq!(provider(&input), Ok(()));
        input.models[0].supports_reasoning = false;
        assert_eq!(provider(&input), Err(ValidationError::ModelEffortsWithoutReasoning("m-a".into())));
        input.models[0].supports_reasoning = true;
        input.models[0].efforts.push(crate::ThinkingLevel::Low);
        assert_eq!(provider(&input), Err(ValidationError::ModelEffortDuplicate("m-a".into())));
    }

    #[test]
    fn secret_is_not_leaked_by_debug() {
        let secret = Secret::new("sk-supersecret");
        let formatted = format!("{:?}", secret);
        assert!(!formatted.contains("sk-supersecret"), "{}", formatted);
    }
    #[test]
    fn normalized_base_url_removes_trailing_path_slashes() {
        assert_eq!(normalized_base_url("https://api.example.com/v1///").unwrap(), "https://api.example.com/v1");
        assert_eq!(normalized_base_url("https://api.example.com/").unwrap(), "https://api.example.com/");
    }
}
