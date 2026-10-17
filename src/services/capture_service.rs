use crate::domain_models::OwnerId;
use crate::domain_models::capture::*;
use crate::engines::capture_policy;
use crate::integrations::capture_store::CaptureStore;

pub struct CaptureService<S> {
    store: S,
    limits: CaptureLimits,
}
impl<S: CaptureStore> CaptureService<S> {
    pub fn new(store: S, limits: CaptureLimits) -> Self {
        Self { store, limits }
    }
    /// Input must already be authorized, sanitized and classified as relevant.
    pub fn capture(&mut self, event: CaptureEvent) -> Result<CaptureReceipt, CaptureError> {
        capture_policy::validate(&event, &self.limits)?;
        let source = event.source.clone();
        let status = match self
            .store
            .save_pending(event, self.limits.max_pending_per_owner)?
        {
            CaptureWrite::Inserted => CaptureStatus::SavedPending,
            CaptureWrite::ExistingIdentical => CaptureStatus::AlreadySavedPending,
            CaptureWrite::Conflict => return Err(CaptureError::SourceConflict),
            CaptureWrite::Full => return Err(CaptureError::BacklogFull),
        };
        Ok(CaptureReceipt { source, status })
    }
    /// Owner must come from authoritative caller context, never a model plan.
    pub fn pending(&self, owner: &OwnerId) -> Result<Vec<PendingEvidence>, CaptureError> {
        self.store.pending(owner)
    }
}
