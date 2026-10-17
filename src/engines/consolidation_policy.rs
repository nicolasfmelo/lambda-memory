use crate::domain_models::capture::WorkOutcome;
use crate::domain_models::consolidation::*;

pub fn validate_snapshot(snapshot: &JobSnapshot) -> Result<(), ConsolidationError> {
    if snapshot.sources.is_empty() || snapshot.sources.len() > 32 {
        return Err(ConsolidationError::InvalidSnapshot);
    }
    for (i, event) in snapshot.sources.iter().enumerate() {
        if event.owner != snapshot.owner
            || event.project != snapshot.project
            || event.recorded_at_ms > snapshot.watermark_ms
            || event.content.trim().is_empty()
            || event.content.len() > 8192
            || event.origin.trim().is_empty()
            || event.origin.len() > 2048
            || event.origin.chars().any(char::is_control)
            || snapshot.sources[..i]
                .iter()
                .any(|old| old.source == event.source)
        {
            return Err(ConsolidationError::InvalidSnapshot);
        }
    }
    Ok(())
}
pub fn validate_revision(
    snapshot: &JobSnapshot,
    revision: &Revision,
) -> Result<(), ConsolidationError> {
    if revision.owner != snapshot.owner
        || revision.project != snapshot.project
        || revision.content.trim().is_empty()
        || revision.content.len() > 8192
        || revision.model.trim().is_empty()
        || revision.model.len() > 256
        || revision.prompt_version.trim().is_empty()
        || revision.prompt_version.len() > 256
        || revision.sources.is_empty()
        || revision.sources.len() > 32
        || revision.prerequisites.len() > 16
        || revision
            .prerequisites
            .iter()
            .any(|p| p.trim().is_empty() || p.len() > 1024)
    {
        return Err(ConsolidationError::InvalidRevision);
    }
    for (i, source) in revision.sources.iter().enumerate() {
        if revision.sources[..i].contains(source)
            || !snapshot.sources.iter().any(|event| &event.source == source)
        {
            return Err(ConsolidationError::InvalidRevision);
        }
    }
    if revision.kind == KnowledgeKind::Procedure
        && (revision.prerequisites.is_empty()
            || !snapshot.sources.iter().any(|event| {
                revision.sources.contains(&event.source) && event.outcome == WorkOutcome::Succeeded
            }))
    {
        return Err(ConsolidationError::InvalidRevision);
    }
    Ok(())
}
