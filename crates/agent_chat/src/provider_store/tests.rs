//! Behavioural tests for `provider_store`.
//!
//! Per `docs/AI工程规范.md` §3.3 test fixtures are exempt from the 500-line
//! file cap; this file carries all round-trip and revision-conflict
//! scenarios. Add a focused helper rather than one-off boilerplate when a new
//! test needs a different draft shape.

use super::*;
use model_provider::{
    AuthMode, BackendTarget, ConfigSource, CredentialOpState, EndpointSnapshot, ModelSelection,
    Protocol, ProviderModel, RoutingMode,
};

async fn fresh_db(name: &'static str) -> ProviderDb {
    ProviderDb::open_test_db(name).await
}

fn sample_draft() -> ProviderDraft {
    ProviderDraft {
        name: "OpenAI Corp".to_owned(),
        enabled: true,
        routing: RoutingMode::Direct,
        auth: AuthMode::ApiKey,
        default_protocol: Protocol::OpenaiChat,
        endpoints: vec![EndpointSnapshot {
            protocol: Protocol::OpenaiChat,
            base_url: "https://api.openai.com".to_owned(),
        }],
        models: vec![ProviderModel {
            model_id: "gpt-x".to_owned(),
            model_type: model_provider::ModelType::Chat,
            name: "GPT-X".to_owned(),
            protocols: vec![Protocol::OpenaiChat],
            context_window: Some(128_000),
            max_output_tokens: Some(8_192),
            supports_vision: false,
            supports_tools: true,
            supports_reasoning: false,
            efforts: Vec::new(),
            enabled: true,
        }],
        sort: 0,
    }
}

#[gpui::test]
async fn insert_list_get_round_trip(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_round_trip").await;
    let snap = db.insert_provider(sample_draft()).await.unwrap();
    assert_eq!(snap.revision, 1);
    assert_eq!(snap.name, "OpenAI Corp");
    assert_eq!(snap.endpoints.len(), 1);
    assert_eq!(snap.models.len(), 1);
    assert!(snap.keys.is_empty());

    let listed = db.list_providers().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0], snap);

    let fetched = db.get_provider(&snap.id).await.unwrap().unwrap();
    assert_eq!(fetched, snap);
}

#[gpui::test]
async fn model_metadata_survives_insert_and_update(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_model_metadata").await;
    let mut draft = sample_draft();
    draft.models[0].supports_reasoning = true;
    draft.models[0].efforts = vec![model_provider::ThinkingLevel::Off, model_provider::ThinkingLevel::High];
    let inserted = db.insert_provider(draft.clone()).await.unwrap();
    assert_eq!(inserted.models, draft.models);
    draft.models[0].model_type = model_provider::ModelType::Embedding;
    draft.models[0].supports_reasoning = false;
    draft.models[0].efforts.clear();
    db.update_provider(&inserted.id, inserted.revision, draft.clone()).await.unwrap();
    let fetched = db.get_provider(&inserted.id).await.unwrap().unwrap();
    assert_eq!(fetched.models, draft.models);
}

#[gpui::test]
async fn update_bumps_revision_and_rejects_stale_expect(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_revision").await;
    let snap = db.insert_provider(sample_draft()).await.unwrap();

    let mut draft = sample_draft();
    draft.name = "OpenAI Corp Renamed".to_owned();
    let updated = db
        .update_provider(&snap.id, snap.revision, draft.clone())
        .await
        .unwrap();
    assert_eq!(updated.revision, 2);
    assert_eq!(updated.name, "OpenAI Corp Renamed");

    // Second update with the now-stale expect must surface Conflict.
    let err = db
        .update_provider(&snap.id, snap.revision, draft)
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderStoreError::Conflict(_, 1)), "got {err:?}");
}

#[gpui::test]
async fn update_missing_provider_is_not_found(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_update_missing").await;
    let err = db
        .update_provider("00000000-0000-0000-0000-000000000000", 1, sample_draft())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderStoreError::NotFound(_)), "got {err:?}");
}

