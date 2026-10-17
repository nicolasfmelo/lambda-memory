use crate::domain_models::{JobId, OwnerId, consolidation::*};
use crate::engines::consolidation_policy;
use crate::integrations::consolidation_store::AsyncConsolidationStore;
pub struct AsyncConsolidationService<S> {
    store: S,
}
impl<S: AsyncConsolidationStore> AsyncConsolidationService<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
    pub async fn create(&self, snapshot: JobSnapshot) -> Result<JobReceipt, ConsolidationError> {
        consolidation_policy::validate_snapshot(&snapshot)?;
        self.store.create_async(snapshot).await
    }
    pub async fn get(
        &self,
        owner: &OwnerId,
        id: &JobId,
    ) -> Result<ConsolidationJob, ConsolidationError> {
        self.store.get_async(owner, id).await
    }
    pub async fn complete(
        &self,
        owner: &OwnerId,
        id: &JobId,
        revisions: Vec<Revision>,
    ) -> Result<JobReceipt, ConsolidationError> {
        let job = self.store.get_async(owner, id).await?;
        if revisions.is_empty() || revisions.len() > 16 {
            return Err(ConsolidationError::InvalidRevision);
        }
        for (i, revision) in revisions.iter().enumerate() {
            consolidation_policy::validate_revision(&job.snapshot, revision)?;
            if revisions[..i]
                .iter()
                .any(|old| old.id == revision.id || old.memory == revision.memory)
            {
                return Err(ConsolidationError::InvalidRevision);
            }
        }
        self.store.complete_async(owner, id, revisions).await
    }
}
