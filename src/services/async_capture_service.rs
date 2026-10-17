use crate::domain_models::{OwnerId, capture::*};
use crate::engines::capture_policy;
use crate::integrations::capture_store::AsyncCaptureStore;
pub struct AsyncCaptureService<S> {
    store: S,
    limits: CaptureLimits,
}
impl<S: AsyncCaptureStore> AsyncCaptureService<S> {
    pub fn new(store: S, limits: CaptureLimits) -> Self {
        Self { store, limits }
    }
    pub async fn capture(&self, event: CaptureEvent) -> Result<CaptureReceipt, CaptureError> {
        capture_policy::validate(&event, &self.limits)?;
        let source = event.source.clone();
        let status = match self
            .store
            .save_pending_async(event, self.limits.max_pending_per_owner)
            .await?
        {
            CaptureWrite::Inserted => CaptureStatus::SavedPending,
            CaptureWrite::ExistingIdentical => CaptureStatus::AlreadySavedPending,
            CaptureWrite::Conflict => return Err(CaptureError::SourceConflict),
            CaptureWrite::Full => return Err(CaptureError::BacklogFull),
        };
        Ok(CaptureReceipt { source, status })
    }
    pub async fn pending(&self, owner: &OwnerId) -> Result<Vec<PendingEvidence>, CaptureError> {
        self.store.pending_async(owner).await
    }
}
