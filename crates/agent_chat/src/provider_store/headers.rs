//! Header metadata: `commit_header`, `remove_header`.

use anyhow::Result;
use model_provider::ProviderSnapshot;

use super::codec::{header_name_key, new_id};
use super::credential_ops::{bump_revision, enqueue_pending_delete};
use super::snapshot::require_snapshot;
use super::{HeaderRecord, ProviderDb, ProviderStoreError, StoreResult};

impl ProviderDb {
    /// Inserts or replaces a header metadata row. Same contract as
    /// `commit_key`: caller already placed the value in the credential store.
    pub async fn commit_header(
        &self,
        provider_id: &str,
        expect: u64,
        record: HeaderRecord,
    ) -> StoreResult<ProviderSnapshot> {
        let provider_id = provider_id.to_owned();
        let outcome = self
            .0
            .write(move |conn| -> Result<std::result::Result<ProviderSnapshot, ProviderStoreError>> {
                conn.with_savepoint("provider_commit_header", || {
                    match bump_revision(conn, &provider_id, expect)? {
                        Err(e) => Ok(Err(e)),
                        Ok(_) => {
                            let (header_id, old_ref) = match &record.id {
                                Some(id) => {
                                    let old: Option<String> = conn.select_row_bound::<(&str, &str), String>(
                                        "SELECT credential_ref FROM model_provider_headers WHERE id = ? AND provider_id = ?",
                                    )?((id.as_str(), provider_id.as_str()))?;
                                    if old.is_none() {
                                        return Ok(Err(ProviderStoreError::NotFound(format!(
                                            "{provider_id}/header/{id}"
                                        ))));
                                    }
                                    (id.clone(), old)
                                }
                                None => (new_id(), None),
                            };
                            let name_key = header_name_key(&record.name);
                            conn.exec_bound::<(&str, &str, &str, &str, &str, &str, i64)>(
                                "INSERT OR REPLACE INTO model_provider_headers (id, provider_id, name, name_key, masked, credential_ref, enabled) VALUES (?, ?, ?, ?, ?, ?, ?)",
                            )?((
                                header_id.as_str(),
                                provider_id.as_str(),
                                record.name.as_str(),
                                name_key.as_str(),
                                record.masked.as_str(),
                                record.credential_ref.as_str(),
                                record.enabled as i64,
                            ))?;
                            conn.exec_bound::<&str>(
                                "DELETE FROM model_credential_ops WHERE credential_ref = ?",
                            )?(record.credential_ref.as_str())?;
                            if let Some(old_ref) = old_ref
                                && old_ref != record.credential_ref
                            {
                                enqueue_pending_delete(conn, std::slice::from_ref(&old_ref))?;
                            }
                            Ok(Ok(require_snapshot(conn, &provider_id)?))
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

    pub async fn remove_header(
        &self,
        provider_id: &str,
        expect: u64,
        header_id: &str,
    ) -> StoreResult<ProviderSnapshot> {
        let provider_id = provider_id.to_owned();
        let header_id = header_id.to_owned();
        let outcome = self
            .0
            .write(move |conn| -> Result<std::result::Result<ProviderSnapshot, ProviderStoreError>> {
                conn.with_savepoint("provider_remove_header", || {
                    match bump_revision(conn, &provider_id, expect)? {
                        Err(e) => Ok(Err(e)),
                        Ok(_) => {
                            let old_ref: Option<String> = conn.select_row_bound::<(&str, &str), String>(
                                "SELECT credential_ref FROM model_provider_headers WHERE id = ? AND provider_id = ?",
                            )?((header_id.as_str(), provider_id.as_str()))?;
                            let Some(old_ref) = old_ref else {
                                return Ok(Err(ProviderStoreError::NotFound(format!(
                                    "{provider_id}/header/{header_id}"
                                ))));
                            };
                            conn.exec_bound::<(&str, &str)>(
                                "DELETE FROM model_provider_headers WHERE id = ? AND provider_id = ?",
                            )?((header_id.as_str(), provider_id.as_str()))?;
                            enqueue_pending_delete(conn, std::slice::from_ref(&old_ref))?;
                            Ok(Ok(require_snapshot(conn, &provider_id)?))
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
}
