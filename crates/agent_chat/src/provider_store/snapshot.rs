//! Read a `ProviderSnapshot` back from SQL.
//!
//! Both helpers take `&Connection` so they compose inside the single-writer
//! `write(|conn| ...)` closure used by every mutating method.

use anyhow::{Context as _, Result, anyhow};
use db::sqlez::connection::Connection;
use model_provider::{
    EndpointSnapshot, HeaderSummary, KeySummary, ProviderModel, ProviderSnapshot,
};

use super::codec::{auth_from_str, protocol_from_str, routing_from_str};

pub(super) fn fetch_snapshot(conn: &Connection, id: &str) -> Result<Option<ProviderSnapshot>> {
    let row = conn
        .select_row_bound::<&str, (Option<String>, String, i64, String, String, String, i64, i64, i64, i64)>(
            "SELECT preset_id, name, enabled, routing_mode, auth_mode, default_protocol, revision, sort, created_at, updated_at FROM model_providers WHERE id = ?",
        )
        .context("prepare select provider")?(id)
        .context("select provider")?;
    let Some((
        preset_id,
        name,
        enabled,
        routing,
        auth,
        default_protocol,
        revision,
        sort,
        created_at,
        updated_at,
    )) = row
    else {
        return Ok(None);
    };

    let endpoints: Vec<EndpointSnapshot> = conn
        .select_bound::<&str, (String, String)>(
            "SELECT protocol, base_url FROM model_provider_endpoints WHERE provider_id = ? ORDER BY protocol",
        )
        .context("prepare select endpoints")?(id)
        .context("select endpoints")?
        .into_iter()
        .map(|(protocol, base_url)| {
            Ok(EndpointSnapshot {
                protocol: protocol_from_str(&protocol)?,
                base_url,
            })
        })
        .collect::<Result<Vec<_>>>()?;

    let models: Vec<ProviderModel> = conn
        .select_bound::<&str, String>(
            "SELECT definition FROM model_provider_models WHERE provider_id = ? ORDER BY sort, model_id",
        )
        .context("prepare select models")?(id)
        .context("select models")?
        .into_iter()
        .map(|def| serde_json::from_str::<ProviderModel>(&def).context("decode model definition"))
        .collect::<Result<Vec<_>>>()?;

    let keys: Vec<KeySummary> = conn
        .select_bound::<&str, (String, Option<String>, String, String, i64, i64)>(
            "SELECT id, label, masked, credential_ref, enabled, sort FROM model_provider_keys WHERE provider_id = ? ORDER BY sort, id",
        )
        .context("prepare select keys")?(id)
        .context("select keys")?
        .into_iter()
        .map(|(key_id, label, masked, credential_ref, enabled, sort)| KeySummary {
            id: key_id,
            label,
            masked,
            credential_ref,
            enabled: enabled != 0,
            sort,
        })
        .collect();

    let headers: Vec<HeaderSummary> = conn
        .select_bound::<&str, (String, String, String, String, i64)>(
            "SELECT id, name, masked, credential_ref, enabled FROM model_provider_headers WHERE provider_id = ? ORDER BY name_key",
        )
        .context("prepare select headers")?(id)
        .context("select headers")?
        .into_iter()
        .map(|(hid, name, masked, credential_ref, enabled)| HeaderSummary {
            id: hid,
            name,
            masked,
            credential_ref,
            enabled: enabled != 0,
        })
        .collect();

    Ok(Some(ProviderSnapshot {
        id: id.to_owned(),
        preset_id,
        revision: revision as u64,
        name,
        enabled: enabled != 0,
        routing: routing_from_str(&routing)?,
        auth: auth_from_str(&auth)?,
        default_protocol: protocol_from_str(&default_protocol)?,
        endpoints,
        models,
        keys,
        headers,
        sort,
        created_at,
        updated_at,
    }))
}

pub(super) fn require_snapshot(conn: &Connection, id: &str) -> Result<ProviderSnapshot> {
    fetch_snapshot(conn, id)?.ok_or_else(|| anyhow!("provider {id} vanished mid-transaction"))
}
