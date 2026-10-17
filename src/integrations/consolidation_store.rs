use crate::domain_models::consolidation::*;
use crate::domain_models::{JobId, OwnerId};

/// Durable implementations must create and complete each job transactionally.
pub trait ConsolidationStore {
    fn create(&mut self, snapshot: JobSnapshot) -> Result<JobReceipt, ConsolidationError>;
    fn get(&self, owner: &OwnerId, id: &JobId) -> Result<ConsolidationJob, ConsolidationError>;
    fn complete(
        &mut self,
        owner: &OwnerId,
        id: &JobId,
        revisions: Vec<Revision>,
    ) -> Result<JobReceipt, ConsolidationError>;
}
#[derive(Default)]
pub struct InMemoryConsolidationStore {
    jobs: Vec<ConsolidationJob>,
}
impl ConsolidationStore for InMemoryConsolidationStore {
    fn create(&mut self, snapshot: JobSnapshot) -> Result<JobReceipt, ConsolidationError> {
        if let Some(job) = self
            .jobs
            .iter()
            .find(|job| job.snapshot.owner == snapshot.owner && job.snapshot.id == snapshot.id)
        {
            return if job.snapshot == snapshot {
                Ok(JobReceipt::AlreadyCreated)
            } else {
                Err(ConsolidationError::Conflict)
            };
        }
        self.jobs.push(ConsolidationJob {
            snapshot,
            status: JobStatus::Pending,
            revisions: Vec::new(),
        });
        Ok(JobReceipt::Created)
    }
    fn get(&self, owner: &OwnerId, id: &JobId) -> Result<ConsolidationJob, ConsolidationError> {
        self.jobs
            .iter()
            .find(|job| &job.snapshot.owner == owner && &job.snapshot.id == id)
            .cloned()
            .ok_or(ConsolidationError::Missing)
    }
    fn complete(
        &mut self,
        owner: &OwnerId,
        id: &JobId,
        revisions: Vec<Revision>,
    ) -> Result<JobReceipt, ConsolidationError> {
        let index = self
            .jobs
            .iter()
            .position(|job| &job.snapshot.owner == owner && &job.snapshot.id == id)
            .ok_or(ConsolidationError::Missing)?;
        if self.jobs[index].status == JobStatus::Completed {
            return if self.jobs[index].revisions == revisions {
                Ok(JobReceipt::AlreadyCompleted)
            } else {
                Err(ConsolidationError::Conflict)
            };
        }
        for revision in &revisions {
            let prior: Vec<_> = self
                .jobs
                .iter()
                .flat_map(|job| &job.revisions)
                .filter(|old| old.owner == *owner)
                .collect();
            if prior.iter().any(|old| old.id == revision.id) {
                return Err(ConsolidationError::Conflict);
            }
            if let Some(predecessor) = &revision.predecessor {
                if !prior.iter().any(|old| {
                    &old.id == predecessor
                        && old.memory == revision.memory
                        && old.project == revision.project
                        && old.kind == revision.kind
                }) {
                    return Err(ConsolidationError::InvalidRevision);
                }
            } else if prior.iter().any(|old| old.memory == revision.memory) {
                return Err(ConsolidationError::InvalidRevision);
            }
            // Linear history: no two committed children of the same revision.
            if revision.predecessor.is_some()
                && prior.iter().any(|old| {
                    old.memory == revision.memory && old.predecessor == revision.predecessor
                })
            {
                return Err(ConsolidationError::Conflict);
            }
        }
        self.jobs[index].revisions = revisions;
        self.jobs[index].status = JobStatus::Completed;
        Ok(JobReceipt::Completed)
    }
}

/// Durable async contracts; SDK data remains inside integration adapters.
pub trait AsyncConsolidationStore {
    fn create_async(
        &self,
        snapshot: JobSnapshot,
    ) -> impl std::future::Future<Output = Result<JobReceipt, ConsolidationError>>;
    fn get_async(
        &self,
        owner: &OwnerId,
        id: &JobId,
    ) -> impl std::future::Future<Output = Result<ConsolidationJob, ConsolidationError>>;
    fn complete_async(
        &self,
        owner: &OwnerId,
        id: &JobId,
        revisions: Vec<Revision>,
    ) -> impl std::future::Future<Output = Result<JobReceipt, ConsolidationError>>;
}
pub trait RevisionRetriever {
    fn retrieve_pending(
        &self,
        request: &crate::domain_models::retrieval::RevisionRetrievalRequest,
    ) -> impl std::future::Future<
        Output = Result<
            Vec<crate::domain_models::retrieval::ProvisionalRevisionHit>,
            crate::domain_models::MemoryError,
        >,
    >;
}
