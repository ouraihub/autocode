//! Fault-injection tests for `ProviderService`.
//!
//! The service orchestrates two trait edges (SQL metadata and credential
//! store). Behavioural safety requires that at every failure point of either
//! edge, the user's existing configuration survives and no orphan
//! credential is ever accepted silently. These tests drive the service with
//! a mock `CredentialMetadataRepo` (`MockRepo`) and a real `InMemoryVault`
//! so both halves have observable state.
//!
//! Scope per `docs/03-详细设计.md` §5.3 and §4.1:
//!
//! - put failure keeps the user draft; no SQL row is written.
//! - get-mismatch is treated as `Rejected` and clears the orphan credential.
//! - SQL commit failure after a successful put clears the orphan credential.
//! - remove drains `pending_delete` when the vault is reachable.
//! - reconcile cleans pending_put for orphans and pending_delete for ghosts,
//!   and retains both when the vault is unavailable or the ref is live.
//!
//! The style rule for match ergonomics (ref / ref mut) applies: we borrow
//! the scrutinee instead of using `ref` bindings.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{
    AgentPreferences, BackendTarget, CredentialOpState, CredentialOperation, CredentialStore,
    EndpointSnapshot, HeaderRecord, HeaderSummary, InMemoryVault, KeyRecord, KeySummary, Protocol,
    ProviderDraft, ProviderRepository, ProviderService, ProviderSnapshot, RoutingMode, Secret,
    ServiceError,
};

// ---------------------------------------------------------------------------
// MockRepo
// ---------------------------------------------------------------------------

#[derive(Default)]
struct MockRepoInner {
    /// A provider row plus its expected revision. `None` means
    /// "provider does not exist", which `commit_key` turns into an
    /// `anyhow` error surfacing as `ServiceError::Storage` upstream.
    provider: Option<MockProvider>,
    /// Credential refs the SQL layer logged as pending_put.
    pending_put: HashSet<String>,
    /// Credential refs logged as pending_delete.
    pending_delete: HashSet<String>,
    /// If `Some`, the next `commit_*` call fails with this error.
    fail_next_commit: Option<String>,
    /// Count of finish_credential_cleanup invocations (for test observability).
    cleanup_calls: u32,
    /// Saved preferences per backend; empty map means "no row saved yet".
    preferences: HashMap<BackendTarget, AgentPreferences>,
}

struct MockProvider {
    id: String,
    revision: u64,
    keys: HashMap<String, KeySummary>,
    headers: HashMap<String, HeaderSummary>,
}

impl MockProvider {
    fn snapshot(&self) -> ProviderSnapshot {
        let mut keys: Vec<KeySummary> = self.keys.values().cloned().collect();
        keys.sort_by(|a, b| a.sort.cmp(&b.sort).then_with(|| a.id.cmp(&b.id)));
        let mut headers: Vec<HeaderSummary> = self.headers.values().cloned().collect();
        headers.sort_by(|a, b| a.id.cmp(&b.id));
        ProviderSnapshot {
            id: self.id.clone(),
            preset_id: None,
            revision: self.revision,
            name: "mock".to_owned(),
            enabled: true,
            routing: RoutingMode::Direct,
            auth: crate::AuthMode::ApiKey,
            default_protocol: Protocol::OpenaiChat,
            endpoints: vec![EndpointSnapshot {
                protocol: Protocol::OpenaiChat,
                base_url: "https://api.example".to_owned(),
            }],
            models: vec![],
            keys,
            headers,
            sort: 0,
            created_at: 0,
            updated_at: 0,
        }
    }
}

#[derive(Clone, Default)]
struct MockRepo {
    inner: Arc<Mutex<MockRepoInner>>,
}

