use crate::domain_models::{
    MemoryError, MemoryRelationFilter, RecallOutput, RecallPolicy, RecallRequest, SelectionRequest,
};
use crate::engines::recall_policy::{pack, scope_candidates, select, validate};
use crate::integrations::memory_ports::{MemoryQueryPlanner, MemoryRepository, MemorySelector};
use std::time::Instant;

pub struct RecallService<R, S, P> {
    repository: R,
    selector: S,
    planner: P,
    policy: RecallPolicy,
}
impl<R: MemoryRepository, S: MemorySelector, P: MemoryQueryPlanner> RecallService<R, S, P> {
    pub fn new(repository: R, selector: S, planner: P, policy: RecallPolicy) -> Self {
        Self {
            repository,
            selector,
            planner,
            policy,
        }
    }
    pub async fn recall(&self, request: RecallRequest) -> Result<RecallOutput, MemoryError> {
        validate(&request, &self.policy)?;
        let start = Instant::now();
        let plan = self.planner.plan(&request).await?;
        let source_count: usize = plan
            .relations
            .iter()
            .map(|filter| match filter {
                MemoryRelationFilter::Sources(ids) => ids.len(),
            })
            .sum();
        if plan.owner != request.owner
            || plan.actor != request.actor
            || plan.project != request.project
            || plan.terms.is_empty()
            || plan.terms.len() > 12
            || plan
                .terms
                .iter()
                .any(|term| term.is_empty() || term.len() > 128)
            || plan.relations.len() > 4
            || source_count > 8
        {
            return Err(MemoryError::InvalidInput(
                "planner attempted invalid scope or terms",
            ));
        }
        let planning_latency = start.elapsed();
        let search_start = Instant::now();
        let candidates = scope_candidates(
            &request,
            self.repository
                .search(&plan, self.policy.max_candidates)
                .await?,
            self.policy.max_candidates,
        );
        let search_latency = search_start.elapsed();
        let selection_start = Instant::now();
        let (selected, failure) = if candidates.is_empty() {
            (vec![], None)
        } else {
            let selection_request = SelectionRequest {
                task: request.task.clone(),
                retrieval_terms: plan.terms.clone(),
                candidates: candidates.clone(),
            };
            match self.selector.evaluate(&selection_request).await {
                Ok(outcome) => (select(&request, &candidates, &outcome), None),
                Err(error) => (vec![], Some(error)),
            }
        };
        Ok(RecallOutput {
            evidence: pack(selected, request.budget_bytes),
            query_plan: plan,
            planning_latency,
            candidate_ids: candidates.into_iter().map(|m| m.id).collect(),
            search_latency,
            selection_latency: selection_start.elapsed(),
            failure,
        })
    }
}
