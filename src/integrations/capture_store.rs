use crate::domain_models::OwnerId;
use crate::domain_models::capture::*;

/// Implementations must atomically enforce (owner, source) identity and capacity.
pub trait CaptureStore {
    fn save_pending(
        &mut self,
        event: CaptureEvent,
        capacity: usize,
    ) -> Result<CaptureWrite, CaptureError>;
    fn pending(&self, owner: &OwnerId) -> Result<Vec<PendingEvidence>, CaptureError>;
}
/// Offline adapter only: one-process mutable access, no durable persistence.
#[derive(Default)]
pub struct InMemoryCaptureStore {
    events: Vec<CaptureEvent>,
}
impl CaptureStore for InMemoryCaptureStore {
    fn save_pending(
        &mut self,
        event: CaptureEvent,
        capacity: usize,
    ) -> Result<CaptureWrite, CaptureError> {
        if let Some(existing) = self
            .events
            .iter()
            .find(|old| old.owner == event.owner && old.source == event.source)
        {
            return Ok(if existing == &event {
                CaptureWrite::ExistingIdentical
            } else {
                CaptureWrite::Conflict
            });
        }
        if self
            .events
            .iter()
            .filter(|old| old.owner == event.owner)
            .count()
            >= capacity
        {
            return Ok(CaptureWrite::Full);
        }
        self.events.push(event);
        Ok(CaptureWrite::Inserted)
    }
    fn pending(&self, owner: &OwnerId) -> Result<Vec<PendingEvidence>, CaptureError> {
        Ok(self
            .events
            .iter()
            .filter(|event| &event.owner == owner)
            .map(|event| PendingEvidence {
                event: event.clone(),
                provisional: true,
            })
            .collect())
    }
}

/// Async durable capture contract; authenticated scope comes from the service caller.
pub trait AsyncCaptureStore {
    fn save_pending_async(
        &self,
        event: CaptureEvent,
        capacity: usize,
    ) -> impl std::future::Future<Output = Result<CaptureWrite, CaptureError>>;
    fn pending_async(
        &self,
        owner: &OwnerId,
    ) -> impl std::future::Future<Output = Result<Vec<PendingEvidence>, CaptureError>>;
}
