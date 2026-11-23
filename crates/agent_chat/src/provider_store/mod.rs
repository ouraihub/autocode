//! SQLite backing store for model providers.
//!
//! Scope (P1-02): table layout, Domain registration, metadata CRUD, and the
//! revision transaction. The store never writes or reads credential plaintext;
//! callers that manage secrets (P1-03 `CredentialStore` broker, P1-04 service)
//! are responsible for the actual `put`/`get`/`delete` round-trips and hand
//! this layer only the resulting opaque `credential_ref` strings.
//!
//! Module layout per `docs/AI工程规范.md` §3.3 (one responsibility per file):
//!
//! | File | Responsibility |
//! |---|---|
//! | `mod.rs` | `ProviderDb`, `Domain` + migrations, draft structs, error type |
//! | `codec.rs` | Enum ⇌ `TEXT` column conversions, timestamp and uuid helpers |
//! | `snapshot.rs` | `fetch_snapshot` / `require_snapshot` (read-back into `ProviderSnapshot`) |
//! | `providers.rs` | Provider CRUD: insert, list, get, update, delete |
//! | `keys.rs` | Key metadata: `commit_key`, `remove_key`, `set_key_enabled` |
//! | `headers.rs` | Header metadata: `commit_header`, `remove_header` |
//! | `credential_ops.rs` | Pending-put / pending-delete log and revision bump helper |
//! | `preferences.rs` | `AgentPreferences` load / save |
//! | `tests.rs` | Behavioural tests (§3.3 fixture exemption) |
//!
//! See `docs/03-详细设计.md` §5 for the schema and transaction contract.

// The service that invokes these methods lives in P1-04; until then every
// method here is unreferenced on purpose. The tests at the bottom of this
// module are the executable contract.
#![allow(dead_code)]

mod codec;
mod credential_ops;
mod headers;
mod keys;
mod preferences;
mod providers;
mod repo_impl;
mod snapshot;

#[cfg(test)]
mod tests;

use db::{
    AppDatabase,
    sqlez::{domain::Domain, thread_safe_connection::ThreadSafeConnection},
    sqlez_macros::sql,
};


// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Errors crossing the store boundary.
///
/// `NotFound`, `Conflict`, and `Forbidden` stay distinct so callers can
/// distinguish missing records, stale revisions, and protected presets.
/// Other storage failures cross this boundary as `Io`.
#[derive(Debug, thiserror::Error)]
pub enum ProviderStoreError {
    #[error("provider {0} not found")]
    NotFound(String),
    #[error("provider {0} revision conflict: on-disk revision is not {1}")]
    Conflict(String, u64),
    #[error("preset provider {0} cannot be deleted")]
    Forbidden(String),
    #[error("storage error: {0}")]
    Io(#[from] anyhow::Error),
}

pub type StoreResult<T> = std::result::Result<T, ProviderStoreError>;

// ---------------------------------------------------------------------------
// Draft types used by the CRUD surface. These mirror the SQL rows; they are
// NOT the user-facing `ProviderInput`, which carries secrets. Callers strip
// secrets before handing drafts to the store.
// ---------------------------------------------------------------------------

// `ProviderDraft`, `KeyRecord`, `HeaderRecord` are re-exported from
// `model_provider` so the service layer and the store share one DTO.
pub use model_provider::{HeaderRecord, KeyRecord, ProviderDraft};

// ---------------------------------------------------------------------------
// Domain
// ---------------------------------------------------------------------------

pub struct ProviderDb(ThreadSafeConnection);

impl ProviderDb {
    pub fn from_app_db(db: &AppDatabase) -> Self {
        Self(db.0.clone())
    }
}

impl Domain for ProviderDb {
    const NAME: &str = stringify!(ProviderDb);

    const MIGRATIONS: &[&str] = &[sql!(
        CREATE TABLE model_providers (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            name_key TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
            routing_mode TEXT NOT NULL,
            auth_mode TEXT NOT NULL,
            default_protocol TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK(revision > 0),
            sort INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            updated_at INTEGER NOT NULL
        ) STRICT;

        CREATE TABLE model_provider_endpoints (
            provider_id TEXT NOT NULL REFERENCES model_providers(id) ON DELETE CASCADE,
            protocol TEXT NOT NULL,
            base_url TEXT NOT NULL,
            PRIMARY KEY(provider_id, protocol)
        ) STRICT;

        CREATE TABLE model_provider_models (
            provider_id TEXT NOT NULL REFERENCES model_providers(id) ON DELETE CASCADE,
            model_id TEXT NOT NULL,
            definition TEXT NOT NULL,
            sort INTEGER NOT NULL,
            PRIMARY KEY(provider_id, model_id)
        ) STRICT;

        CREATE TABLE model_provider_keys (
            id TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL REFERENCES model_providers(id) ON DELETE CASCADE,
            label TEXT,
            masked TEXT NOT NULL,
            credential_ref TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
            sort INTEGER NOT NULL
        ) STRICT;

        CREATE TABLE model_provider_headers (
            id TEXT PRIMARY KEY,
            provider_id TEXT NOT NULL REFERENCES model_providers(id) ON DELETE CASCADE,
            name TEXT NOT NULL,
            name_key TEXT NOT NULL,
            masked TEXT NOT NULL,
            credential_ref TEXT NOT NULL UNIQUE,
            enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
            UNIQUE(provider_id, name_key)
        ) STRICT;

        CREATE TABLE agent_launch_preferences (
            backend TEXT PRIMARY KEY,
            definition TEXT NOT NULL,
            revision INTEGER NOT NULL CHECK(revision > 0)
        ) STRICT;

        CREATE TABLE model_credential_ops (
            credential_ref TEXT PRIMARY KEY,
            state TEXT NOT NULL,
            created_at INTEGER NOT NULL
        ) STRICT;
    ), sql!(
        ALTER TABLE model_providers ADD COLUMN preset_id TEXT;
        CREATE UNIQUE INDEX model_providers_preset_id ON model_providers(preset_id);
    )];
}

db::static_connection!(ProviderDb, [workspace::WorkspaceDb]);
