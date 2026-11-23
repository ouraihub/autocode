//! Provider-level CRUD: insert, list, get, update, delete.

use anyhow::Result;
use db::sqlez::connection::Connection;
use model_provider::{EndpointSnapshot, ProviderModel, ProviderSnapshot, PROVIDER_PRESETS};

use super::codec::{
    auth_to_str, name_key, new_id, now_ts, protocol_to_str, routing_to_str,
};
use super::credential_ops::{bump_revision, enqueue_pending_delete};
use super::snapshot::{fetch_snapshot, require_snapshot};
use super::{ProviderDb, ProviderDraft, ProviderStoreError, StoreResult};

pub(super) fn insert_endpoints(
    conn: &Connection,
    provider_id: &str,
    endpoints: &[EndpointSnapshot],
) -> Result<()> {
    let mut stmt = conn.exec_bound::<(&str, &str, &str)>(
        "INSERT INTO model_provider_endpoints (provider_id, protocol, base_url) VALUES (?, ?, ?)",
    )?;
    for endpoint in endpoints {
        stmt((
            provider_id,
            protocol_to_str(endpoint.protocol),
            endpoint.base_url.as_str(),
        ))?;
    }
    Ok(())
}

pub(super) fn insert_models(
    conn: &Connection,
    provider_id: &str,
    models: &[ProviderModel],
) -> Result<()> {
    let mut stmt = conn.exec_bound::<(&str, &str, &str, i64)>(
        "INSERT INTO model_provider_models (provider_id, model_id, definition, sort) VALUES (?, ?, ?, ?)",
    )?;
    for (sort, model) in models.iter().enumerate() {
        let def = serde_json::to_string(model)?;
        stmt((
            provider_id,
            model.model_id.as_str(),
            def.as_str(),
            sort as i64,
        ))?;
    }
    Ok(())
}

impl ProviderDb {
    /// Inserts missing disabled templates in catalog order and returns only
    /// newly inserted snapshots. Existing preset identities or normalized
    /// names are skipped without changing any saved fields or revisions.
    pub async fn seed_presets(&self) -> StoreResult<Vec<ProviderSnapshot>> {
        let result = self
            .0
            .write(|conn| -> Result<Vec<ProviderSnapshot>> {
                conn.with_savepoint("provider_seed_presets", || {
                    let mut inserted = Vec::new();
                    let now = now_ts();
                    for (sort, preset) in PROVIDER_PRESETS.iter().enumerate() {
                        let name_key = name_key(preset.name);
                        let exists = conn.select_row_bound::<(&str, &str), String>(
                            "SELECT id FROM model_providers WHERE preset_id = ? OR name_key = ? LIMIT 1",
                        )?((preset.id, name_key.as_str()))?;
                        if exists.is_some() {
                            continue;
                        }
                        let id = new_id();
                        conn.exec_bound::<(&str, &str, &str, &str, &str, i64, i64, i64)>(
                            "INSERT INTO model_providers (id, preset_id, name, name_key, enabled, routing_mode, auth_mode, default_protocol, revision, sort, created_at, updated_at) \
                             VALUES (?, ?, ?, ?, 0, 'direct', 'api_key', ?, 1, ?, ?, ?)",
                        )?((
                            id.as_str(),
                            preset.id,
                            preset.name,
                            name_key.as_str(),
                            protocol_to_str(preset.protocol),
                            sort as i64,
                            now,
                            now,
                        ))?;
                        insert_endpoints(conn, &id, &[EndpointSnapshot {
                            protocol: preset.protocol,
                            base_url: preset.base_url.to_owned(),
                        }])?;
                        inserted.push(require_snapshot(conn, &id)?);
                    }
                    Ok(inserted)
                })
            })
            .await;
        Ok(result?)
    }

    /// Lists every provider, oldest first by `sort` then `id`.
    pub async fn list_providers(&self) -> StoreResult<Vec<ProviderSnapshot>> {
        let result = self
            .0
            .write(|conn| -> Result<Vec<ProviderSnapshot>> {
                let ids: Vec<String> = conn
                    .select::<String>("SELECT id FROM model_providers ORDER BY sort, id")?()?;
                let mut out = Vec::with_capacity(ids.len());
                for id in ids {
                    if let Some(snap) = fetch_snapshot(conn, &id)? {
                        out.push(snap);
                    }
                }
                Ok(out)
            })
            .await;
        Ok(result?)
    }

    /// Loads one provider; `Ok(None)` when it has been deleted.
    pub async fn get_provider(&self, id: &str) -> StoreResult<Option<ProviderSnapshot>> {
        let id = id.to_owned();
        let result = self.0.write(move |conn| fetch_snapshot(conn, &id)).await;
        Ok(result?)
    }

