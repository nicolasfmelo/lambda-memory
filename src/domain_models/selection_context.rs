//! Caller-declared recall semantics and native-store validity attestations.
use super::{MemoryId, OwnerId, ProjectId, SourceId};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectionIntent {
    TopicRecall,
    FactualQuestion,
}
#[derive(Clone, Debug)]
pub struct CandidateValidity {
    pub memory: MemoryId,
    pub source: SourceId,
    pub owner: OwnerId,
    pub project: ProjectId,
    pub valid_from_ms: u64,
    pub expires_at_ms: Option<u64>,
}
#[derive(Clone, Debug)]
pub struct SelectionContext {
    pub intent: SelectionIntent,
    pub owner: OwnerId,
    pub project: ProjectId,
    pub as_of_ms: u64,
    pub validity: Vec<CandidateValidity>,
}

/// No inference from query keywords. Missing, duplicate or stale attestations fail closed.
pub fn validate_selection_context(
    request: &crate::domain_models::SelectionRequest,
    context: &crate::domain_models::selection_context::SelectionContext,
) -> Result<(), crate::domain_models::MemoryError> {
    if context.validity.len() != request.candidates.len() {
        return Err(crate::domain_models::MemoryError::InvalidSelection);
    }
    for memory in &request.candidates {
        let attestations: Vec<_> = context
            .validity
            .iter()
            .filter(|v| v.memory == memory.id)
            .collect();
        if attestations.len() != 1 {
            return Err(crate::domain_models::MemoryError::InvalidSelection);
        }
        let v = attestations[0];
        if memory.owner != context.owner
            || memory.project != context.project
            || v.owner != memory.owner
            || v.project != memory.project
            || v.source != memory.source
            || v.valid_from_ms > context.as_of_ms
            || v.expires_at_ms
                .is_some_and(|expiry| context.as_of_ms >= expiry)
            || !memory.active
            || memory.restricted
        {
            return Err(crate::domain_models::MemoryError::InvalidSelection);
        }
    }
    Ok(())
}
