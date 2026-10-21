//! Pure domain vocabulary. No infrastructure or other-layer dependencies.
use std::fmt;
use std::time::Duration;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MemoryError {
    InvalidIdentity(&'static str),
    InvalidInput(&'static str),
    StorageUnavailable,
    SelectionUnavailable,
    InvalidSelection,
    PaidCallsDisabled,
    /// The provider refused the request before doing billable work.
    ProviderRejected,
}
impl fmt::Display for MemoryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidIdentity(kind) => write!(formatter, "invalid {kind}"),
            Self::InvalidInput(reason) => write!(formatter, "invalid recall input: {reason}"),
            Self::StorageUnavailable => formatter.write_str("memory storage unavailable"),
            Self::SelectionUnavailable => formatter.write_str("memory selection unavailable"),
            Self::InvalidSelection => formatter.write_str("invalid memory selection"),
            Self::PaidCallsDisabled => formatter.write_str("paid memory calls disabled"),
            Self::ProviderRejected => {
                formatter.write_str("memory provider rejected the request without charge")
            }
        }
    }
}
impl std::error::Error for MemoryError {}

macro_rules! identity {
    ($name:ident, $kind:literal) => {
        #[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(String);
        impl $name {
            pub fn new(value: impl Into<String>) -> Result<Self, MemoryError> {
                let value = value.into();
                if value.trim().is_empty()
                    || value.len() > 512
                    || value.chars().any(char::is_control)
                {
                    return Err(MemoryError::InvalidIdentity($kind));
                }
                Ok(Self(value))
            }
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(formatter)
            }
        }
    };
}
identity!(MemoryId, "memory ID");
identity!(ActorId, "actor ID");
identity!(OwnerId, "owner ID");
identity!(ProjectId, "project ID");
identity!(SourceId, "source ID");

#[derive(Clone, Debug)]
pub struct Memory {
    pub id: MemoryId,
    pub owner: OwnerId,
    pub actor: ActorId,
    pub project: ProjectId,
    pub text: String,
    pub source: SourceId,
    pub active: bool,
    pub restricted: bool,
}
#[derive(Clone, Debug)]
pub struct RecallRequest {
    pub task: String,
    pub owner: OwnerId,
    pub actor: ActorId,
    pub project: ProjectId,
    pub budget_bytes: usize,
}
#[derive(Clone, Debug)]
pub struct RecallPolicy {
    pub max_candidates: usize,
    pub paid_calls_enabled: bool,
}
impl Default for RecallPolicy {
    fn default() -> Self {
        Self {
            max_candidates: 4,
            paid_calls_enabled: false,
        }
    }
}
#[derive(Clone, Debug)]
pub struct SelectionRequest {
    pub task: String,
    pub retrieval_terms: Vec<String>,
    pub candidates: Vec<Memory>,
}
#[derive(Clone, Debug)]
pub enum MemoryRelationFilter {
    Sources(Vec<SourceId>),
}
#[derive(Clone, Debug)]
pub struct MemoryQueryPlan {
    pub terms: Vec<String>,
    pub owner: OwnerId,
    pub actor: ActorId,
    pub project: ProjectId,
    pub relations: Vec<MemoryRelationFilter>,
}
#[derive(Clone, Debug)]
pub struct CandidateRelevance {
    pub memory_id: MemoryId,
    pub relevance_probability: f64,
    pub utility_score: f64,
    pub confidence: f64,
}
#[derive(Clone, Debug)]
pub struct SelectionOutcome {
    pub best: Option<MemoryId>,
    pub confidence: f64,
    pub candidates: Vec<CandidateRelevance>,
}
#[derive(Debug)]
pub struct MemoryEvidence {
    pub context: String,
    pub selected: Vec<Memory>,
}
#[derive(Debug)]
pub struct RecallOutput {
    pub evidence: MemoryEvidence,
    pub query_plan: MemoryQueryPlan,
    pub planning_latency: Duration,
    pub candidate_ids: Vec<MemoryId>,
    pub search_latency: Duration,
    pub selection_latency: Duration,
    pub failure: Option<MemoryError>,
}

pub mod capture;

pub mod consolidation;

identity!(RevisionId, "revision ID");
identity!(JobId, "job ID");

pub mod embeddings;

pub mod retrieval;

pub mod lifecycle;

pub mod agent_models;

/// Explicit direct-support predicate is distinct from ordinal-score confidence.
#[derive(Clone, Debug)]
pub struct SupportedSelectionOutcome {
    pub outcome: SelectionOutcome,
    pub direct_support: std::collections::BTreeMap<MemoryId, f64>,
}

pub mod selection_context;

pub mod runtime;

identity!(ActivityId, "activity ID");
identity!(WorkerInstanceId, "worker instance ID");
pub mod runtime_diagnostics;
#[cfg(feature = "memory-runtime")]
pub mod runtime_embedding;
pub mod timeline;