impl MockRepo {
    fn new() -> Self {
        let inner = MockRepoInner {
            provider: Some(MockProvider {
                id: "p1".to_owned(),
                revision: 1,
                keys: HashMap::new(),
                headers: HashMap::new(),
            }),
            ..Default::default()
        };
        Self {
            inner: Arc::new(Mutex::new(inner)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, MockRepoInner> {
        self.inner.lock().expect("mock poisoned")
    }

    fn fail_next_commit_with(&self, msg: impl Into<String>) {
        self.lock().fail_next_commit = Some(msg.into());
    }

    fn set_revision(&self, rev: u64) {
        if let Some(provider) = &mut self.lock().provider {
            provider.revision = rev;
        }
    }
}

#[async_trait]
impl ProviderRepository for MockRepo {
    async fn begin_credential_put(&self, reference: &str) -> anyhow::Result<()> {
        self.lock().pending_put.insert(reference.to_owned());
        Ok(())
    }

    async fn commit_key(
        &self,
        provider_id: &str,
        expect: u64,
        record: KeyRecord,
    ) -> anyhow::Result<ProviderSnapshot> {
        let mut inner = self.lock();
        if let Some(msg) = inner.fail_next_commit.take() {
            return Err(anyhow::anyhow!(msg));
        }
        let provider = inner
            .provider
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("provider missing"))?;
        anyhow::ensure!(provider.id == provider_id, "wrong provider id");
        anyhow::ensure!(provider.revision == expect, "revision mismatch");
        let key_id = record.id.clone().unwrap_or_else(|| "k1".to_owned());
        let old = provider.keys.insert(
            key_id.clone(),
            KeySummary {
                id: key_id,
                label: record.label.clone(),
                masked: record.masked.clone(),
                credential_ref: record.credential_ref.clone(),
                enabled: record.enabled,
                sort: record.sort,
            },
        );
        provider.revision += 1;
        inner.pending_put.remove(&record.credential_ref);
        if let Some(old) = old
            && old.credential_ref != record.credential_ref
        {
            inner.pending_delete.insert(old.credential_ref);
        }
        Ok(inner.provider.as_ref().unwrap().snapshot())
    }

    async fn commit_header(
        &self,
        _provider_id: &str,
        _expect: u64,
        _record: HeaderRecord,
    ) -> anyhow::Result<ProviderSnapshot> {
        // Headers mirror keys exactly; the tests here only need the key path.
        // Keeping this as a stub makes any accidental use blow up.
        Err(anyhow::anyhow!("headers not exercised by these tests"))
    }

    async fn remove_key(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
    ) -> anyhow::Result<ProviderSnapshot> {
        let mut inner = self.lock();
        let provider = inner
            .provider
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("provider missing"))?;
        anyhow::ensure!(provider.id == provider_id, "wrong provider id");
        anyhow::ensure!(provider.revision == expect, "revision mismatch");
        let removed = provider
            .keys
            .remove(key_id)
            .ok_or_else(|| anyhow::anyhow!("key missing"))?;
        provider.revision += 1;
        inner.pending_delete.insert(removed.credential_ref);
        Ok(inner.provider.as_ref().unwrap().snapshot())
    }

    async fn remove_header(
        &self,
        _provider_id: &str,
        _expect: u64,
        _header_id: &str,
    ) -> anyhow::Result<ProviderSnapshot> {
        Err(anyhow::anyhow!("headers not exercised by these tests"))
    }

    async fn pending_credential_ops(&self) -> anyhow::Result<Vec<CredentialOperation>> {
        let inner = self.lock();
        let mut ops: Vec<CredentialOperation> = inner
            .pending_put
            .iter()
            .map(|r| CredentialOperation {
                credential_ref: r.clone(),
                state: CredentialOpState::PendingPut,
                created_at: 0,
            })
            .chain(inner.pending_delete.iter().map(|r| CredentialOperation {
                credential_ref: r.clone(),
                state: CredentialOpState::PendingDelete,
                created_at: 0,
            }))
            .collect();
        ops.sort_by(|a, b| a.credential_ref.cmp(&b.credential_ref));
        Ok(ops)
    }

    async fn finish_credential_cleanup(&self, reference: &str) -> anyhow::Result<()> {
        let mut inner = self.lock();
        inner.pending_put.remove(reference);
        inner.pending_delete.remove(reference);
        inner.cleanup_calls += 1;
        Ok(())
    }

    async fn credential_ref_is_live(&self, reference: &str) -> anyhow::Result<bool> {
        let inner = self.lock();
        let live = inner
            .provider
            .as_ref()
            .map(|p| {
                p.keys.values().any(|k| k.credential_ref == reference)
                    || p.headers.values().any(|h| h.credential_ref == reference)
            })
            .unwrap_or(false);
        Ok(live)
    }

    async fn list_providers(&self) -> anyhow::Result<Vec<ProviderSnapshot>> {
        Ok(self.lock().provider.iter().map(|p| p.snapshot()).collect())
    }

    async fn seed_presets(&self) -> anyhow::Result<Vec<ProviderSnapshot>> { Ok(Vec::new()) }

    async fn get_provider(&self, id: &str) -> anyhow::Result<Option<ProviderSnapshot>> {
        Ok(self
            .lock()
            .provider
            .as_ref()
            .filter(|p| p.id == id)
            .map(|p| p.snapshot()))
    }

    async fn insert_provider(
        &self,
        _draft: ProviderDraft,
    ) -> anyhow::Result<ProviderSnapshot> {
        Err(anyhow::anyhow!("insert_provider not exercised"))
    }

    async fn update_provider(
        &self,
        _id: &str,
        _expect: u64,
        _draft: ProviderDraft,
    ) -> anyhow::Result<ProviderSnapshot> {
        Err(anyhow::anyhow!("update_provider not exercised"))
    }

    async fn delete_provider(&self, id: &str, expect: u64) -> anyhow::Result<Vec<String>> {
        let mut inner = self.lock();
        let provider = inner
            .provider
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("provider missing"))?;
        anyhow::ensure!(provider.id == id, "wrong provider id");
        anyhow::ensure!(provider.revision == expect, "revision mismatch");
        let mut refs: Vec<String> = provider
            .keys
            .values()
            .map(|k| k.credential_ref.clone())
            .chain(provider.headers.values().map(|h| h.credential_ref.clone()))
            .collect();
        refs.sort();
        for r in &refs {
            inner.pending_delete.insert(r.clone());
        }
        inner.provider = None;
        Ok(refs)
    }

