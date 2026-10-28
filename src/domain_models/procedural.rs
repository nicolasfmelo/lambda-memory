//! Procedural-memory vocabulary: sanitized action episodes and fixed identities.

/// Fixed memory ID of the per-project procedural playbook.
pub const PLAYBOOK_MEMORY: &str = "procedural:playbook";
/// Origin of the legacy per-event tool-metadata sources, excluded from derivation.
pub const LEGACY_TOOL_ORIGIN: &str = "omp://tool_result";

/// One sanitized tool call, as stored in the content of an `action` source.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionEpisode {
    pub tool: String,
    pub intent: String,
    pub summary: String,
    pub error_head: Option<String>,
    pub session: String,
    pub seq: u64,
}

/// One retained action episode with its outcome and capture time, input of the aggregation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EpisodeRecord {
    pub episode: ActionEpisode,
    pub outcome: super::capture::WorkOutcome,
    pub recorded_at_ms: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FactKind {
    Procedure,
    Pitfall,
    HotFile,
}

/// A deterministic, numbered fact the playbook may cite as `[F#]`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProceduralFact {
    /// `F1`, `F2`, … in rank order.
    pub id: String,
    pub kind: FactKind,
    /// Normalized command key (procedure), fixing command key (pitfall) or edited path (hot file).
    pub key: String,
    /// Pitfall only: the error head that was fixed.
    pub detail: Option<String>,
    pub successes: u32,
    pub failures: u32,
    pub unverified: u32,
    pub sessions: u32,
    pub last_seen_ms: u64,
}

/// Why a generated playbook was refused.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlaybookRejection {
    Empty,
    TooLarge,
    UncitedBullet,
    UnknownCitation(String),
}

/// Read-only view of a project's active playbook; all `None` and `0` when it has none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybookView {
    pub revision: Option<super::RevisionId>,
    pub content: Option<String>,
    /// Publication time of the active version.
    pub generated_at_ms: Option<u64>,
    /// Retained action episodes of the project.
    pub episodes: u64,
}
