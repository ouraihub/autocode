//! Startup reconciliation of pending credential operations.

use crate::{CredentialError, CredentialOpState};

use super::{ProviderRepository, ProviderService, ReconcileReport, ServiceResult};

impl<R, C> ProviderService<R, C>
where
    R: ProviderRepository,
    C: crate::CredentialStore,
{
    /// Startup reconcile per §5.3 paragraph 5-6. Idempotent: safe to call at
    /// every app boot; returns the number of entries drained plus the number
    /// left behind (still referenced or failed to delete).
    pub async fn reconcile_pending(&self) -> ServiceResult<ReconcileReport> {
        let ops = self.repo().pending_credential_ops().await?;
        let mut cleaned = 0u32;
        let mut retained = 0u32;
        for op in ops {
            match op.state {
                CredentialOpState::PendingPut => {
                    if self.repo().credential_ref_is_live(&op.credential_ref).await? {
                        retained += 1;
                        continue;
                    }
                    match self.credentials().delete(&op.credential_ref).await {
                        Ok(()) => {
                            self.repo()
                                .finish_credential_cleanup(&op.credential_ref)
                                .await?;
                            cleaned += 1;
                        }
                        Err(CredentialError::Unavailable) => retained += 1,
                        Err(_) => retained += 1,
                    }
                }
                CredentialOpState::PendingDelete => {
                    if self.repo().credential_ref_is_live(&op.credential_ref).await? {
                        self.repo()
                            .finish_credential_cleanup(&op.credential_ref)
                            .await?;
                        retained += 1;
                        continue;
                    }
                    match self.credentials().delete(&op.credential_ref).await {
                        Ok(()) => {
                            self.repo()
                                .finish_credential_cleanup(&op.credential_ref)
                                .await?;
                            cleaned += 1;
                        }
                        Err(CredentialError::Unavailable) => retained += 1,
                        Err(_) => retained += 1,
                    }
                }
            }
        }
        Ok(ReconcileReport { cleaned, retained })
    }
}