    async fn set_key_enabled(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
        enabled: bool,
    ) -> anyhow::Result<ProviderSnapshot> {
        let mut inner = self.lock();
        let provider = inner
            .provider
            .as_mut()
            .ok_or_else(|| anyhow::anyhow!("provider missing"))?;
        anyhow::ensure!(provider.id == provider_id, "wrong provider id");
        anyhow::ensure!(provider.revision == expect, "revision mismatch");
        let key = provider
            .keys
            .get_mut(key_id)
            .ok_or_else(|| anyhow::anyhow!("key missing"))?;
        key.enabled = enabled;
        provider.revision += 1;
        Ok(inner.provider.as_ref().unwrap().snapshot())
    }

    async fn load_preferences(
        &self,
        target: BackendTarget,
    ) -> anyhow::Result<AgentPreferences> {
        Ok(self
            .lock()
            .preferences
            .get(&target)
            .cloned()
            .unwrap_or_else(|| AgentPreferences::native_default(target)))
    }

    async fn save_preferences(
        &self,
        target: BackendTarget,
        expect: u64,
        mut value: AgentPreferences,
    ) -> anyhow::Result<AgentPreferences> {
        let mut inner = self.lock();
        let current = inner
            .preferences
            .get(&target)
            .map(|p| p.revision)
            .unwrap_or(0);
        anyhow::ensure!(current == expect, "preferences revision mismatch");
        value.revision = expect + 1;
        inner.preferences.insert(target, value.clone());
        Ok(value)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn template(masked: &str) -> KeyRecord {
    KeyRecord {
        id: None,
        label: Some("primary".into()),
        masked: masked.into(),
        credential_ref: String::new(), // minted by the service
        enabled: true,
        sort: 0,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn commit_key_round_trip_writes_credential_and_bumps_revision() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());

    let snap = service
        .commit_key_secret("p1", 1, template("sk-***"), Secret::new("live"))
        .await
        .expect("commit succeeds");
    assert_eq!(snap.revision, 2);
    assert_eq!(snap.keys.len(), 1);
    let stored_ref = snap.keys[0].credential_ref.clone();
    assert!(stored_ref.starts_with(crate::CREDENTIAL_REF_PREFIX));

    // Credential actually in the vault.
    let got = vault.get(&stored_ref).await.unwrap().unwrap();
    assert_eq!(got.expose(), "live");

    // pending_put was cleared by the commit.
    let ops = repo.pending_credential_ops().await.unwrap();
    assert!(ops.is_empty(), "no pending ops after clean commit, got {ops:?}");
}

#[tokio::test]
async fn put_failure_leaves_sql_untouched_and_no_orphan() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    vault.fail_next_with("platform cap exceeded");
    let service = ProviderService::new(repo.clone(), vault.clone());

    let err = service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .expect_err("put must fail");
    assert!(
        matches!(&err, ServiceError::CredentialRejected(m) if m.contains("cap")),
        "got {err:?}"
    );

    // SQL provider still at revision 1 with no keys.
    let snap = repo.lock().provider.as_ref().unwrap().snapshot();
    assert_eq!(snap.revision, 1);
    assert!(snap.keys.is_empty());

    // Vault must not retain anything: write failed outright.
    assert!(vault.is_empty(), "vault should be clean: {:?}", vault.take_references());

    // Pending_put was written (step 2) then cleared (service's cleanup).
    // The log is NOT drained by the service on put failure: the broker never
    // succeeded, so there's no stray credential. But the SQL log stays until
    // reconcile. Assert so the design intent is captured.
    let ops = repo.pending_credential_ops().await.unwrap();
    assert_eq!(ops.len(), 1, "pending_put remains until reconcile: {ops:?}");
    assert_eq!(ops[0].state, CredentialOpState::PendingPut);
}

#[tokio::test]
async fn sql_commit_failure_after_put_drops_orphan_credential() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    repo.fail_next_commit_with("revision conflict in SQL");
    let service = ProviderService::new(repo.clone(), vault.clone());

