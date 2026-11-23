//! Enum ⇌ `TEXT` conversions used in SQL binds, plus timestamp/uuid helpers.
//!
//! The TEXT representations are a storage contract: adding a new variant must
//! add its string here, never rely on `to_string` or `Serialize` which may
//! change independently.

use anyhow::{Result, anyhow};
use chrono::Utc;
use model_provider::{AuthMode, BackendTarget, CredentialOpState, Protocol, RoutingMode};
use uuid::Uuid;

pub(super) fn routing_to_str(r: RoutingMode) -> &'static str {
    match r {
        RoutingMode::Direct => "direct",
        RoutingMode::Gateway => "gateway",
    }
}

pub(super) fn routing_from_str(s: &str) -> Result<RoutingMode> {
    match s {
        "direct" => Ok(RoutingMode::Direct),
        "gateway" => Ok(RoutingMode::Gateway),
        other => Err(anyhow!("unknown routing_mode in db: {other:?}")),
    }
}

pub(super) fn auth_to_str(a: AuthMode) -> &'static str {
    match a {
        AuthMode::None => "none",
        AuthMode::ApiKey => "api_key",
    }
}

pub(super) fn auth_from_str(s: &str) -> Result<AuthMode> {
    match s {
        "none" => Ok(AuthMode::None),
        "api_key" => Ok(AuthMode::ApiKey),
        other => Err(anyhow!("unknown auth_mode in db: {other:?}")),
    }
}

pub(super) fn protocol_to_str(p: Protocol) -> &'static str {
    match p {
        Protocol::OpenaiChat => "openai-chat",
        Protocol::OpenaiResponses => "openai-responses",
        Protocol::AnthropicMessages => "anthropic-messages",
    }
}

pub(super) fn protocol_from_str(s: &str) -> Result<Protocol> {
    match s {
        "openai-chat" => Ok(Protocol::OpenaiChat),
        "openai-responses" => Ok(Protocol::OpenaiResponses),
        "anthropic-messages" => Ok(Protocol::AnthropicMessages),
        other => Err(anyhow!("unknown protocol in db: {other:?}")),
    }
}

pub(super) fn backend_to_str(b: BackendTarget) -> &'static str {
    match b {
        BackendTarget::Pi => "pi",
        BackendTarget::Omp => "omp",
        BackendTarget::Dsh => "dsh",
    }
}

pub(super) fn op_state_from_str(s: &str) -> Result<CredentialOpState> {
    match s {
        "pending_put" => Ok(CredentialOpState::PendingPut),
        "pending_delete" => Ok(CredentialOpState::PendingDelete),
        other => Err(anyhow!("unknown credential op state in db: {other:?}")),
    }
}

pub(super) fn now_ts() -> i64 {
    Utc::now().timestamp_millis()
}

pub(super) fn new_id() -> String {
    Uuid::new_v4().to_string()
}

pub(super) fn name_key(name: &str) -> String {
    name.trim().to_lowercase()
}

pub(super) fn header_name_key(name: &str) -> String {
    name.trim().to_lowercase()
}
