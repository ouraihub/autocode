//! `ProviderService` glues a `CredentialMetadataRepo` (SQL metadata +
//! revision bookkeeping) and a `CredentialStore` (system keychain) into the
//! 6-step commit flow described in `docs/03-详细设计.md` §5.3.
//!
//! Scope (P1-04):
//!
//! - commit / remove flows for Key and Header secrets
//! - startup reconciler that drains `pending_put` / `pending_delete`
//!
//! Out of scope (P1-05): `AgentPreferences`, ProviderChange broadcast,
//! `ProviderService::create / update / delete_provider` composition.

mod change;
mod maintenance;
mod reconcile;

pub use change::{ProviderChange, ProviderChangeKind, ProviderChangeSender};

use async_trait::async_trait;
use uuid::Uuid;

use crate::{
    AgentPreferences, BackendTarget, CredentialError, CredentialOperation, CredentialStore,
    HeaderRecord, KeyRecord, ProviderDraft, ProviderSnapshot, Secret,
};

/// Opaque stable prefix for every credential reference the service mints.
/// Having a prefix lets the credential store (or a human operator) filter
/// out everything that is NOT ours before considering a reference for
/// deletion.
pub const CREDENTIAL_REF_PREFIX: &str = "autocode-model-provider://credential/";

// ---------------------------------------------------------------------------
// Repository port
// ---------------------------------------------------------------------------

/// The repository port. `ProviderService` uses this to reach all persisted
/// provider state; the concrete SQL implementation lives in
/// `agent_chat::provider_store`. Keeping the trait here means
/// `model_provider` stays UI- and sqlez-free.
///
/// Errors cross as `anyhow::Error`; the service translates known
/// `ProviderStoreError` cases downstream via its own `ServiceError`.
#[async_trait]
pub trait ProviderRepository: Send + Sync {
    // ---- Provider CRUD ----
    async fn list_providers(&self) -> anyhow::Result<Vec<ProviderSnapshot>>;
    /// Inserts missing templates without changing existing providers; returns inserted rows only.
    async fn seed_presets(&self) -> anyhow::Result<Vec<ProviderSnapshot>>;
    async fn get_provider(&self, id: &str) -> anyhow::Result<Option<ProviderSnapshot>>;
    async fn insert_provider(
        &self,
        draft: ProviderDraft,
    ) -> anyhow::Result<ProviderSnapshot>;
    async fn update_provider(
        &self,
        id: &str,
        expect: u64,
        draft: ProviderDraft,
    ) -> anyhow::Result<ProviderSnapshot>;
    /// Deletes a provider; returns the credential refs enqueued as
    /// `pending_delete` so the service can purge the credential store.
    async fn delete_provider(&self, id: &str, expect: u64) -> anyhow::Result<Vec<String>>;

    // ---- Credential ops ----
    async fn begin_credential_put(&self, reference: &str) -> anyhow::Result<()>;
    async fn commit_key(
        &self,
        provider_id: &str,
        expect: u64,
        record: KeyRecord,
    ) -> anyhow::Result<ProviderSnapshot>;
    async fn commit_header(
        &self,
        provider_id: &str,
        expect: u64,
        record: HeaderRecord,
    ) -> anyhow::Result<ProviderSnapshot>;
    async fn remove_key(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
    ) -> anyhow::Result<ProviderSnapshot>;
    async fn remove_header(
        &self,
        provider_id: &str,
        expect: u64,
        header_id: &str,
    ) -> anyhow::Result<ProviderSnapshot>;
    async fn set_key_enabled(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
        enabled: bool,
    ) -> anyhow::Result<ProviderSnapshot>;
    async fn pending_credential_ops(&self) -> anyhow::Result<Vec<CredentialOperation>>;
    async fn finish_credential_cleanup(&self, reference: &str) -> anyhow::Result<()>;
    async fn credential_ref_is_live(&self, reference: &str) -> anyhow::Result<bool>;