    let err = service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .expect_err("commit must fail");
    assert!(matches!(&err, ServiceError::Storage(_)), "got {err:?}");

    // The service must have cleaned the credential it wrote before SQL failed.
    assert!(
        vault.is_empty(),
        "orphan credential not cleaned: {:?}",
        vault.take_references()
    );

    // Provider stays at revision 1, no keys.
    let snap = repo.lock().provider.as_ref().unwrap().snapshot();
    assert_eq!(snap.revision, 1);
    assert!(snap.keys.is_empty());
}

#[tokio::test]
async fn remove_key_drains_pending_delete_when_vault_reachable() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());

    // First commit a key so we have something to remove.
    let snap = service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .unwrap();
    let key_id = snap.keys[0].id.clone();
    let ref_before = snap.keys[0].credential_ref.clone();

    let after = service
        .remove_key("p1", snap.revision, &key_id)
        .await
        .unwrap();
    assert!(after.keys.is_empty());

    // After the best-effort purge the credential is gone from both sides.
    assert!(vault.get(&ref_before).await.unwrap().is_none());
    let ops = repo.pending_credential_ops().await.unwrap();
    assert!(ops.is_empty(), "pending_delete drained, got {ops:?}");
}

#[tokio::test]
async fn reconcile_cleans_orphan_pending_put_when_ref_is_not_live() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    // Simulate a crashed commit: pending_put logged + credential actually
    // written, but no Key row adopted the ref.
    let orphan_ref = format!("{}orphan", crate::CREDENTIAL_REF_PREFIX);
    repo.lock().pending_put.insert(orphan_ref.clone());
    vault.put(&orphan_ref, Secret::new("ghost")).await.unwrap();

    let service = ProviderService::new(repo.clone(), vault.clone());
    let report = service.reconcile_pending().await.unwrap();
    assert_eq!(report.cleaned, 1);
    assert_eq!(report.retained, 0);

    assert!(vault.is_empty());
    assert!(repo.pending_credential_ops().await.unwrap().is_empty());
}

