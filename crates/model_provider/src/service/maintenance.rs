//! Post-mutation cleanup helpers.

use crate::CredentialOpState;

use super::{ProviderRepository, ProviderService};

impl<R, C> ProviderService<R, C>
where
    R: ProviderRepository,
    C: crate::CredentialStore,
{
    /// Best-effort drain of `pending_delete` after remove/delete. Failures
    /// remain logged for the next startup reconcile.
    pub(super) async fn purge_pending_deletes(&self) {
        let ops = match self.repo().pending_credential_ops().await {
            Ok(ops) => ops,
            Err(_) => return,
        };
        for op in ops
            .into_iter()
            .filter(|o| matches!(o.state, CredentialOpState::PendingDelete))
        {
            if self
                .repo()
                .credential_ref_is_live(&op.credential_ref)
                .await
                .unwrap_or(true)
            {
                continue;
            }
            if self.credentials().delete(&op.credential_ref).await.is_ok() {
                let _ = self.repo().finish_credential_cleanup(&op.credential_ref).await;
            }
        }
    }
}

impl<R, C> ProviderService<R, C>
where
    R: ProviderRepository,
    C: crate::CredentialStore,
{
    pub(super) async fn write_and_verify(
        &self,
        reference: &str,
        secret: crate::Secret,
    ) -> super::ServiceResult<()> {
        self.repo().begin_credential_put(reference).await?;
        let plaintext = secret.expose().to_owned();
        self.credentials()
            .put(reference, crate::Secret::new(plaintext.clone()))
            .await
            .map_err(super::ServiceError::from_credential)?;
        match self.credentials().get(reference).await {
            Ok(Some(round_trip)) if round_trip.expose() == plaintext => Ok(()),
            Ok(Some(_)) => {
                let _ = self.credentials().delete(reference).await;
                Err(super::ServiceError::CredentialRejected(
                    "credential store round-trip mismatch".to_owned(),
                ))
            }
            Ok(None) => {
                let _ = self.credentials().delete(reference).await;
                Err(super::ServiceError::CredentialRejected(
                    "credential store did not retain the value".to_owned(),
                ))
            }
            Err(e) => {
                let _ = self.credentials().delete(reference).await;
                Err(super::ServiceError::from_credential(e))
            }
        }
    }
}
