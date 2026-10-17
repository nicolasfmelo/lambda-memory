use super::capture::CaptureEvent;
use super::{MemoryId, OwnerId, ProjectId, SourceId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KnowledgeKind {
    Knowledge,
    Procedure,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RevisionStatus {
    PendingReview,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Revision {
    pub id: super::RevisionId,
    pub memory: MemoryId,
    pub owner: OwnerId,
    pub project: ProjectId,
    pub kind: KnowledgeKind,
    pub status: RevisionStatus,
    pub content: String,
    pub prerequisites: Vec<String>,
    pub sources: Vec<SourceId>,
    pub predecessor: Option<super::RevisionId>,
    pub model: String,
    pub prompt_version: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JobSnapshot {
    pub id: super::JobId,
    pub owner: OwnerId,
    pub project: ProjectId,
    pub watermark_ms: u64,
    /// Frozen originals; late captures cannot change this job's evidence.
    pub sources: Vec<CaptureEvent>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobStatus {
    Pending,
    Completed,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ConsolidationJob {
    pub snapshot: JobSnapshot,
    pub status: JobStatus,
    pub revisions: Vec<Revision>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ConsolidationError {
    InvalidSnapshot,
    InvalidRevision,
    Conflict,
    Missing,
    StorageUnavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum JobReceipt {
    Created,
    AlreadyCreated,
    Completed,
    AlreadyCompleted,
}
