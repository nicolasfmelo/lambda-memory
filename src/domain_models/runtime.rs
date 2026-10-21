//! Authenticated runtime scope, lifecycle transitions and bounded operations.
use super::capture::CaptureEvent;
use super::consolidation::Revision;
use super::{ActorId, MemoryId, OwnerId, ProjectId, RevisionId, SourceId};

#[derive(Clone, Debug)]
pub struct RuntimeScope {
    pub owner: OwnerId,
    pub actor: ActorId,
    /// Operator-authorized projects. Request arguments cannot widen this list.
    pub projects: Vec<ProjectId>,
}

#[derive(Clone, Debug)]
pub struct RuntimeLimits {
    pub max_content_bytes: usize,
    pub max_pending: usize,
    pub max_candidates: usize,
    pub context_bytes: usize,
    pub dream_batch: usize,
    pub idle_ms: u64,
    pub tick_ms: u64,
    /// None preserves evidence indefinitely; purge remains explicit.
    pub retention_ms: Option<u64>,
}
impl Default for RuntimeLimits {
    fn default() -> Self {
        Self {
            max_content_bytes: 8192,
            max_pending: 1024,
            max_candidates: 8,
            context_bytes: 8192,
            dream_batch: 8,
            idle_ms: 60_000,
            tick_ms: 1000,
            retention_ms: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LifecycleState {
    /// Awaiting automatic indexing/publication, never human approval.
    Processing,
    /// Retrievable historical evidence, not verified truth.
    Active,
    Rejected,
    Conflicted,
    Superseded,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReviewAction {
    Promote,
    Reject,
    Conflict,
}
#[derive(Clone, Debug)]
pub struct ReviewRequest {
    pub project: ProjectId,
    pub revision: RevisionId,
    pub action: ReviewAction,
    /// Compare-and-swap guard: an explicit correction replaces exactly this active revision.
    pub expected_active: Option<RevisionId>,
    pub conflict_with: Option<RevisionId>,
}
#[derive(Clone, Debug)]
pub struct RuntimeRevision {
    pub revision: Revision,
    pub state: LifecycleState,
    pub sources: Vec<CaptureEvent>,
    pub reviewed_at_ms: Option<u64>,
    /// Lineage ordering key: the job watermark the revision was committed under.
    pub watermark_ms: u64,
}
/// A bounded review listing page, keyset on `(watermark_ms, revision_id)`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ListQuery {
    pub project: ProjectId,
    pub states: Vec<LifecycleState>,
    pub limit: usize,
    /// Restricts the page to one memory's revision lineage.
    pub memory: Option<MemoryId>,
    pub after: Option<(u64, RevisionId)>,
}
/// One recorded lifecycle transition; the acting principal is never exposed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LifecycleAuditEntry {
    pub revision: RevisionId,
    pub action: ReviewAction,
    pub from: LifecycleState,
    pub to: LifecycleState,
    pub expected_active: Option<RevisionId>,
    pub conflict_with: Option<RevisionId>,
    pub at_ms: u64,
}
#[derive(Clone, Debug)]
pub struct ListPage {
    pub items: Vec<RuntimeRevision>,
    /// Transitions of the returned items, ordered by `at_ms`; filled only for lineage reads.
    pub audit: Vec<LifecycleAuditEntry>,
    pub more: bool,
}
#[derive(Clone, Debug)]
pub struct RuntimeHit {
    pub item: RuntimeRevision,
    pub lexical_score: u64,
}
#[derive(Clone, Debug)]
pub struct RuntimeRecall {
    pub context: String,
    pub hits: Vec<RuntimeHit>,
    pub mode: &'static str,
    /// At least one hit was shortened or omitted to fit the context budget.
    pub truncated: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PurgeReceipt {
    pub sources: usize,
    pub revisions: usize,
    pub jobs: usize,
    pub timeline_entries: usize,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DreamReceipt {
    pub batches: usize,
    pub sources: usize,
}

/// Exact originals and admissions an operator must inspect before authorizing a retry.
#[derive(Clone, Debug)]
pub struct DreamRecoveryBatch {
    pub sources: Vec<CaptureEvent>,
    pub previous_operations: Vec<super::JobId>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RuntimeError {
    Unauthorized,
    InvalidInput(&'static str),
    Conflict,
    Missing,
    BacklogFull,
    StorageUnavailable,
    ProviderUnavailable,
    /// The provider refused the request before any billable work (offline, auth, credit, rate limit).
    ProviderRejected,
    /// A durable paid admission already exists; never replay it automatically.
    UncertainAdmission,
}
impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => f.write_str("scope not authorized"),
            Self::InvalidInput(reason) => write!(f, "invalid runtime input: {reason}"),
            Self::Conflict => f.write_str("lifecycle or source conflict"),
            Self::Missing => f.write_str("record not found"),
            Self::BacklogFull => f.write_str("pending capture capacity reached"),
            Self::StorageUnavailable => f.write_str("runtime storage unavailable"),
            Self::ProviderUnavailable => f.write_str(
                "OpenRouter memory operation failed; no local fallback or automatic retry",
            ),
            Self::ProviderRejected => f.write_str(
                "OpenRouter rejected the memory operation without charge; it may be retried",
            ),
            Self::UncertainAdmission => f.write_str(
                "memory operation has an unsettled paid admission; automatic replay is forbidden",
            ),
        }
    }
}
impl std::error::Error for RuntimeError {}

/// Stable memory identity can be specified by reviewable clients, never inferred from similarity.
#[derive(Clone, Debug)]
pub struct RevisionProposal {
    pub project: ProjectId,
    pub memory: MemoryId,
    pub revision: RevisionId,
    pub sources: Vec<SourceId>,
    pub content: String,
    pub predecessor: Option<RevisionId>,
}
