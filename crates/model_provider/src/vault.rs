//! `InMemoryVault`: real-read-back test backend for `CredentialStore`.
//!
//! The GPUI test platform's `read_credentials` is a no-op stub (`Ok(None)`),
//! which would make broker tests always pass regardless of correctness. Per
//! design §4.2 acceptance tests MUST hit a backend that actually returns the
//! bytes that were written. `InMemoryVault` is that backend.
//!
//! Keep it structurally identical to `GpuiCredentialBroker` from the service
//! layer's point of view: `Clone`, `Send + Sync`, and the same trait
//! impl. Tests can swap one for the other without conditional code.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use crate::{CredentialError, CredentialResult, CredentialStore, Secret};

#[derive(Clone, Default)]
pub struct InMemoryVault {
    entries: Arc<Mutex<HashMap<String, String>>>,
    /// When set, future `put`/`get`/`delete` calls fail with `Rejected`. Used
    /// by tests to assert the service handles a backend outage without
    /// losing the user's draft.
    forced_error: Arc<Mutex<Option<CredentialError>>>,
}

impl InMemoryVault {
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns how many references are currently stored. Useful in tests to
    /// assert "the service deleted everything" without exposing the values.
    pub fn len(&self) -> usize {
        self.entries.lock().expect("vault poisoned").len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Drains the stored references; the plaintext secrets are lost. Only
    /// the key list is returned so tests can diff against the SQL side
    /// without comparing secrets.
    pub fn take_references(&self) -> Vec<String> {
        let mut guard = self.entries.lock().expect("vault poisoned");
        let mut keys: Vec<String> = guard.drain().map(|(k, _)| k).collect();
        keys.sort();
        keys
    }

    /// Install a forced failure. The error is cloned by converting to a
    /// `Rejected` message because `CredentialError` is not `Clone`.
    pub fn fail_next_with(&self, message: impl Into<String>) {
        *self.forced_error.lock().expect("vault poisoned") =
            Some(CredentialError::Rejected(message.into()));
    }

    pub fn clear_forced_error(&self) {
        *self.forced_error.lock().expect("vault poisoned") = None;
    }

    fn consume_forced_error(&self) -> Option<CredentialError> {
        // Consume the forced error so one call to `fail_next_with` affects
        // exactly one request; the next one goes through the real store.
        let mut guard = self.forced_error.lock().expect("vault poisoned");
        match guard.take() {
            Some(CredentialError::Rejected(msg)) => Some(CredentialError::Rejected(msg)),
            Some(CredentialError::Unavailable) => Some(CredentialError::Unavailable),
            Some(CredentialError::Backend(_)) => {
                Some(CredentialError::Backend(anyhow::anyhow!("forced backend failure")))
            }
            None => None,
        }
    }
}

#[async_trait]
impl CredentialStore for InMemoryVault {
    async fn put(&self, reference: &str, value: Secret) -> CredentialResult<()> {
        if let Some(err) = self.consume_forced_error() {
            return Err(err);
        }
        self.entries
            .lock()
            .expect("vault poisoned")
            .insert(reference.to_owned(), value.into_string());
        Ok(())
    }

    async fn get(&self, reference: &str) -> CredentialResult<Option<Secret>> {
        if let Some(err) = self.consume_forced_error() {
            return Err(err);
        }
        Ok(self
            .entries
            .lock()
            .expect("vault poisoned")
            .get(reference)
            .cloned()
            .map(Secret::new))
    }

    async fn delete(&self, reference: &str) -> CredentialResult<()> {
        if let Some(err) = self.consume_forced_error() {
            return Err(err);
        }
        // Idempotent per trait contract.
        self.entries
            .lock()
            .expect("vault poisoned")
            .remove(reference);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn round_trip_put_get_delete() {
        let vault = InMemoryVault::new();
        assert!(
            vault.get("ref/a").await.unwrap().is_none(),
            "missing ref is None, not an error"
        );

        vault.put("ref/a", Secret::new("secret-value")).await.unwrap();
        let got = vault.get("ref/a").await.unwrap().expect("just stored it");
        assert_eq!(got.expose(), "secret-value");

        vault.put("ref/a", Secret::new("rotated")).await.unwrap();
        assert_eq!(vault.get("ref/a").await.unwrap().unwrap().expose(), "rotated");

        vault.delete("ref/a").await.unwrap();
        assert!(vault.get("ref/a").await.unwrap().is_none());
        vault.delete("ref/a").await.unwrap();
    }

    #[tokio::test]
    async fn forced_error_is_consumed_once() {
        let vault = InMemoryVault::new();
        vault.fail_next_with("platform cap exceeded");
        let err = vault
            .put("ref/a", Secret::new("v"))
            .await
            .expect_err("forced error must fire");
        assert!(
            matches!(&err, CredentialError::Rejected(m) if m.contains("cap")),
            "got {err:?}"
        );
        vault.put("ref/a", Secret::new("v")).await.unwrap();
        assert_eq!(vault.get("ref/a").await.unwrap().unwrap().expose(), "v");
    }
}
