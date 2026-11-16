use super::{ActorId, OwnerId, ProjectId, SourceId};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkEventKind {
    Attempt,
    Result,
    Error,
    Decision,
    Action,
    Exchange,
    Lesson,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WorkOutcome {
    Unknown,
    Succeeded,
    Failed,
}

/// Action episodes and legacy per-tool results are retained, but never curated.
pub fn is_curatable(kind: &WorkEventKind, origin: &str) -> bool {
    *kind != WorkEventKind::Action && !origin.starts_with("omp://tool_result")
}

/// Identity imports reserve space for subsequent live capture.
pub fn import_pending_limit(max_pending: usize) -> usize {
    max_pending.saturating_sub(128)
}
/// Sanitized, relevant evidence; timestamps are supplied by the caller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureEvent {
    pub source: SourceId,
    pub owner: OwnerId,
    pub actor: ActorId,
    pub project: ProjectId,
    pub origin: String,
    pub recorded_at_ms: u64,
    pub kind: WorkEventKind,
    pub outcome: WorkOutcome,
    pub content: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureStatus {
    SavedPending,
    AlreadySavedPending,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CaptureReceipt {
    pub source: SourceId,
    pub status: CaptureStatus,
}
/// Pending evidence never implies an active semantic fact or procedure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingEvidence {
    pub event: CaptureEvent,
    pub provisional: bool,
}
#[derive(Clone, Debug)]
pub struct CaptureLimits {
    pub max_content_bytes: usize,
    pub max_pending_per_owner: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureError {
    InvalidEvidence,
    BacklogFull,
    SourceConflict,
    StorageUnavailable,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CaptureWrite {
    Inserted,
    ExistingIdentical,
    Conflict,
    Full,
}

#[derive(Clone, Debug)]
pub struct ActivitySnapshot {
    pub last_capture_ms: u64,
    pub last_recall_ms: u64,
    pub capture_in_flight: bool,
    pub recall_in_flight: bool,
}
#[derive(Clone, Debug)]
pub struct DreamingPolicy {
    pub idle_for_ms: u64,
    pub max_batch: usize,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DreamingBatch {
    pub owner: OwnerId,
    pub sources: Vec<SourceId>,
    pub watermark_ms: u64,
}
