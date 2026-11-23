//! Public DTOs.
//!
//! `ProviderInput` is a user-authored draft; it carries secrets only as
//! `Secret` values and is not meant for logs. `ProviderSnapshot` is the sanitised
//! form returned by repositories: keys and secret headers are reduced to
//! stable `_ref` strings plus a `masked` preview, never the plaintext.

use std::fmt;

use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};

/// Which engine a Provider is being projected for. This is distinct from the
/// UI's `AgentKind` on purpose: `model_provider` does not know about Claude /
/// Codex / OpenCode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendTarget {
    Pi,
    Omp,
    Dsh,
}

impl BackendTarget {
    pub const ALL: [BackendTarget; 3] = [Self::Pi, Self::Omp, Self::Dsh];

    /// A short, protocol-agnostic label — no engine version baked in.
    pub fn label(self) -> &'static str {
        match self {
            Self::Pi => "Pi",
            Self::Omp => "OMP",
            Self::Dsh => "DSH",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Protocol {
    OpenaiChat,
    OpenaiResponses,
    AnthropicMessages,
}

impl Protocol {
    /// Engine-side name used in `api:` fields.
    pub fn engine_api(self) -> &'static str {
        match self {
            Self::OpenaiChat => "openai-completions",
            Self::OpenaiResponses => "openai-responses",
            Self::AnthropicMessages => "anthropic-messages",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthMode {
    None,
    #[default]
    ApiKey,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoutingMode {
    /// Engines call the upstream directly using the injected key env.
    #[default]
    Direct,
    /// Requests go through the loopback gateway; key rotation is per request.
    Gateway,
}

/// Model identity as seen by the chat metadata. Old chats may still carry a
/// bare id string — those are read back as `Native`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "source", rename_all = "snake_case")]
pub enum ModelSelection {
    /// The engine's own model id, meaningful only to that engine.
    Native { id: String },
    /// A autocode-managed model: `provider_id` is the full Provider UUID, never a
    /// display name.
    Managed { provider_id: String, model_id: String },
}

impl ModelSelection {
    pub fn is_managed(&self) -> bool {
        matches!(self, Self::Managed { .. })
    }
}

/// A secret value (API key, header value). Never serialised through serde, never
/// printed through `Debug`; callers wanting the bytes must go through `expose`.
#[derive(Clone, Zeroize, ZeroizeOnDrop)]
pub struct Secret(String);

impl Secret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn expose(&self) -> &str {
        &self.0
    }

    pub fn into_string(mut self) -> String {
        std::mem::take(&mut self.0)
    }
}

impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Secret").finish_non_exhaustive()
    }
}

impl PartialEq for Secret {
    fn eq(&self, other: &Self) -> bool {
        // Not constant-time — callers should not use `Secret` for sensitive
        // comparisons; this only supports structural equality in tests.
        self.0 == other.0
    }
}

/// One endpoint line of a Provider: a (protocol, base URL) pair plus optional
/// per-line headers. `base_url` is user input and must be run through
/// `validate::parse_endpoint` before persistence.
#[derive(Debug, Clone, PartialEq)]
pub struct EndpointInput {
    pub protocol: Protocol,
    pub base_url: String,
    pub headers: Vec<HeaderInput>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct HeaderInput {
    pub name: String,
    /// Plaintext header value. Marked sensitive when `secret = true`; the
    /// repository layer moves such values to the credential store before
    /// persisting the metadata.
    pub value: String,
    pub secret: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct KeyInput {
    pub label: Option<String>,
    pub secret: Secret,
    pub enabled: bool,
}
/// Reasoning levels in ascending order, matching the model-service editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ThinkingLevel {
    Off,
    Minimal,
    Low,
    Medium,
    High,
    Xhigh,
    Max,
}

impl ThinkingLevel {
    /// Every reasoning level in ascending order.
    pub const ALL: [Self; 7] = [Self::Off, Self::Minimal, Self::Low, Self::Medium, Self::High, Self::Xhigh, Self::Max];

    /// The spelling used in model metadata and engine configuration.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Minimal => "minimal",
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
            Self::Xhigh => "xhigh",
            Self::Max => "max",
        }
    }
}

/// Highest declared effort not above the saved preference; absent if no level qualifies.
pub fn clamp_effort(saved: Option<ThinkingLevel>, efforts: &[ThinkingLevel]) -> Option<ThinkingLevel> {
    let saved = saved?;
    efforts.iter().copied().filter(|effort| *effort <= saved).max()
}

/// What a model does. Only chat models can be launched in an agent harness.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ModelType {
    #[default]
    Chat,
    Image,
    Video,
    Tts,
    Stt,
    Embedding,
    Rerank,
    Unknown,
}

impl ModelType {
    /// Every model type in editor order.
    pub const ALL: [Self; 8] = [Self::Chat, Self::Image, Self::Video, Self::Tts, Self::Stt, Self::Embedding, Self::Rerank, Self::Unknown];
}

/// User-visible model metadata attached to a Provider. Capability flags are
/// hand-maintained unless a successful `/models` sync populated them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderModel {
    pub model_id: String,
    /// Defaults to chat when loading models saved before type metadata existed.
    #[serde(default)]
    pub model_type: ModelType,
    pub name: String,
    /// One or more protocols this model is reachable over; must be a non-empty
    /// subset of the owning Provider's endpoint protocols.
    pub protocols: Vec<Protocol>,
    #[serde(default)]
    pub context_window: Option<u64>,
    #[serde(default)]
    pub max_output_tokens: Option<u64>,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
    /// Declared reasoning levels; must be empty when reasoning is disabled.
    #[serde(default)]
    pub efforts: Vec<ThinkingLevel>,
    #[serde(default)]
    pub enabled: bool,
}