#[gpui::test]
async fn delete_enqueues_and_cascades(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_delete").await;
    let snap = db.insert_provider(sample_draft()).await.unwrap();
    db.begin_credential_put("autocode-model-provider://credential/k1")
        .await
        .unwrap();
    let after_key = db
        .commit_key(
            &snap.id,
            snap.revision,
            KeyRecord {
                id: None,
                label: Some("primary".into()),
                masked: "sk-***abc".into(),
                credential_ref: "autocode-model-provider://credential/k1".into(),
                enabled: true,
                sort: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(after_key.revision, 2);
    assert_eq!(after_key.keys.len(), 1);

    let refs = db
        .delete_provider(&snap.id, after_key.revision)
        .await
        .unwrap();
    assert_eq!(refs, vec!["autocode-model-provider://credential/k1".to_owned()]);
    assert!(db.get_provider(&snap.id).await.unwrap().is_none());
    let ops = db.pending_credential_ops().await.unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].credential_ref, "autocode-model-provider://credential/k1");
    assert_eq!(ops[0].state, CredentialOpState::PendingDelete);
}

#[gpui::test]
async fn commit_key_replaces_and_enqueues_old_ref(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_key_replace").await;
    let snap = db.insert_provider(sample_draft()).await.unwrap();
    let v1 = db
        .commit_key(
            &snap.id,
            snap.revision,
            KeyRecord {
                id: None,
                label: None,
                masked: "sk-***v1".into(),
                credential_ref: "ref/v1".into(),
                enabled: true,
                sort: 0,
            },
        )
        .await
        .unwrap();
    let key_id = v1.keys[0].id.clone();

    let v2 = db
        .commit_key(
            &snap.id,
            v1.revision,
            KeyRecord {
                id: Some(key_id.clone()),
                label: Some("rotated".into()),
                masked: "sk-***v2".into(),
                credential_ref: "ref/v2".into(),
                enabled: true,
                sort: 0,
            },
        )
        .await
        .unwrap();
    assert_eq!(v2.keys.len(), 1);
    assert_eq!(v2.keys[0].credential_ref, "ref/v2");
    assert_eq!(v2.keys[0].masked, "sk-***v2");
    assert_eq!(v2.revision, v1.revision + 1);

    let ops = db.pending_credential_ops().await.unwrap();
    let refs: Vec<_> = ops.iter().map(|o| o.credential_ref.as_str()).collect();
    assert!(refs.contains(&"ref/v1"), "expected ref/v1 in pending_delete, got {refs:?}");
    assert!(!refs.contains(&"ref/v2"));
}

#[gpui::test]
async fn credential_op_log_round_trip(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_ops").await;
    db.begin_credential_put("ref/x").await.unwrap();
    let ops = db.pending_credential_ops().await.unwrap();
    assert_eq!(ops.len(), 1);
    assert_eq!(ops[0].state, CredentialOpState::PendingPut);
    db.finish_credential_cleanup("ref/x").await.unwrap();
    assert!(db.pending_credential_ops().await.unwrap().is_empty());
}

#[gpui::test]
async fn preferences_default_then_save_and_conflict(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_prefs").await;
    let default = db.load_preferences(BackendTarget::Pi).await.unwrap();
    assert_eq!(default.revision, 0);
    assert_eq!(default.source, ConfigSource::Native);

    let mut updated = default.clone();
    updated.source = ConfigSource::Managed;
    updated.provider_choice = Some(model_provider::EngineProviderChoice::Provider("pid".into()));
    updated.default_model = Some(ModelSelection::Managed {
        provider_id: "pid".into(),
        model_id: "mid".into(),
    });
    let saved = db
        .save_preferences(BackendTarget::Pi, 0, updated.clone())
        .await
        .unwrap();
    assert_eq!(saved.revision, 1);

    let err = db
        .save_preferences(BackendTarget::Pi, 0, updated.clone())
        .await
        .unwrap_err();
    assert!(matches!(err, ProviderStoreError::Conflict(_, 0)), "got {err:?}");

    let loaded = db.load_preferences(BackendTarget::Pi).await.unwrap();
    assert_eq!(loaded.revision, 1);
    assert_eq!(loaded.source, ConfigSource::Managed);
    assert_eq!(loaded.provider_choice, Some(model_provider::EngineProviderChoice::Provider("pid".into())));
}

