//! `agent_launch_preferences`: per-backend load / save with its own revision.

use anyhow::{Context as _, Result, anyhow};
use model_provider::{AgentPreferences, BackendTarget};

use super::codec::backend_to_str;
use super::{ProviderDb, ProviderStoreError, StoreResult};

impl ProviderDb {
    /// Loads the stored preferences for a backend. Missing row yields
    /// `AgentPreferences::native_default(target)` with `revision = 0` so the
    /// caller can distinguish "never saved" from a real empty record.
    pub async fn load_preferences(
        &self,
        target: BackendTarget,
    ) -> StoreResult<AgentPreferences> {
        let r = self
            .0
            .write(move |conn| -> Result<AgentPreferences> {
                let row: Option<(String, i64)> = conn
                    .select_row_bound::<&str, (String, i64)>(
                        "SELECT definition, revision FROM agent_launch_preferences WHERE backend = ?",
                    )?(backend_to_str(target))?;
                match row {
                    None => Ok(AgentPreferences::native_default(target)),
                    Some((definition, revision)) => {
                        let mut parsed: AgentPreferences = serde_json::from_str(&definition)
                            .context("decode agent preferences")?;
                        // Row revision wins; JSON `revision` is only a hint
                        // kept around for human diffing.
                        parsed.revision = revision as u64;
                        parsed.target = target;
                        Ok(parsed)
                    }
                }
            })
            .await;
        Ok(r?)
    }

    /// Saves preferences under optimistic concurrency. `expect == 0` requires
    /// the row to not exist (creation); any other value requires the row's
    /// stored revision to match exactly.
    pub async fn save_preferences(
        &self,
        target: BackendTarget,
        expect: u64,
        value: AgentPreferences,
    ) -> StoreResult<AgentPreferences> {
        if value.target != target {
            return Err(ProviderStoreError::Io(anyhow!(
                "preferences target mismatch: row {} vs value {}",
                backend_to_str(target),
                backend_to_str(value.target),
            )));
        }
        let outcome = self
            .0
            .write(move |conn| -> Result<std::result::Result<AgentPreferences, ProviderStoreError>> {
                conn.with_savepoint("preferences_save", || {
                    let backend = backend_to_str(target);
                    let next_revision = expect + 1;
                    let mut persisted = value.clone();
                    persisted.revision = next_revision;
                    let definition =
                        serde_json::to_string(&persisted).context("encode agent preferences")?;
                    if expect == 0 {
                        let exists: Option<i64> = conn
                            .select_row_bound::<&str, i64>(
                                "SELECT revision FROM agent_launch_preferences WHERE backend = ?",
                            )?(backend)?;
                        if exists.is_some() {
                            return Ok(Err(ProviderStoreError::Conflict(backend.to_owned(), 0)));
                        }
                        let mut stmt = conn.exec_bound::<(&str, &str, i64)>(
                            "INSERT INTO agent_launch_preferences (backend, definition, revision) VALUES (?, ?, ?)",
                        )?;
                        stmt((backend, definition.as_str(), next_revision as i64))?;
                    } else {
                        let mut stmt = conn.exec_bound::<(&str, i64, &str, i64)>(
                            "UPDATE agent_launch_preferences SET definition = ?, revision = ? WHERE backend = ? AND revision = ?",
                        )?;
                        stmt((
                            definition.as_str(),
                            next_revision as i64,
                            backend,
                            expect as i64,
                        ))?;
                        let changes: i64 =
                            conn.select_row::<i64>("SELECT changes()")?()?.unwrap_or(0);
                        if changes == 0 {
                            let actual: Option<i64> = conn
                                .select_row_bound::<&str, i64>(
                                    "SELECT revision FROM agent_launch_preferences WHERE backend = ?",
                                )?(backend)?;
                            return Ok(Err(match actual {
                                None => ProviderStoreError::NotFound(backend.to_owned()),
                                Some(_) => {
                                    ProviderStoreError::Conflict(backend.to_owned(), expect)
                                }
                            }));
                        }
                    }
                    Ok(Ok(persisted))
                })
            })
            .await;
        match outcome? {
            Ok(v) => Ok(v),
            Err(e) => Err(e),
        }
    }
}