/// User-authored Provider draft. Validation rules live in `validate.rs`.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderInput {
    pub name: String,
    pub auth: AuthMode,
    pub routing: RoutingMode,
    pub endpoints: Vec<EndpointInput>,
    pub default_protocol: Protocol,
    pub keys: Vec<KeyInput>,
    pub models: Vec<ProviderModel>,
    pub enabled: bool,
}

/// The single model-service source chosen for a coding partner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "provider_id")]
pub enum EngineProviderChoice {
    Gateway,
    Official,
    Provider(String),
}

/// Agent-level preferences: which Provider/model to use by default, and which
/// configuration source (`Native` vs `Managed`) this agent uses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentPreferences {
    pub target: BackendTarget,
    #[serde(default)]
    pub source: ConfigSource,
    #[serde(default)]
    pub default_model: Option<ModelSelection>,
    /// Coding-partner route choice; None preserves each enabled provider's access mode.
    #[serde(default)]
    pub provider_choice: Option<EngineProviderChoice>,
    /// Path to the user-chosen executable; `None` means fall back to PATH.
    #[serde(default)]
    pub launch_executable: Option<std::path::PathBuf>,
    /// Monotonic revision for optimistic-concurrency writes.
    #[serde(default)]
    pub revision: u64,
}

impl AgentPreferences {
    pub fn native_default(target: BackendTarget) -> Self {
        Self {
            target,
            source: ConfigSource::Native,
            default_model: None,
            provider_choice: None,
            launch_executable: None,
            revision: 0,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    #[default]
    Native,
    Managed,
}

/// The outcome of a single probe call (list_models or check_model). Never
/// contains the raw upstream body or a `Secret`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    pub protocol: Protocol,
    pub key_label: Option<String>,
    pub elapsed_ms: u64,
    pub http_status: Option<u16>,
    pub outcome: ProbeOutcome,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ProbeOutcome {
    /// A successful `/models` call, with the number of models the upstream
    /// listed (not their contents).
    ModelsListed { count: usize },
    /// A successful minimal request against a specific model.
    ModelReachable { model_id: String },
    /// Authenticated endpoint rejected the credentials.
    AuthRejected,
    /// Endpoint returned a non-success HTTP status.
    HttpError,
    /// The upstream rate-limited the request.
    RateLimited,
    /// TLS, DNS or TCP-level failure.
    Unreachable,
    /// Request/response timed out inside the probe's bounded deadline.
    Timeout,
    /// Response body was not JSON of the expected shape.
    Protocol,
}

// -----------------------------------------------------------------------------
// Snapshots: sanitised views returned by the repository.
//
// `*Snapshot`/`*Summary` must never carry plaintext secrets. `credential_ref`
// is an opaque identifier, not a key, and is safe to display and persist.
// -----------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderSnapshot {
    pub id: String,
    /// Built-in template identity, retained across user edits; None for custom services.
    #[serde(default)]
    pub preset_id: Option<String>,
    pub revision: u64,
    pub name: String,
    pub enabled: bool,
    pub routing: RoutingMode,
    pub auth: AuthMode,
    pub default_protocol: Protocol,
    pub endpoints: Vec<EndpointSnapshot>,
    pub models: Vec<ProviderModel>,
    pub keys: Vec<KeySummary>,
    pub headers: Vec<HeaderSummary>,
    pub sort: i64,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EndpointSnapshot {
    pub protocol: Protocol,
    pub base_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeySummary {
    pub id: String,
    pub label: Option<String>,
    pub masked: String,
    pub credential_ref: String,
    pub enabled: bool,
    pub sort: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HeaderSummary {
    pub id: String,
    pub name: String,
    pub masked: String,
    pub credential_ref: String,
    pub enabled: bool,
}

/// Identifies which secret slot of a Provider is being changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SecretSlot {
    Key { key_id: String },
    Header { header_id: String },
}

/// Metadata written to SQL alongside a committed credential. The plaintext
/// value stays on the credential-store side; this struct never carries it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SecretMetadataChange {
    pub slot: SecretSlot,
    pub new_reference: String,
    pub masked: String,
    /// For `Key` slots: UI label; for `Header` slots: header name.
    pub display: Option<String>,
    pub enabled: bool,
}

/// One row of `model_credential_ops`; used by the startup reconciler.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CredentialOperation {
    pub credential_ref: String,
    pub state: CredentialOpState,
    pub created_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialOpState {
    PendingPut,
    PendingDelete,
}

/// Repository-level DTO for inserting or replacing a key metadata row. The
/// `credential_ref` MUST already have been written to the `CredentialStore`.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyRecord {
    pub id: Option<String>,
    pub label: Option<String>,
    pub masked: String,
    pub credential_ref: String,
    pub enabled: bool,
    pub sort: i64,
}

/// Repository-level DTO for inserting or replacing a header metadata row.
#[derive(Debug, Clone, PartialEq)]
pub struct HeaderRecord {
    pub id: Option<String>,
    pub name: String,
    pub masked: String,
    pub credential_ref: String,
    pub enabled: bool,
}

/// A validated provider draft, ready to be written to the repository.
/// Carries no secrets; keys and headers are managed through separate
/// `commit_*_secret` / `remove_*` calls.
#[derive(Debug, Clone, PartialEq)]
pub struct ProviderDraft {
    pub name: String,
    pub enabled: bool,
    pub routing: RoutingMode,
    pub auth: AuthMode,
    pub default_protocol: Protocol,
    pub endpoints: Vec<EndpointSnapshot>,
    pub models: Vec<ProviderModel>,
    pub sort: i64,
}
