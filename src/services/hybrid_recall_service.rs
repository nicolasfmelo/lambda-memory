use crate::domain_models::embeddings::{HybridOutput, HybridPolicy};
use crate::domain_models::{
    MemoryError, RecallOutput, RecallPolicy, RecallRequest, SelectionRequest,
};
use crate::engines::{hybrid_policy, recall_policy};
use crate::integrations::embedding_ports::{EmbeddingClient, VectorRepository};
use crate::integrations::memory_ports::{MemoryQueryPlanner, MemoryRepository, MemorySelector};
use std::time::Instant;
pub struct HybridRecallService<L, V, E, S, P> {
    lexical: L,
    vector: V,
    embedder: E,
    selector: S,
    planner: P,
    recall: RecallPolicy,
    hybrid: HybridPolicy,
}
impl<
    L: MemoryRepository,
    V: VectorRepository,
    E: EmbeddingClient,
    S: MemorySelector,
    P: MemoryQueryPlanner,
> HybridRecallService<L, V, E, S, P>
{
    pub fn new(
        lexical: L,
        vector: V,
        embedder: E,
        selector: S,
        planner: P,
        recall: RecallPolicy,
        hybrid: HybridPolicy,
    ) -> Self {
        Self {
            lexical,
            vector,
            embedder,
            selector,
            planner,
            recall,
            hybrid,
        }
    }
    pub async fn recall(&self, request: RecallRequest) -> Result<HybridOutput, MemoryError> {
        recall_policy::validate(&request, &self.recall)?;
        let start = Instant::now();
        let plan = self.planner.plan(&request).await?;
        hybrid_policy::validate_plan(&request, &plan, &self.hybrid)?;
        if self.hybrid.fusion_limit > self.recall.max_candidates {
            return Err(MemoryError::InvalidInput("fusion exceeds selector limit"));
        }
        let planning_latency = start.elapsed();
        let search_start = Instant::now();
        let lexical = self.lexical.search(&plan, self.hybrid.stream_limit).await?;
        let vector_result = match self.embedder.embed(&request.task).await {
            Ok(query) => {
                self.vector
                    .search_vector(&plan, &query, self.hybrid.stream_limit)
                    .await
            }
            Err(error) => Err(error),
        };
        let (vector, vector_failure) = match vector_result {
            Ok(hits) => (hits, None),
            Err(error) => (vec![], Some(error)),
        };
        let vector_candidates = vector.len();
        let candidates = hybrid_policy::fuse(&request, &plan, lexical, vector, &self.hybrid);
        let search_latency = search_start.elapsed();
        let selection_start = Instant::now();
        let (selected, failure) = if candidates.is_empty() {
            (vec![], None)
        } else {
            match self
                .selector
                .evaluate(&SelectionRequest {
                    task: request.task.clone(),
                    retrieval_terms: plan.terms.clone(),
                    candidates: candidates.clone(),
                })
                .await
            {
                Ok(outcome) => (recall_policy::select(&request, &candidates, &outcome), None),
                Err(error) => (vec![], Some(error)),
            }
        };
        Ok(HybridOutput {
            vector_failure,
            vector_candidates,
            recall: RecallOutput {
                evidence: recall_policy::pack(selected, request.budget_bytes),
                query_plan: plan,
                planning_latency,
                candidate_ids: candidates.into_iter().map(|m| m.id).collect(),
                search_latency,
                selection_latency: selection_start.elapsed(),
                failure,
            },
        })
    }
}