#[gpui::test]
async fn service_rejects_invalid_provider_writes_without_changing_saved_data(cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_service_validation").await;
    let (broker, _pump) = cx.update(crate::provider_services::spawn_credential_broker);
    let service = model_provider::ProviderService::new(db.clone(), broker);
    let mut draft = sample_draft();
    draft.endpoints[0].base_url = "file:///private".into();
    assert!(matches!(service.create_provider(draft).await,
        Err(model_provider::ServiceError::Invalid(model_provider::ValidationError::BaseUrlScheme))));
    assert!(db.list_providers().await.unwrap().is_empty());

    let saved = service.create_provider(sample_draft()).await.unwrap();
    let mut draft = sample_draft();
    draft.models[0].protocols = vec![Protocol::AnthropicMessages];
    assert!(matches!(service.update_provider(&saved.id, saved.revision, draft).await,
        Err(model_provider::ServiceError::Invalid(model_provider::ValidationError::ModelProtocolMissing(_)))));
    assert_eq!(db.get_provider(&saved.id).await.unwrap(), Some(saved));
}

#[gpui::test]
async fn preset_seed_inserts_disabled_templates_once_in_catalog_order(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_seed_presets").await;
    let inserted = db.seed_presets().await.unwrap();
    assert_eq!(inserted.len(), 3);
    for (sort, (snapshot, preset)) in inserted.iter().zip(model_provider::PROVIDER_PRESETS).enumerate() {
        assert_eq!(snapshot.preset_id.as_deref(), Some(preset.id));
        assert_eq!(snapshot.name, preset.name);
        assert_eq!(snapshot.revision, 1);
        assert_eq!(snapshot.sort, sort as i64);
        assert!(!snapshot.enabled);
        assert_eq!(snapshot.routing, RoutingMode::Direct);
        assert_eq!(snapshot.auth, AuthMode::ApiKey);
        assert_eq!(snapshot.default_protocol, preset.protocol);
        assert_eq!(snapshot.endpoints, vec![EndpointSnapshot {
            protocol: preset.protocol,
            base_url: preset.base_url.to_owned(),
        }]);
        assert!(snapshot.models.is_empty());
        assert!(snapshot.keys.is_empty());
        assert!(snapshot.headers.is_empty());
    }
    assert_eq!(db.list_providers().await.unwrap(), inserted);
    assert!(db.seed_presets().await.unwrap().is_empty());
    assert_eq!(db.list_providers().await.unwrap(), inserted);
    assert!(db.pending_credential_ops().await.unwrap().is_empty());
}

#[gpui::test]
async fn renamed_preset_edits_survive_reseeding(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_preset_edits").await;
    let inserted = db.seed_presets().await.unwrap();
    let preset = &inserted[0];
    let mut draft = sample_draft();
    draft.name = "My renamed Qiniu".to_owned();
    draft.endpoints[0].base_url = "https://my-proxy.example/v1".to_owned();
    draft.sort = 42;
    let updated = db.update_provider(&preset.id, preset.revision, draft).await.unwrap();
    assert_eq!(updated.preset_id.as_deref(), Some("qiniu"));
    let before = db.list_providers().await.unwrap();
    assert!(db.seed_presets().await.unwrap().is_empty());
    assert_eq!(db.list_providers().await.unwrap(), before);
    assert_eq!(db.get_provider(&preset.id).await.unwrap(), Some(updated));
}

#[gpui::test]
async fn preset_seed_skips_user_name_collision_without_adopting_it(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_preset_collision").await;
    let mut draft = sample_draft();
    draft.name = " fEnNo ".to_owned();
    let user_provider = db.insert_provider(draft).await.unwrap();
    assert_eq!(user_provider.preset_id, None);
    let inserted = db.seed_presets().await.unwrap();
    assert_eq!(inserted.iter().map(|provider| provider.preset_id.as_deref()).collect::<Vec<_>>(),
        vec![Some("qiniu"), Some("siliconflow")]);
    assert_eq!(db.get_provider(&user_provider.id).await.unwrap(), Some(user_provider.clone()));
    let before = db.list_providers().await.unwrap();
    assert!(db.seed_presets().await.unwrap().is_empty());
    assert_eq!(db.list_providers().await.unwrap(), before);
    db.delete_provider(&user_provider.id, user_provider.revision).await.unwrap();
    assert!(db.get_provider(&user_provider.id).await.unwrap().is_none());
    let inserted = db.seed_presets().await.unwrap();
    assert_eq!(inserted.len(), 1);
    assert_eq!(inserted[0].preset_id.as_deref(), Some("fenno"));
}

