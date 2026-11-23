//! Credential broker that owns the GPUI platform credential API and the
//! bounded request channel the model-service layer uses to talk to it.
//!
//! Module layout per `docs/AI工程规范.md` §3.3:
//!
//! | File | Responsibility |
//! |---|---|
//! | `mod.rs` | Module facade |
//! | `broker.rs` | `GpuiCredentialBroker`: foreground pump + `CredentialStore` client |
//! | `services.rs` | `ProviderServices` App Global + composition root |
//! | `tests.rs` | Behavioural tests for the broker |
//!
//! Per design `docs/03-详细设计.md` §4.2 the GPUI platform's default
//! test `read_credentials` returns `Ok(None)` and is NOT an acceptance
//! substitute for real round-trips. Non-broker tests MUST use
//! `model_provider::InMemoryVault` (behind `test-support` feature).

#![allow(dead_code, unused_imports)]

mod broker;
pub(crate) mod catalog;
pub(crate) mod managed;
mod probe;

pub use probe::{KeyCheck, check_provider, discover_models, models_url};
mod services;

#[cfg(test)]
mod tests;
#[cfg(test)]
mod managed_tests;

pub use broker::{GpuiCredentialBroker, spawn_credential_broker};
pub use services::{ProviderServices, provider_services};
