//! `model_credential_ops` table plus the `bump_revision` helper.
//!
//! These primitives are the glue between the credential broker and the
//! SQL-only metadata CRUD. Every mutating operation in a sibling module calls
//! `bump_revision` first, then (where relevant) `enqueue_pending_delete` for
//! retired credential references.

use anyhow::Result;
use db::sqlez::connection::Connection;
use model_provider::CredentialOperation;

use super::codec::{now_ts, op_state_from_str};
use super::{ProviderDb, ProviderStoreError, StoreResult};

/// Runs `UPDATE model_providers SET revision=revision+1 WHERE id=? AND revision=?`,
/// classifying `0 affected rows` as either `NotFound` (row gone) or
/// `Conflict` (revision moved under us). Must be called inside the enclosing
/// write-transaction before any dependent write.
pub(super) fn bump_revision(
    conn: &Connection,
    id: &str,
    expect: u64,
) -> Result<std::result::Result<(), ProviderStoreError>> {
    let now = now_ts();
    let mut stmt = conn.exec_bound::<(i64, &str, i64)>(
        "UPDATE model_providers SET revision = revision + 1, updated_at = ? WHERE id = ? AND revision = ?",
    )?;
    stmt((now, id, expect as i64))?;
    let changes: i64 = conn
        .select_row::<i64>("SELECT changes()")?()?
        .unwrap_or(0);
    if changes == 1 {
        return Ok(Ok(()));
    }
    let actual: Option<i64> = conn
        .select_row_bound::<&str, i64>("SELECT revision FROM model_providers WHERE id = ?")?(id)?;
    match actual {
        None => Ok(Err(ProviderStoreError::NotFound(id.to_owned()))),
        Some(_) => Ok(Err(ProviderStoreError::Conflict(id.to_owned(), expect))),
    }
}

pub(super) fn enqueue_pending_delete(conn: &Connection, refs: &[String]) -> Result<()> {
    if refs.is_empty() {
        return Ok(());
    }
    let mut stmt = conn.exec_bound::<(&str, &str, i64)>(
        "INSERT OR REPLACE INTO model_credential_ops (credential_ref, state, created_at) VALUES (?, ?, ?)",
    )?;
    let now = now_ts();
    for r in refs {
        stmt((r.as_str(), "pending_delete", now))?;
    }
    Ok(())
}

impl ProviderDb {
    /// Log a credential reference as `pending_put` prior to writing the secret
    /// to the credential store. On a crash, the reconciler can see the
    /// reference, check whether anything in `model_provider_keys` /
    /// `model_provider_headers` adopted it, and delete the orphan.
    pub async fn begin_credential_put(&self, reference: &str) -> StoreResult<()> {
        let reference = reference.to_owned();
        let r = self
            .0
            .write(move |conn| -> Result<()> {
                let mut stmt = conn.exec_bound::<(&str, &str, i64)>(
                    "INSERT OR REPLACE INTO model_credential_ops (credential_ref, state, created_at) VALUES (?, ?, ?)",
                )?;
                stmt((reference.as_str(), "pending_put", now_ts()))?;
                Ok(())
            })
            .await;
        Ok(r?)
    }

    pub async fn pending_credential_ops(&self) -> StoreResult<Vec<CredentialOperation>> {
        let r = self
            .0
            .write(|conn| -> Result<Vec<CredentialOperation>> {
                let rows: Vec<(String, String, i64)> = conn
                    .select::<(String, String, i64)>(
                        "SELECT credential_ref, state, created_at FROM model_credential_ops ORDER BY created_at",
                    )?()?;
                rows.into_iter()
                    .map(|(credential_ref, state, created_at)| {
                        Ok(CredentialOperation {
                            credential_ref,
                            state: op_state_from_str(&state)?,
                            created_at,
                        })
                    })
                    .collect::<Result<Vec<_>>>()
            })
            .await;
        Ok(r?)
    }

    pub async fn finish_credential_cleanup(&self, reference: &str) -> StoreResult<()> {
        let reference = reference.to_owned();
        let r = self
            .0
            .write(move |conn| -> Result<()> {
                let mut stmt = conn.exec_bound::<&str>(
                    "DELETE FROM model_credential_ops WHERE credential_ref = ?",
                )?;
                stmt(reference.as_str())?;
                Ok(())
            })
            .await;
        Ok(r?)
    }
}

impl ProviderDb {
    /// Returns true when `reference` is still referenced by a live Key or
    /// Header metadata row. Used by the reconciler so we never delete a
    /// credential a current provider still needs.
    pub async fn credential_ref_is_live(&self, reference: &str) -> StoreResult<bool> {
        let reference = reference.to_owned();
        let r = self
            .0
            .write(move |conn| -> Result<bool> {
                let count: Option<i64> = conn.select_row_bound::<&str, i64>(
                    "SELECT (SELECT COUNT(*) FROM model_provider_keys WHERE credential_ref = ?1) + (SELECT COUNT(*) FROM model_provider_headers WHERE credential_ref = ?1)",
                )?(reference.as_str())?;
                Ok(count.unwrap_or(0) > 0)
            })
            .await;
        Ok(r?)
    }
}
