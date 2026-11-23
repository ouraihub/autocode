//! Credential storage port for the model-service layer.
//!
//! The provider_store writes only `credential_ref` strings; the plaintext
//! `Secret` lives inside the host's system credential store (Keychain /
//! Credential Manager / libsecret). This trait is the single edge where the
//! service hands secrets across. Implementations:
//!
//! - `agent_chat::provider_services::GpuiCredentialBroker` forwards to the
//!   GPUI platform credential API through a bounded foreground task.
//! - `agent_chat::provider_services::InMemoryVault` is the test backend that
//!   actually stores + returns the bytes (GPUI's test platform `read_credentials`
//!   is a no-op and is not a valid acceptance test per design §4.2).
//!
//! See `docs/03-详细设计.md` §4.2 and §5.3.

use async_trait::async_trait;

use crate::Secret;

/// Errors that cross the credential boundary.
///
/// The variants are classified by the operational recovery they allow, not
/// by the raw platform error. Platform-specific messages are flattened into
/// `Backend` after being scrubbed of keys and request bodies.
#[derive(Debug, thiserror::Error)]
pub enum CredentialError {
    /// The broker is not accepting new requests (channel dropped, App shutting
    /// down, or the platform credential API was never wired). Callers MUST
    /// treat this as "configuration not saved", not as success.
    #[error("credential store unavailable")]
    Unavailable,

    /// The platform refused the operation: value exceeds the platform cap,
    /// the user denied access, or the ref is not retrievable. Retrying with
    /// the same input will keep failing; the service should surface the
    /// reason to the UI and leave the draft intact.
    #[error("credential store rejected the request: {0}")]
    Rejected(String),

    /// An unexpected platform or runtime error. Callers may retry once; the
    /// service MUST NOT assume success without a round-trip `get`.
    #[error("credential backend error: {0}")]
    Backend(#[source] anyhow::Error),
}

pub type CredentialResult<T> = std::result::Result<T, CredentialError>;

/// The credential port. Implementations must be `Send + Sync` so one instance
/// is shareable across every Tokio task that saves / loads a provider.
///
/// # Contract
///
/// - `put` MUST either store the value verbatim (so a follow-up `get` returns
///   the same bytes) or fail with `Rejected`/`Backend`. Silent truncation is
///   forbidden.
/// - `get` returns `None` for references that were never written or that have
///   been deleted. `None` is NOT an error.
/// - `delete` is idempotent: deleting a missing reference MUST succeed.
#[async_trait]
pub trait CredentialStore: Send + Sync {
    async fn put(&self, reference: &str, value: Secret) -> CredentialResult<()>;
    async fn get(&self, reference: &str) -> CredentialResult<Option<Secret>>;
    async fn delete(&self, reference: &str) -> CredentialResult<()>;
}
