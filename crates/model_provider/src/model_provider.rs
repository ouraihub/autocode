//! Domain types for autocode's model-service layer.
//!
//! The types in this crate are the stable contract between the settings UI, the
//! persistent store, the router that prepares an engine launch, and the three
//! new agent harnesses. The crate is intentionally UI- and I/O-free so it stays
//! portable between the GPUI App and background Tokio tasks.
//!
//! See `docs/03-详细设计.md` §3 for the field-level rules this module enforces.

mod credentials;
mod model_catalog;
mod presets;
mod service;
mod types;
mod validate;

#[cfg(any(test, feature = "test-support"))]
mod vault;

pub use credentials::{CredentialError, CredentialResult, CredentialStore};
pub use model_catalog::{ModelHints, model_group, model_hints, model_type};
pub use presets::{PROVIDER_PRESETS, ProviderPreset, provider_preset};
pub use service::{
    CREDENTIAL_REF_PREFIX, ProviderChange, ProviderChangeKind, ProviderChangeSender,
    ProviderRepository, ProviderService, ReconcileReport, ServiceError, ServiceResult,
};
pub use types::*;
pub use validate::{
    ValidationError, draft as validate_draft, header_name as validate_header_name,
    header_value as validate_header_value, normalized_base_url, provider as validate_provider,
};

#[cfg(any(test, feature = "test-support"))]
pub use vault::InMemoryVault;

#[cfg(test)]
mod metadata_tests;

/// Errors that cross the model_provider boundary. Repository and network
/// implementations contribute their own variants later; keeping the `Invalid`
/// case is sufficient for the type-only task.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid input: {0}")]
    Invalid(#[from] ValidationError),
}

pub type Result<T, E = Error> = std::result::Result<T, E>;
