//! Project-scoped operational evidence. No transport, clock or datastore dependencies.
use super::runtime::RuntimeError;
use super::{ActivityId, OwnerId, ProjectId, RevisionId, SourceId, WorkerInstanceId};

pub const ACTIVITY_RETENTION_MS: u64 = 7 * 24 * 60 * 60 * 1000;
pub const ACTIVITY_DEFAULT_LIMIT: usize = 20;
pub const ACTIVITY_MAX_LIMIT: usize = 32;
pub const ACTIVITY_MAX_CURSOR_BYTES: usize = 1024;

macro_rules! codes {
    ($name:ident { $($variant:ident => $code:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { $($variant),+ }
        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $code),+ }
            }
            pub fn parse(value: &str) -> Option<Self> {
                match value { $($code => Some(Self::$variant)),+, _ => None }
            }
        }
    };
}

codes!(ActivityOperation {
    Capture => "capture", Propose => "propose", Dream => "dream", List => "list",
    Recall => "recall", Review => "review", Purge => "purge", Timeline => "timeline",
});
codes!(ActivityPhase {
    Capture => "capture", Propose => "propose", List => "list", Review => "review",
    Purge => "purge", Preparation => "preparation", Derivation => "derivation",
    Indexing => "indexing", Publication => "publication", QueryPlanning => "query_planning",
    QueryEmbedding => "query_embedding", Retrieval => "retrieval", Selection => "selection",
    Packing => "packing", Timeline => "timeline",
});
codes!(ActivityOutcome {
    Running => "running", Succeeded => "succeeded", Failed => "failed",
    Indeterminate => "indeterminate",
});
codes!(ActivityReason {
    InvalidInput => "invalid_input", Missing => "missing", BacklogFull => "backlog_full",
    ProviderUnavailable => "provider_unavailable", ProviderRejected => "provider_rejected",
    UncertainAdmission => "uncertain_admission",
    StorageUnavailable => "storage_unavailable", LifecycleConflict => "lifecycle_conflict",
    Interrupted => "interrupted", FinalRecordUnavailable => "final_record_unavailable",
    InternalError => "internal_error",
});
codes!(ActivityEntryKind { Operation => "operation", Stage => "stage" });
codes!(DreamState {
    Running => "running", WaitingForeground => "waiting_foreground", WaitingIdle => "waiting_idle",
    Ready => "ready", Idle => "idle", Stopped => "stopped",
});
codes!(DiagnosticRecordingState { Ok => "ok", Degraded => "degraded" });

impl ActivityReason {
    pub fn from_error(error: &RuntimeError) -> Self {
        match error {
            RuntimeError::Unauthorized | RuntimeError::InvalidInput(_) => Self::InvalidInput,
            RuntimeError::Conflict => Self::LifecycleConflict,
            RuntimeError::Missing => Self::Missing,
            RuntimeError::BacklogFull => Self::BacklogFull,
            RuntimeError::StorageUnavailable => Self::StorageUnavailable,
            RuntimeError::ProviderUnavailable => Self::ProviderUnavailable,
            RuntimeError::ProviderRejected => Self::ProviderRejected,
            RuntimeError::UncertainAdmission => Self::UncertainAdmission,
        }
    }
}

