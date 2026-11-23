//! Key metadata: `commit_key`, `remove_key`, `set_key_enabled`.
//!
//! The credential plaintext is already in the system credential store by the
//! time any of these methods is called; the only thing that varies across the
//! two flows is whether an old `credential_ref` needs to be enqueued for
//! background deletion.

use anyhow::Result;
use model_provider::ProviderSnapshot;

use super::codec::new_id;
use super::credential_ops::{bump_revision, enqueue_pending_delete};
use super::snapshot::require_snapshot;
use super::{KeyRecord, ProviderDb, ProviderStoreError, StoreResult};

impl ProviderDb {
    /// Inserts or replaces a key metadata row. The caller has already written
    /// the credential, resolved its `credential_ref`, and verified read-back.
    /// Enqueues the previous `credential_ref` (if any) for background deletion.
    pub async fn commit_key(
        &self,
        provider_id: &str,
        expect: u64,
        record: KeyRecord,
    ) -> StoreResult<ProviderSnapshot> {
        let provider_id = provider_id.to_owned();
        let outcome = self
            .0
            .write(move |conn| -> Result<std::result::Result<ProviderSnapshot, ProviderStoreError>> {
                conn.with_savepoint("provider_commit_key", || {
                    match bump_revision(conn, &provider_id, expect)? {
                        Err(e) => Ok(Err(e)),
                        Ok(_) => {
                            let (key_id, old_ref) = match &record.id {
                                Some(id) => {
                                    let old: Option<String> = conn.select_row_bound::<(&str, &str), String>(
                                        "SELECT credential_ref FROM model_provider_keys WHERE id = ? AND provider_id = ?",
                                    )?((id.as_str(), provider_id.as_str()))?;
                                    if old.is_none() {
                                        return Ok(Err(ProviderStoreError::NotFound(format!(
                                            "{provider_id}/key/{id}"
                                        ))));
                                    }
                                    (id.clone(), old)
                                }
                                None => (new_id(), None),
                            };
                            conn.exec_bound::<(&str, &str, Option<&str>, &str, &str, i64, i64)>(
                                "INSERT OR REPLACE INTO model_provider_keys (id, provider_id, label, masked, credential_ref, enabled, sort) VALUES (?, ?, ?, ?, ?, ?, ?)",
                            )?((
                                key_id.as_str(),
                                provider_id.as_str(),
                                record.label.as_deref(),
                                record.masked.as_str(),
                                record.credential_ref.as_str(),
                                record.enabled as i64,
                                record.sort,
                            ))?;
                            // The committed ref is now live; drop it from
                            // pending_put. The old ref (if different) joins
                            // pending_delete so the service deletes it after.
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

    /// Removes a key metadata row and enqueues its credential reference for
    /// background deletion.
    pub async fn remove_key(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
    ) -> StoreResult<ProviderSnapshot> {
        let provider_id = provider_id.to_owned();
        let key_id = key_id.to_owned();
        let outcome = self
            .0
            .write(move |conn| -> Result<std::result::Result<ProviderSnapshot, ProviderStoreError>> {
                conn.with_savepoint("provider_remove_key", || {
                    match bump_revision(conn, &provider_id, expect)? {
                        Err(e) => Ok(Err(e)),
                        Ok(_) => {
                            let old_ref: Option<String> = conn.select_row_bound::<(&str, &str), String>(
                                "SELECT credential_ref FROM model_provider_keys WHERE id = ? AND provider_id = ?",
                            )?((key_id.as_str(), provider_id.as_str()))?;
                            let Some(old_ref) = old_ref else {
                                return Ok(Err(ProviderStoreError::NotFound(format!(
                                    "{provider_id}/key/{key_id}"
                                ))));
                            };
                            conn.exec_bound::<(&str, &str)>(
                                "DELETE FROM model_provider_keys WHERE id = ? AND provider_id = ?",
                            )?((key_id.as_str(), provider_id.as_str()))?;
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

    pub async fn set_key_enabled(
        &self,
        provider_id: &str,
        expect: u64,
        key_id: &str,
        enabled: bool,
    ) -> StoreResult<ProviderSnapshot> {
        let provider_id = provider_id.to_owned();
        let key_id = key_id.to_owned();
        let outcome = self
            .0
            .write(move |conn| -> Result<std::result::Result<ProviderSnapshot, ProviderStoreError>> {
                conn.with_savepoint("provider_toggle_key", || {
                    match bump_revision(conn, &provider_id, expect)? {
                        Err(e) => Ok(Err(e)),
                        Ok(_) => {
                            let mut stmt = conn.exec_bound::<(i64, &str, &str)>(
                                "UPDATE model_provider_keys SET enabled = ? WHERE id = ? AND provider_id = ?",
                            )?;
                            stmt((enabled as i64, key_id.as_str(), provider_id.as_str()))?;
                            let changes: i64 = conn
                                .select_row::<i64>("SELECT changes()")?()?
                                .unwrap_or(0);
                            if changes == 0 {
                                return Ok(Err(ProviderStoreError::NotFound(format!(
                                    "{provider_id}/key/{key_id}"
                                ))));
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
}