#[tokio::test]
async fn reconcile_retains_pending_put_when_ref_is_live() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());

    // Commit a key normally, then manually re-insert a pending_put log as if
    // a second half-committed put landed on the same ref.
    let snap = service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .unwrap();
    let live_ref = snap.keys[0].credential_ref.clone();
    repo.lock().pending_put.insert(live_ref.clone());

    let report = service.reconcile_pending().await.unwrap();
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.retained, 1, "live ref must not be deleted");

    // Credential is still readable.
    assert!(vault.get(&live_ref).await.unwrap().is_some());
}

#[tokio::test]
async fn reconcile_cleans_pending_delete_when_ref_is_not_live() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let dead_ref = format!("{}dead", crate::CREDENTIAL_REF_PREFIX);
    repo.lock().pending_delete.insert(dead_ref.clone());
    vault.put(&dead_ref, Secret::new("stale")).await.unwrap();

    let service = ProviderService::new(repo.clone(), vault.clone());
    let report = service.reconcile_pending().await.unwrap();
    assert_eq!(report.cleaned, 1);
    assert_eq!(report.retained, 0);
    assert!(vault.is_empty());
    assert!(repo.pending_credential_ops().await.unwrap().is_empty());
}

#[tokio::test]
async fn reconcile_drops_pending_delete_intent_when_ref_is_live_again() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());

    // Commit a key so a live row exists, then manually stage a pending_delete
    // for the same ref (simulating a concurrent rotation that re-adopted it).
    let snap = service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .unwrap();
    let live_ref = snap.keys[0].credential_ref.clone();
    repo.lock().pending_delete.insert(live_ref.clone());

    let report = service.reconcile_pending().await.unwrap();
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.retained, 1);

    // Credential must still be readable: we dropped the delete intent.
    assert!(vault.get(&live_ref).await.unwrap().is_some());
}

#[tokio::test]
async fn reconcile_retains_when_vault_unavailable() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let orphan_ref = format!("{}orphan", crate::CREDENTIAL_REF_PREFIX);
    repo.lock().pending_delete.insert(orphan_ref.clone());
    vault.put(&orphan_ref, Secret::new("stale")).await.unwrap();
    vault.fail_next_with("denied by platform");

    let service = ProviderService::new(repo.clone(), vault.clone());
    let report = service.reconcile_pending().await.unwrap();
    assert_eq!(report.cleaned, 0);
    assert_eq!(report.retained, 1, "retry on next boot; credential kept");

    // Pending_delete still present for next reconcile.
    let ops = repo.pending_credential_ops().await.unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].state, CredentialOpState::PendingDelete);
}

#[tokio::test]
async fn commit_key_fails_on_revision_mismatch_and_keeps_vault_clean() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    repo.set_revision(5);
    let service = ProviderService::new(repo.clone(), vault.clone());

    let err = service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .expect_err("stale expect must fail");
    assert!(matches!(&err, ServiceError::Storage(_)), "got {err:?}");

    // Vault was cleaned after the SQL error.
    assert!(vault.is_empty(), "orphan ref: {:?}", vault.take_references());
}

#[tokio::test]
async fn commit_key_broadcasts_after_success_only() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());
    let mut rx = service.subscribe_changes();

    service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .unwrap();

    let event = rx.try_recv().expect("broadcast must have fired");
    assert_eq!(event.kind, crate::ProviderChangeKind::Key);
    assert_eq!(event.provider_id.as_deref(), Some("p1"));
    assert_eq!(event.revision, 2, "revision must be post-commit");
}

