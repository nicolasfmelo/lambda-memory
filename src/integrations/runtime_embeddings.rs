//! Persistent identities shared by admission, recovery and operational diagnostics.
use crate::domain_models::embeddings::EmbeddingSpace;
use crate::domain_models::runtime::RuntimeRevision;
use crate::domain_models::{OwnerId, ProjectId, RevisionId};
use sha2::{Digest, Sha256};

pub fn digest(parts: &[&str]) -> String {
    let mut hash = Sha256::new();
    for part in parts {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    format!("{:x}", hash.finalize())
}
pub fn operation(
    owner: &OwnerId,
    project: &ProjectId,
    revision: &RevisionId,
    space: &EmbeddingSpace,
) -> String {
    format!(
        "embedding:{}",
        digest(&[
            owner.as_str(),
            project.as_str(),
            revision.as_str(),
            &space.provider,
            &space.model,
            &space.revision,
            &space.dimensions.to_string()
        ])
    )
}
/// Old markers did not encode dimensions. Their outcome remains unknown in that same space.
pub fn legacy_operation(revision: &RevisionId, space: &EmbeddingSpace) -> String {
    format!(
        "embedding:{}",
        digest(&[
            revision.as_str(),
            &space.provider,
            &space.model,
            &space.revision
        ])
    )
}
fn field(hash: &mut Sha256, bytes: &[u8]) {
    hash.update((bytes.len() as u64).to_be_bytes());
    hash.update(bytes);
}
pub fn snapshot(item: &RuntimeRevision) -> String {
    use crate::domain_models::capture::{WorkEventKind, WorkOutcome};
    use crate::domain_models::consolidation::{KnowledgeKind, RevisionStatus};
    let mut hash = Sha256::new();
    let revision = &item.revision;
    for text in [
        revision.id.as_str(),
        revision.memory.as_str(),
        revision.owner.as_str(),
        revision.project.as_str(),
        revision.content.as_str(),
        revision.model.as_str(),
        revision.prompt_version.as_str(),
    ] {
        field(&mut hash, text.as_bytes());
    }
    field(
        &mut hash,
        match revision.kind {
            KnowledgeKind::Knowledge => b"knowledge",
            KnowledgeKind::Procedure => b"procedure",
        },
    );
    field(
        &mut hash,
        match revision.status {
            RevisionStatus::PendingReview => b"pending_review",
        },
    );
    field(
        &mut hash,
        &(revision.prerequisites.len() as u64).to_be_bytes(),
    );
    for text in &revision.prerequisites {
        field(&mut hash, text.as_bytes());
    }
    field(&mut hash, &(revision.sources.len() as u64).to_be_bytes());
    for source in &revision.sources {
        field(&mut hash, source.as_str().as_bytes());
    }
    field(
        &mut hash,
        revision
            .predecessor
            .as_ref()
            .map_or(b"".as_slice(), |id| id.as_str().as_bytes()),
    );
    field(&mut hash, &(item.sources.len() as u64).to_be_bytes());
    for source in &item.sources {
        for text in [
            source.source.as_str(),
            source.owner.as_str(),
            source.actor.as_str(),
            source.project.as_str(),
            source.origin.as_str(),
            source.content.as_str(),
        ] {
            field(&mut hash, text.as_bytes());
        }
        field(&mut hash, &source.recorded_at_ms.to_be_bytes());
        field(
            &mut hash,
            match source.kind {
                WorkEventKind::Attempt => b"attempt",
                WorkEventKind::Result => b"result",
                WorkEventKind::Error => b"error",
                WorkEventKind::Decision => b"decision",
                WorkEventKind::Action => b"action",
                WorkEventKind::Exchange => b"exchange",
            },
        );
        field(
            &mut hash,
            match source.outcome {
                WorkOutcome::Unknown => b"unknown",
                WorkOutcome::Succeeded => b"succeeded",
                WorkOutcome::Failed => b"failed",
            },
        );
    }
    format!("{:x}", hash.finalize())
}
