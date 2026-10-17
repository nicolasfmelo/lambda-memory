use crate::domain_models::{MemoryError, retrieval::*};
use crate::engines::retrieval_policy;
use crate::integrations::consolidation_store::RevisionRetriever;
pub struct ReviewRetrievalService<R> {
    repository: R,
}
impl<R: RevisionRetriever> ReviewRetrievalService<R> {
    pub fn new(repository: R) -> Self {
        Self { repository }
    }
    /// Caller-authorized scope only; results are explicitly provisional, not active recall.
    pub async fn retrieve(
        &self,
        request: RevisionRetrievalRequest,
    ) -> Result<Vec<ProvisionalRevisionHit>, MemoryError> {
        let request = retrieval_policy::normalize(request)?;
        self.repository.retrieve_pending(&request).await
    }
}