#[tokio::test]
async fn commit_key_does_not_broadcast_on_failure() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    repo.fail_next_commit_with("db locked");
    let service = ProviderService::new(repo.clone(), vault.clone());
    let mut rx = service.subscribe_changes();

    service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .expect_err("SQL commit should fail");

    // broadcast::Receiver::try_recv returns `Empty` when nothing has been sent.
    assert!(matches!(
        rx.try_recv(),
        Err(tokio::sync::broadcast::error::TryRecvError::Empty)
    ));
}

#[tokio::test]
async fn save_preferences_publishes_target_scoped_event() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());
    let mut rx = service.subscribe_changes();

    let default = service.load_preferences(BackendTarget::Pi).await.unwrap();
    let saved = service
        .save_preferences(BackendTarget::Pi, 0, default.clone())
        .await
        .unwrap();
    assert_eq!(saved.revision, 1);

    let event = rx.try_recv().unwrap();
    assert_eq!(event.kind, crate::ProviderChangeKind::Preferences);
    assert_eq!(event.target, Some(BackendTarget::Pi));
    assert_eq!(event.provider_id, None, "preferences event has no provider id");
    assert_eq!(event.revision, 1);
}

#[tokio::test]
async fn reveal_key_returns_plaintext_only_after_commit() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());

    assert!(
        service.reveal_key("p1", "missing").await.unwrap().is_none(),
        "revealing a non-existent key id must return None, not an error"
    );

    let snapshot = service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .unwrap();
    let key_id = &snapshot.keys[0].id;

    let revealed = service.reveal_key("p1", key_id).await.unwrap();
    assert_eq!(revealed.expect("secret present").expose(), "live");
}

#[tokio::test]
async fn delete_provider_cleans_credentials_and_broadcasts_deleted() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());
    let snapshot = service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .unwrap();
    assert_eq!(vault.len(), 1);

    let mut rx = service.subscribe_changes();
    service
        .delete_provider("p1", snapshot.revision)
        .await
        .unwrap();

    assert!(vault.is_empty(), "credentials must be purged");
    let event = rx.try_recv().unwrap();
    assert_eq!(event.kind, crate::ProviderChangeKind::Deleted);
    assert_eq!(event.provider_id.as_deref(), Some("p1"));
    assert_eq!(event.revision, 0, "deleted events carry revision 0");
    assert!(service.get_provider("p1").await.unwrap().is_none());
}

#[tokio::test]
async fn set_key_enabled_toggles_without_credential_touch() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());
    let snapshot = service
        .commit_key_secret("p1", 1, template("sk"), Secret::new("live"))
        .await
        .unwrap();

    let key_id = snapshot.keys[0].id.clone();
    let toggled = service
        .set_key_enabled("p1", snapshot.revision, &key_id, false)
        .await
        .unwrap();
    assert!(!toggled.keys[0].enabled);
    // Vault is untouched by the toggle.
    assert_eq!(vault.len(), 1, "credential bytes preserved");
}

#[tokio::test]
async fn invalid_header_is_rejected_before_credentials_or_metadata_change() {
    let repo = MockRepo::new();
    let vault = InMemoryVault::new();
    let service = ProviderService::new(repo.clone(), vault.clone());
    for (name, value, expected) in [
        ("Authorization", "secret", crate::ValidationError::HeaderReserved("Authorization".into())),
        ("X-Custom", "secret\r\nInjected: true", crate::ValidationError::HeaderValueControl),
    ] {
        let error = service.commit_header_secret("p1", 1, HeaderRecord {
            id: None, name: name.into(), masked: "***".into(),
            credential_ref: String::new(), enabled: true,
        }, Secret::new(value)).await.unwrap_err();
        assert!(matches!(error, ServiceError::Invalid(actual) if actual == expected));
        assert!(vault.is_empty());
        let saved = service.get_provider("p1").await.unwrap().unwrap();
        assert_eq!(saved.revision, 1);
        assert!(saved.headers.is_empty());
        assert!(repo.pending_credential_ops().await.unwrap().is_empty());
    }
}
