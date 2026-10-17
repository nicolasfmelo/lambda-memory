use crate::domain_models::embeddings::HybridPolicy;
use crate::domain_models::{
    Memory, MemoryError, MemoryQueryPlan, MemoryRelationFilter, RecallRequest,
};
use crate::engines::recall_policy::eligible;
pub fn validate_plan(
    request: &RecallRequest,
    plan: &MemoryQueryPlan,
    policy: &HybridPolicy,
) -> Result<(), MemoryError> {
    let sources: usize = plan
        .relations
        .iter()
        .map(|f| match f {
            MemoryRelationFilter::Sources(ids) => ids.len(),
        })
        .sum();
    if request.owner != plan.owner
        || request.actor != plan.actor
        || request.project != plan.project
        || plan.terms.is_empty()
        || plan.terms.len() > 12
        || plan
            .terms
            .iter()
            .any(|t| t.trim().is_empty() || t.len() > 128)
        || plan.relations.len() > 4
        || sources > 8
        || policy.stream_limit == 0
        || policy.stream_limit > 32
        || policy.fusion_limit == 0
        || policy.fusion_limit > 99
        || policy.rrf_k == 0
        || policy.rrf_k > 1000
    {
        return Err(MemoryError::InvalidInput(
            "invalid hybrid scope, plan or limits",
        ));
    }
    Ok(())
}
/// Authorize and deduplicate each stream before rank assignment/fusion.
pub fn fuse(
    request: &RecallRequest,
    plan: &MemoryQueryPlan,
    lexical: Vec<Memory>,
    vector: Vec<Memory>,
    policy: &HybridPolicy,
) -> Vec<Memory> {
    let mut scores: Vec<(Memory, f64)> = Vec::new();
    for stream in [lexical, vector] {
        let mut seen = Vec::new();
        for memory in stream.into_iter().take(policy.stream_limit) {
            if !eligible(&memory, request)
                || !plan.relations.iter().all(|f| match f {
                    MemoryRelationFilter::Sources(ids) => ids.contains(&memory.source),
                })
                || seen.contains(&memory.id)
            {
                continue;
            }
            seen.push(memory.id.clone());
            let contribution = 1.0 / (policy.rrf_k as f64 + seen.len() as f64);
            if let Some((existing, score)) = scores.iter_mut().find(|(old, _)| old.id == memory.id)
            {
                // Inconsistent source/text under one ID cannot reinforce the candidate.
                if existing.source == memory.source && existing.text == memory.text {
                    *score += contribution;
                }
            } else {
                scores.push((memory, contribution));
            }
        }
    }
    scores.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.id.cmp(&b.0.id)));
    scores
        .into_iter()
        .take(policy.fusion_limit)
        .map(|(memory, _)| memory)
        .collect()
}
