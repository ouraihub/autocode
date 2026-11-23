//! `ProviderRepository for ProviderDb`.
//!
//! The trait and the service that consumes it live in `model_provider`; this
//! file is the thin SQL-side wiring that maps each trait method to a
//! `ProviderDb` method and promotes `ProviderStoreError` into the
//! trait-level `anyhow::Error`. Keeping the glue tiny keeps the service
//! independent of sqlez.

use async_trait::async_trait;
use model_provider::{
    AgentPreferences, BackendTarget, CredentialOperation, HeaderRecord, KeyRecord,
    ProviderDraft, ProviderRepository, ProviderSnapshot,
};

use super::ProviderDb;

#[async_trait]
impl ProviderRepository for ProviderDb {
    // ---- Provider CRUD ----
    async fn seed_presets(&self) -> anyhow::Result<Vec<ProviderSnapshot>> {
        ProviderDb::seed_presets(self).await.map_err(Into::into)
    }

    async fn list_providers(&self) -> anyhow::Result<Vec<ProviderSnapshot>> {
        ProviderDb::list_providers(self).await.map_err(Into::into)
    }

    async fn get_provider(&self, id: &str) -> anyhow::Result<Option<ProviderSnapshot>> {
        ProviderDb::get_provider(self, id).await.map_err(Into::into)
    }

    async fn insert_provider(
        &self,
        draft: ProviderDraft,
    ) -> anyhow::Result<ProviderSnapshot> {
        ProviderDb::insert_provider(self, draft)
            .await
            .map_err(Into::into)
    }

    async fn update_provider(
        &self,
        id: &str,
        expect: u64,
        draft: ProviderDraft,
    ) -> anyhow::Result<ProviderSnapshot> {
        ProviderDb::update_provider(self, id, expect, draft)
            .await
            .map_err(Into::into)
    }

    async fn delete_provider(&self, id: &str, expect: u64) -> anyhow::Result<Vec<String>> {
        ProviderDb::delete_provider(self, id, expect)
            .await
            .map_err(Into::into)
    }

    // ---- Credential ops ----
    async fn begin_credential_put(&self, reference: &str) -> anyhow::Result<()> {
        ProviderDb::begin_credential_put(self, reference)
            .await
            .map_err(Into::into)
    }

    async fn commit_key(
        &self,
        provider_id: &str,
        expect: u64,
        record: KeyRecord,
    ) -> anyhow::Result<ProviderSnapshot> {
        ProviderDb::commit_key(self, provider_id, expect, record)
            .await
            .map_err(Into::into)
    }

    async fn commit_header(
        &self,
        provider_id: &str,
        expect: u64,
        record: HeaderRecord,
    ) -> anyhow::Result<ProviderSnapshot> {
        ProviderDb::commit_header(self, provider_id, expect, record)
            .await
            .map_err(Into::into)
    }

    async fn remove_key(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
    ) -> anyhow::Result<ProviderSnapshot> {
        ProviderDb::remove_key(self, provider_id, expect, key_id)
            .await
            .map_err(Into::into)
    }

    async fn remove_header(
        &self,
        provider_id: &str,
        expect: u64,
        header_id: &str,
    ) -> anyhow::Result<ProviderSnapshot> {
        ProviderDb::remove_header(self, provider_id, expect, header_id)
            .await
            .map_err(Into::into)
    }

    async fn set_key_enabled(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
        enabled: bool,
    ) -> anyhow::Result<ProviderSnapshot> {
        ProviderDb::set_key_enabled(self, provider_id, expect, key_id, enabled)
            .await
            .map_err(Into::into)
    }

    async fn pending_credential_ops(&self) -> anyhow::Result<Vec<CredentialOperation>> {
        ProviderDb::pending_credential_ops(self)
            .await
            .map_err(Into::into)
    }

    async fn finish_credential_cleanup(&self, reference: &str) -> anyhow::Result<()> {
        ProviderDb::finish_credential_cleanup(self, reference)
            .await
            .map_err(Into::into)
    }

    async fn credential_ref_is_live(&self, reference: &str) -> anyhow::Result<bool> {
        ProviderDb::credential_ref_is_live(self, reference)
            .await
            .map_err(Into::into)
    }

    // ---- Preferences ----
    async fn load_preferences(
        &self,
        target: BackendTarget,
    ) -> anyhow::Result<AgentPreferences> {
        ProviderDb::load_preferences(self, target)
            .await
            .map_err(Into::into)
    }

    async fn save_preferences(
        &self,
        target: BackendTarget,
        expect: u64,
        value: AgentPreferences,
    ) -> anyhow::Result<AgentPreferences> {
        ProviderDb::save_preferences(self, target, expect, value)
            .await
            .map_err(Into::into)
    }
}