    // ---- Preferences ----
    async fn load_preferences(
        &self,
        target: BackendTarget,
    ) -> anyhow::Result<AgentPreferences>;
    async fn save_preferences(
        &self,
        target: BackendTarget,
        expect: u64,
        value: AgentPreferences,
    ) -> anyhow::Result<AgentPreferences>;
}


// ---------------------------------------------------------------------------
// Service errors
// ---------------------------------------------------------------------------

/// Errors returned by `ProviderService` operations. Variants map to concrete
/// UI reactions.
#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(transparent)]
    Invalid(#[from] crate::ValidationError),

    /// The credential store could not be reached. The caller's draft is
    /// intact; the user can retry when connectivity returns.
    #[error("credential store unavailable")]
    CredentialUnavailable,

    /// The credential store refused the request (platform cap, denial). The
    /// draft is intact; retrying with the same value will fail again.
    #[error("credential store rejected: {0}")]
    CredentialRejected(String),

    /// An unexpected storage error. Treat as transient; the caller MAY retry
    /// once but MUST NOT assume success without re-reading state.
    #[error(transparent)]
    Storage(#[from] anyhow::Error),
}

impl ServiceError {
    fn from_credential(err: CredentialError) -> Self {
        match err {
            CredentialError::Unavailable => Self::CredentialUnavailable,
            CredentialError::Rejected(message) => Self::CredentialRejected(message),
            CredentialError::Backend(e) => Self::Storage(e),
        }
    }
}

pub type ServiceResult<T> = std::result::Result<T, ServiceError>;

// ---------------------------------------------------------------------------
// Service
// ---------------------------------------------------------------------------

/// Holds the metadata repo, the credential store, and the change broadcast.
/// Clone is cheap when `R` and `C` are `Arc`-based (SQL handle + mpsc sender);
/// the caller controls that side.
pub struct ProviderService<R, C> {
    repo: R,
    credentials: C,
    changes: ProviderChangeSender,
}

impl<R, C> ProviderService<R, C>
where
    R: ProviderRepository,
    C: CredentialStore,
{
    pub fn new(repo: R, credentials: C) -> Self {
        Self {
            repo,
            credentials,
            changes: ProviderChangeSender::new(),
        }
    }

    pub fn repo(&self) -> &R {
        &self.repo
    }

    pub fn credentials(&self) -> &C {
        &self.credentials
    }

    /// Subscribes to provider state transitions. Each subscription is backed
    /// by a bounded buffer; slow receivers may lose events, which is safe
    /// because every event is advisory (snapshots are re-fetched on demand).
    pub fn subscribe_changes(&self) -> tokio::sync::broadcast::Receiver<ProviderChange> {
        self.changes.subscribe()
    }

    fn publish(&self, kind: ProviderChangeKind, provider_id: Option<String>, revision: u64) {
        self.changes.send(ProviderChange {
            kind,
            provider_id,
            target: None,
            revision,
        });
    }

    fn publish_preferences(&self, target: BackendTarget, revision: u64) {
        self.changes.send(ProviderChange {
            kind: ProviderChangeKind::Preferences,
            provider_id: None,
            target: Some(target),
            revision,
        });
    }

    /// Generates a fresh credential reference. Externally observable and
    /// unique across the running host.
    fn mint_reference() -> String {
        format!("{CREDENTIAL_REF_PREFIX}{}", Uuid::new_v4())
    }

    /// Steps §5.3.1–§5.3.6 applied to a Key slot.
    ///
    /// `template` carries the metadata that will end up in SQL: `masked`,
    /// `label`, `enabled`, `sort` and optionally the existing `id` (for a
    /// rotation). The `secret` is written to the credential store and
    /// verified by read-back before metadata is swapped in SQL. The new
    /// `credential_ref` is minted by the service, NOT supplied by callers.
    pub async fn commit_key_secret(
        &self,
        provider_id: &str,
        expect: u64,
        mut template: KeyRecord,
        secret: Secret,
    ) -> ServiceResult<ProviderSnapshot> {
        let reference = Self::mint_reference();
        template.credential_ref = reference.clone();
        self.write_and_verify(&reference, secret).await?;
        match self
            .repo
            .commit_key(provider_id, expect, template)
            .await
        {
            Ok(snapshot) => {
                self.publish(
                    ProviderChangeKind::Key,
                    Some(snapshot.id.clone()),
                    snapshot.revision,
                );
                Ok(snapshot)
            }
            Err(err) => {
                // SQL failed after a successful credential write: clean the
                // orphan ref so it is never reused. A delete failure here is
                // logged but doesn't shadow the original SQL error — the
                // pending_put row (if any) is picked up by the reconciler.
                let _ = self.credentials.delete(&reference).await;
                let _ = self.repo.finish_credential_cleanup(&reference).await;
                Err(ServiceError::Storage(err))
            }
        }
    }

    /// Mirror of `commit_key_secret` for Header slots.
    pub async fn commit_header_secret(
        &self,
        provider_id: &str,
        expect: u64,
        mut template: HeaderRecord,
        secret: Secret,
    ) -> ServiceResult<ProviderSnapshot> {
        crate::validate::header_name(&template.name)?;
        crate::validate::header_value(secret.expose())?;
        let reference = Self::mint_reference();
        template.credential_ref = reference.clone();
        self.write_and_verify(&reference, secret).await?;
        match self
            .repo
            .commit_header(provider_id, expect, template)
            .await
        {
            Ok(snapshot) => {
                self.publish(
                    ProviderChangeKind::Header,
                    Some(snapshot.id.clone()),
                    snapshot.revision,
                );
                Ok(snapshot)
            }
            Err(err) => {
                let _ = self.credentials.delete(&reference).await;
                let _ = self.repo.finish_credential_cleanup(&reference).await;
                Err(ServiceError::Storage(err))
            }
        }
    }

    /// Removes a Key: SQL deletes the row and enqueues the old ref, then the
    /// service best-efforts the credential delete. A credential-delete
    /// failure does not fail the operation; the reconciler retries later.
    pub async fn remove_key(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
    ) -> ServiceResult<ProviderSnapshot> {
        let snapshot = self.repo.remove_key(provider_id, expect, key_id).await?;
        self.purge_pending_deletes().await;
        self.publish(
            ProviderChangeKind::Key,
            Some(snapshot.id.clone()),
            snapshot.revision,
        );
        Ok(snapshot)
    }

    pub async fn remove_header(
        &self,
        provider_id: &str,
        expect: u64,
        header_id: &str,
    ) -> ServiceResult<ProviderSnapshot> {
        let snapshot = self
            .repo
            .remove_header(provider_id, expect, header_id)
            .await?;
        self.purge_pending_deletes().await;
        self.publish(
            ProviderChangeKind::Header,
            Some(snapshot.id.clone()),
            snapshot.revision,
        );
        Ok(snapshot)
    }

    /// Toggle whether a stored Key is enabled, without a credential round-trip.
    pub async fn set_key_enabled(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
        enabled: bool,
    ) -> ServiceResult<ProviderSnapshot> {
        let snapshot = self
            .repo
            .set_key_enabled(provider_id, expect, key_id, enabled)
            .await?;
        self.publish(
            ProviderChangeKind::Key,
            Some(snapshot.id.clone()),
            snapshot.revision,
        );
        Ok(snapshot)
    }

    // --------- Provider CRUD ---------

    pub async fn list_providers(&self) -> ServiceResult<Vec<ProviderSnapshot>> {
        Ok(self.repo.list_providers().await?)
    }

    /// Returns saved services after idempotently inserting disabled public templates.
    pub async fn seed_presets(&self) -> ServiceResult<Vec<ProviderSnapshot>> {
        let providers = self.repo.seed_presets().await?;
        for provider in &providers {
            self.publish(ProviderChangeKind::Provider, Some(provider.id.clone()), provider.revision);
        }
        Ok(self.repo.list_providers().await?)
    }

    pub async fn get_provider(&self, id: &str) -> ServiceResult<Option<ProviderSnapshot>> {
        Ok(self.repo.get_provider(id).await?)
    }

    pub async fn create_provider(&self, draft: ProviderDraft) -> ServiceResult<ProviderSnapshot> {
        crate::validate::draft(&draft)?;
        let snapshot = self.repo.insert_provider(draft).await?;
        self.publish(
            ProviderChangeKind::Provider,
            Some(snapshot.id.clone()),
            snapshot.revision,
        );
        Ok(snapshot)
    }

    pub async fn update_provider(
        &self,
        id: &str,
        expect: u64,
        draft: ProviderDraft,
    ) -> ServiceResult<ProviderSnapshot> {
        crate::validate::draft(&draft)?;
        let snapshot = self.repo.update_provider(id, expect, draft).await?;
        self.publish(
            ProviderChangeKind::Provider,
            Some(snapshot.id.clone()),
            snapshot.revision,
        );
        Ok(snapshot)
    }

    /// Deletes the provider. Credential refs are enqueued by the repo and
    /// best-effort purged from the credential store; the broadcast fires
    /// regardless of whether every credential was actually wiped (the
    /// reconciler mops up the rest).
    pub async fn delete_provider(&self, id: &str, expect: u64) -> ServiceResult<()> {
        let refs = self.repo.delete_provider(id, expect).await?;
        for reference in &refs {
            if self.credentials.delete(reference).await.is_ok() {
                let _ = self.repo.finish_credential_cleanup(reference).await;
            }
        }
        // revision == 0 marks "the row is gone"; subscribers that cache by id
        // can evict unconditionally.
        self.publish(ProviderChangeKind::Deleted, Some(id.to_owned()), 0);
        Ok(())
    }

    // --------- Reveal (explicit opt-in to plaintext) ---------

    /// Reveals the plaintext secret behind a Key slot. `None` means the row
    /// exists but the credential store does not have the value (for example
    /// the keychain is locked). Callers MUST gate this behind a user action.
    pub async fn reveal_key(
        &self,
        provider_id: &str,
        key_id: &str,
    ) -> ServiceResult<Option<Secret>> {
        let snapshot = self.require_snapshot(provider_id).await?;
        let credential_ref = snapshot
            .keys
            .iter()
            .find(|k| k.id == key_id)
            .map(|k| k.credential_ref.clone());
        let Some(credential_ref) = credential_ref else {
            return Ok(None);
        };
        self.credentials
            .get(&credential_ref)
            .await
            .map_err(ServiceError::from_credential)
    }

    pub async fn reveal_header(
        &self,
        provider_id: &str,
        header_id: &str,
    ) -> ServiceResult<Option<Secret>> {
        let snapshot = self.require_snapshot(provider_id).await?;
        let credential_ref = snapshot
            .headers
            .iter()
            .find(|h| h.id == header_id)
            .map(|h| h.credential_ref.clone());
        let Some(credential_ref) = credential_ref else {
            return Ok(None);
        };
        self.credentials
            .get(&credential_ref)
            .await
            .map_err(ServiceError::from_credential)
    }

    async fn require_snapshot(&self, provider_id: &str) -> ServiceResult<ProviderSnapshot> {
        self.repo
            .get_provider(provider_id)
            .await?
            .ok_or_else(|| {
                ServiceError::Storage(anyhow::anyhow!("provider {provider_id} not found"))
            })
    }

    // --------- Preferences ---------

    pub async fn load_preferences(
        &self,
        target: BackendTarget,
    ) -> ServiceResult<AgentPreferences> {
        Ok(self.repo.load_preferences(target).await?)
    }

    pub async fn save_preferences(
        &self,
        target: BackendTarget,
        expect: u64,
        value: AgentPreferences,
    ) -> ServiceResult<AgentPreferences> {
        let saved = self.repo.save_preferences(target, expect, value).await?;
        self.publish_preferences(target, saved.revision);
        Ok(saved)
    }

}


/// How many credential refs the reconciler touched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileReport {
    pub cleaned: u32,
    pub retained: u32,
}

#[cfg(test)]
mod tests;
