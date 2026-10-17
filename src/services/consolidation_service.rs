use crate::domain_models::consolidation::*;
use crate::domain_models::{JobId, OwnerId};
use crate::engines::consolidation_policy;
use crate::integrations::consolidation_store::ConsolidationStore;

pub struct ConsolidationService<S> {
    store: S,
}
impl<S: ConsolidationStore> ConsolidationService<S> {
    pub fn new(store: S) -> Self {
        Self { store }
    }
    /// Snapshot must be assembled from authoritative, authorized capture storage.
    pub fn create(&mut self, snapshot: JobSnapshot) -> Result<JobReceipt, ConsolidationError> {
        consolidation_policy::validate_snapshot(&snapshot)?;
        self.store.create(snapshot)
    }
    pub fn get(&self, owner: &OwnerId, id: &JobId) -> Result<ConsolidationJob, ConsolidationError> {
        self.store.get(owner, id)
    }
    /// Supplied derivation remains pending review; this does not activate recall.
    pub fn complete(
        &mut self,
        owner: &OwnerId,
        id: &JobId,
        revisions: Vec<Revision>,
    ) -> Result<JobReceipt, ConsolidationError> {
        let job = self.store.get(owner, id)?;
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
        self.store.complete(owner, id, revisions)
    }
}