/// None means inapplicable or unobserved, not a claimed zero effect.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ActivityCounts {
    pub sources_created: Option<u64>,
    pub sources_consumed: Option<u64>,
    pub revisions_created: Option<u64>,
    pub revisions_indexed: Option<u64>,
    pub revisions_published: Option<u64>,
    pub hits_returned: Option<u64>,
    pub sources_removed: Option<u64>,
    pub revisions_removed: Option<u64>,
    pub jobs_removed: Option<u64>,
}
impl ActivityCounts {
    pub fn accumulate(&mut self, other: &Self) {
        fn add(target: &mut Option<u64>, value: Option<u64>) {
            if let Some(value) = value {
                *target = Some(target.unwrap_or(0).saturating_add(value));
            }
        }
        add(&mut self.sources_created, other.sources_created);
        add(&mut self.sources_consumed, other.sources_consumed);
        add(&mut self.revisions_created, other.revisions_created);
        add(&mut self.revisions_indexed, other.revisions_indexed);
        add(&mut self.revisions_published, other.revisions_published);
        add(&mut self.hits_returned, other.hits_returned);
        add(&mut self.sources_removed, other.sources_removed);
        add(&mut self.revisions_removed, other.revisions_removed);
        add(&mut self.jobs_removed, other.jobs_removed);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActivityEntry {
    pub id: ActivityId,
    pub parent: Option<ActivityId>,
    pub project: ProjectId,
    pub entry_kind: ActivityEntryKind,
    pub operation: ActivityOperation,
    pub phase: ActivityPhase,
    pub instance: WorkerInstanceId,
    pub started_at_ms: u64,
    pub finished_at_ms: Option<u64>,
    pub classified_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub outcome: ActivityOutcome,
    pub reason: Option<ActivityReason>,
    pub counts: ActivityCounts,
}

/// Internal provenance associations are never part of diagnostic transport output.
#[derive(Clone, Debug)]
pub struct ActivityRecord {
    pub entry: ActivityEntry,
    pub sources: Vec<SourceId>,
    pub revisions: Vec<RevisionId>,
    pub paid_operation: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ActivityFinish {
    pub id: ActivityId,
    pub phase: ActivityPhase,
    pub outcome: ActivityOutcome,
    pub finished_at_ms: Option<u64>,
    pub classified_at_ms: Option<u64>,
    pub duration_ms: Option<u64>,
    pub reason: Option<ActivityReason>,
    pub counts: ActivityCounts,
}

/// Explicit operation-local completion, applied with the datastore effect transaction.
#[derive(Clone, Debug)]
pub struct ActivityCommit {
    pub owner: OwnerId,
    pub project: ProjectId,
    pub finish: ActivityFinish,
    pub root_finish: Option<ActivityFinish>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActivityWrite {
    Recorded,
    AlreadyClosed,
    Removed,
}

#[derive(Clone, Debug)]
pub struct ActivityReadContext {
    pub instance: WorkerInstanceId,
    pub active_entries: Vec<ActivityId>,
    pub active_paid_operations: Vec<String>,
}

#[derive(Clone, Debug)]
pub struct ActivityRequest {
    pub project: ProjectId,
    pub limit: usize,
    pub operations: Vec<ActivityOperation>,
    pub outcomes: Vec<ActivityOutcome>,
    pub cursor: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryCoverage {
    pub retention_days: u8,
    pub instrumentation_started_at_ms: Option<u64>,
    /// Cutoff applies to the root's finish/classification time, not entry start time.
    pub retained_since_ms: u64,
}

#[derive(Clone, Debug)]
pub struct ActivityPage {
    pub project: ProjectId,
    pub observed_at_ms: u64,
    pub history: HistoryCoverage,
    pub entries: Vec<ActivityEntry>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PendingCounts {
    pub unconsumed_sources: u64,
    pub processing_revisions: u64,
    pub blocked_sources: u64,
    pub blocked_revisions: u64,
    pub active_without_current_embedding: Option<u64>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LifecycleCounts {
    pub active: u64,
    pub processing: u64,
    pub conflicted: u64,
    pub rejected: u64,
    pub superseded: u64,
}

#[derive(Clone, Debug)]
pub struct StoredActivityStatus {
    pub pending: PendingCounts,
    pub lifecycle: LifecycleCounts,
    pub unresolved_conflicts: u64,
    pub last_success: Option<ActivityEntry>,
    pub last_failure: Option<ActivityEntry>,
    pub last_dream_success: Option<ActivityEntry>,
    pub last_dream_failure: Option<ActivityEntry>,
    pub history: HistoryCoverage,
}

#[derive(Clone, Debug)]
pub struct DreamStatus {
    pub state: DreamState,
    pub phase: Option<ActivityPhase>,
    pub stop_reason: Option<ActivityReason>,
    pub idle_ms: u64,
    pub tick_ms: u64,
    pub dream_batch: usize,
    pub remaining_idle_ms: Option<u64>,
}

#[derive(Clone, Debug)]
pub struct WorkerStatus {
    pub instance: WorkerInstanceId,
    pub started_at_ms: u64,
    pub foreground_active: usize,
    pub dreaming: DreamStatus,
}

#[derive(Clone, Debug)]
pub struct RuntimeStatus {
    pub project: ProjectId,
    pub mode: &'static str,
    pub observed_at_ms: u64,
    pub worker: WorkerStatus,
    pub stored: StoredActivityStatus,
    pub recording_state: DiagnosticRecordingState,
    pub recording_reason: Option<ActivityReason>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiagnosticsWarning {
    pub operation_id: ActivityId,
    pub reason: ActivityReason,
}

/// Known memory results and recording failures have independent semantics.
#[derive(Debug)]
pub struct RecordedOperation<T> {
    pub result: Result<T, RuntimeError>,
    pub diagnostics_warning: Option<DiagnosticsWarning>,
}