#[gpui::test]
async fn preset_delete_is_forbidden_without_revision_or_credential_changes(_cx: &mut gpui::TestAppContext) {
    let db = fresh_db("provider_store_preset_delete").await;
    let inserted = db.seed_presets().await.unwrap();
    let preset = &inserted[0];
    db.begin_credential_put("preset/key").await.unwrap();
    let with_key = db.commit_key(&preset.id, preset.revision, KeyRecord {
        id: None,
        label: None,
        masked: "***key".to_owned(),
        credential_ref: "preset/key".to_owned(),
        enabled: true,
        sort: 0,
    }).await.unwrap();
    db.begin_credential_put("preset/header").await.unwrap();
    let saved = db.commit_header(&preset.id, with_key.revision, HeaderRecord {
        id: None,
        name: "X-Custom".to_owned(),
        masked: "***header".to_owned(),
        credential_ref: "preset/header".to_owned(),
        enabled: true,
    }).await.unwrap();
    assert!(db.pending_credential_ops().await.unwrap().is_empty());
    for expect in [preset.revision, saved.revision] {
        let error = db.delete_provider(&saved.id, expect).await.unwrap_err();
        assert!(matches!(error, ProviderStoreError::Forbidden(_)), "got {error:?}");
        assert_eq!(db.get_provider(&saved.id).await.unwrap(), Some(saved.clone()));
        assert!(db.pending_credential_ops().await.unwrap().is_empty());
        assert!(db.credential_ref_is_live("preset/key").await.unwrap());
        assert!(db.credential_ref_is_live("preset/header").await.unwrap());
    }
    let user_provider = db.insert_provider(sample_draft()).await.unwrap();
    assert!(db.delete_provider(&user_provider.id, user_provider.revision).await.unwrap().is_empty());
    assert!(db.get_provider(&user_provider.id).await.unwrap().is_none());
}

#[test]
fn preset_migration_preserves_legacy_rows_as_non_presets() {
    use db::sqlez::{connection::Connection, domain::Migrator as _};

    let connection = Connection::open_memory(Some("provider_store_preset_migration"));
    connection.migrate(ProviderDb::NAME, &ProviderDb::MIGRATIONS[..1], &mut |_, _, _| false).unwrap();
    connection.exec(
        "INSERT INTO model_providers (id, name, name_key, enabled, routing_mode, auth_mode, default_protocol, revision, sort, created_at, updated_at) VALUES
         ('legacy', 'Legacy Provider', 'legacy provider', 1, 'direct', 'api_key', 'openai-chat', 7, 9, 10, 20),
         ('legacy-other', 'Other Provider', 'other provider', 0, 'direct', 'api_key', 'openai-chat', 1, 0, 10, 10);
         INSERT INTO model_provider_endpoints (provider_id, protocol, base_url) VALUES ('legacy', 'openai-chat', 'https://legacy.example/v1');
         INSERT INTO model_provider_keys (id, provider_id, label, masked, credential_ref, enabled, sort) VALUES ('legacy-key', 'legacy', NULL, '***old', 'legacy/credential', 1, 0)",
    ).unwrap()().unwrap();

    ProviderDb::migrate(&connection).unwrap();

    let snapshot = super::snapshot::require_snapshot(&connection, "legacy").unwrap();
    assert_eq!(snapshot.preset_id, None);
    assert_eq!(snapshot.name, "Legacy Provider");
    assert!(snapshot.enabled);
    assert_eq!(snapshot.revision, 7);
    assert_eq!(snapshot.sort, 9);
    assert_eq!(snapshot.created_at, 10);
    assert_eq!(snapshot.updated_at, 20);
    assert_eq!(snapshot.endpoints[0].base_url, "https://legacy.example/v1");
    assert_eq!(snapshot.keys[0].credential_ref, "legacy/credential");
    let other = super::snapshot::require_snapshot(&connection, "legacy-other").unwrap();
    assert_eq!(other.preset_id, None);
    assert_eq!(other.name, "Other Provider");
    connection.exec("UPDATE model_providers SET preset_id = 'qiniu' WHERE id = 'legacy'").unwrap()().unwrap();
    assert!(connection.exec("UPDATE model_providers SET preset_id = 'qiniu' WHERE id = 'legacy-other'").unwrap()().is_err());
    assert_eq!(super::snapshot::require_snapshot(&connection, "legacy-other").unwrap(), other);
}