    /// Inserts a new provider with revision = 1. Name uniqueness is enforced
    /// by the `name_key` UNIQUE constraint.
    pub async fn insert_provider(
        &self,
        draft: ProviderDraft,
    ) -> StoreResult<ProviderSnapshot> {
        let id = new_id();
        let id_for_closure = id.clone();
        let now = now_ts();
        let result = self
            .0
            .write(move |conn| -> Result<ProviderSnapshot> {
                conn.with_savepoint("provider_insert", || {
                    let name_key = name_key(&draft.name);
                    conn.exec_bound::<(&str, &str, &str, i64, &str, &str, &str, i64, i64, i64)>(
                        "INSERT INTO model_providers (id, name, name_key, enabled, routing_mode, auth_mode, default_protocol, revision, sort, created_at, updated_at)\
                         VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?)",
                    )?((
                        id_for_closure.as_str(),
                        draft.name.as_str(),
                        name_key.as_str(),
                        draft.enabled as i64,
                        routing_to_str(draft.routing),
                        auth_to_str(draft.auth),
                        protocol_to_str(draft.default_protocol),
                        draft.sort,
                        now,
                        now,
                    ))?;

                    insert_endpoints(conn, &id_for_closure, &draft.endpoints)?;
                    insert_models(conn, &id_for_closure, &draft.models)?;

                    require_snapshot(conn, &id_for_closure)
                })
            })
            .await;
        Ok(result?)
    }

    /// Replaces a provider's endpoints and model list inside a single
    /// revision-checked transaction. Keys and headers are managed through
    /// `commit_key` / `commit_header`; they are untouched here.
    pub async fn update_provider(
        &self,
        id: &str,
        expect: u64,
        draft: ProviderDraft,
    ) -> StoreResult<ProviderSnapshot> {
        let id = id.to_owned();
        let outcome = self
            .0
            .write(move |conn| -> Result<std::result::Result<ProviderSnapshot, ProviderStoreError>> {
                conn.with_savepoint("provider_update", || {
                    match bump_revision(conn, &id, expect)? {
                        Err(e) => Ok(Err(e)),
                        Ok(_) => {
                            let name_key = name_key(&draft.name);
                            conn.exec_bound::<(&str, &str, i64, &str, &str, &str, i64, &str)>(
                                "UPDATE model_providers SET name = ?, name_key = ?, enabled = ?, routing_mode = ?, auth_mode = ?, default_protocol = ?, sort = ? WHERE id = ?",
                            )?((
                                draft.name.as_str(),
                                name_key.as_str(),
                                draft.enabled as i64,
                                routing_to_str(draft.routing),
                                auth_to_str(draft.auth),
                                protocol_to_str(draft.default_protocol),
                                draft.sort,
                                id.as_str(),
                            ))?;
                            conn.exec_bound::<&str>(
                                "DELETE FROM model_provider_endpoints WHERE provider_id = ?",
                            )?(id.as_str())?;
                            conn.exec_bound::<&str>(
                                "DELETE FROM model_provider_models WHERE provider_id = ?",
                            )?(id.as_str())?;
                            insert_endpoints(conn, &id, &draft.endpoints)?;
                            insert_models(conn, &id, &draft.models)?;
                            Ok(Ok(require_snapshot(conn, &id)?))
                        }
                    }
                })
            })
            .await;
        match outcome? {
            Ok(snap) => Ok(snap),
            Err(e) => Err(e),
        }
    }

    /// Deletes a provider; its endpoints, models, keys, and headers are
    /// cascade-deleted. Returns the credential references (from keys and
    /// headers) that were enqueued to `model_credential_ops` as
    /// `pending_delete` so the caller can purge the credential store.
    pub async fn delete_provider(&self, id: &str, expect: u64) -> StoreResult<Vec<String>> {
        let id = id.to_owned();
        let outcome = self
            .0
            .write(move |conn| -> Result<std::result::Result<Vec<String>, ProviderStoreError>> {
                conn.with_savepoint("provider_delete", || {
                    let preset_id = conn.select_row_bound::<&str, Option<String>>(
                        "SELECT preset_id FROM model_providers WHERE id = ?",
                    )?(id.as_str())?;
                    if matches!(preset_id, Some(Some(_))) {
                        return Ok(Err(ProviderStoreError::Forbidden(id.clone())));
                    }
                    match bump_revision(conn, &id, expect)? {
                        Err(e) => Ok(Err(e)),
                        Ok(_) => {
                            let mut refs: Vec<String> = conn.select_bound::<&str, String>(
                                "SELECT credential_ref FROM model_provider_keys WHERE provider_id = ?",
                            )?(id.as_str())?;
                            refs.extend(conn.select_bound::<&str, String>(
                                "SELECT credential_ref FROM model_provider_headers WHERE provider_id = ?",
                            )?(id.as_str())?);
                            enqueue_pending_delete(conn, &refs)?;
                            conn.exec_bound::<&str>("DELETE FROM model_providers WHERE id = ?")?(
                                id.as_str(),
                            )?;
                            Ok(Ok(refs))
                        }
                    }
                })
            })
            .await;
        match outcome? {
            Ok(refs) => Ok(refs),
            Err(e) => Err(e),
        }
    }
}
