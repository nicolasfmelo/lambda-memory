//! Domain vocabulary for curated memory trees.
use super::{MemoryId, ProjectId, RevisionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreKind {
    Project,
    User,
}
impl StoreKind {
    pub fn of(project: &ProjectId) -> Self {
        if project.as_str().starts_with("user:") {
            Self::User
        } else {
            Self::Project
        }
    }
}

pub fn roots(kind: StoreKind) -> &'static [TreeRoot] {
    match kind {
        StoreKind::Project => &[
            TreeRoot::Decisions,
            TreeRoot::Preferences,
            TreeRoot::Architecture,
            TreeRoot::OpenItems,
            TreeRoot::Lessons,
        ],
        StoreKind::User => &[
            TreeRoot::Preferences,
            TreeRoot::WorkingRules,
            TreeRoot::GeneralLessons,
        ],
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TreeRoot {
    Decisions,
    Preferences,
    Architecture,
    OpenItems,
    WorkingRules,
    GeneralLessons,
    Lessons,
}
impl TreeRoot {
    pub fn is_lesson(&self) -> bool {
        matches!(self, Self::Lessons | Self::GeneralLessons)
    }
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Decisions => "decisions",
            Self::Preferences => "preferences",
            Self::Architecture => "architecture",
            Self::OpenItems => "open_items",
            Self::WorkingRules => "working_rules",
            Self::GeneralLessons => "general_lessons",
            Self::Lessons => "lessons",
        }
    }
    pub fn label(&self) -> &'static str {
        match self {
            Self::Decisions => "Decisions",
            Self::Preferences => "Preferences",
            Self::Architecture => "Architecture",
            Self::OpenItems => "Open items",
            Self::WorkingRules => "Working rules",
            Self::GeneralLessons => "General lessons",
            Self::Lessons => "Lessons",
        }
    }
    pub fn memory_id(&self) -> MemoryId {
        MemoryId::new(match self {
            Self::Decisions => "tree-root:decisions",
            Self::Preferences => "tree-root:preferences",
            Self::Architecture => "tree-root:architecture",
            Self::OpenItems => "tree-root:open-items",
            Self::WorkingRules => "tree-root:working-rules",
            Self::GeneralLessons => "tree-root:general-lessons",
            Self::Lessons => "tree-root:lessons",
        })
        .expect("fixed root identity")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeMode {
    Flat,
    Tree,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Basis {
    #[default]
    Observed,
    Concluded,
}
impl Basis {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Concluded => "concluded",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Confidence {
    Low,
    Medium,
    High,
}
impl Confidence {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
    pub fn lower(self) -> Option<Self> {
        match self {
            Self::High => Some(Self::Medium),
            Self::Medium => Some(Self::Low),
            Self::Low => None,
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
    pub basis: Basis,
    pub confidence: Option<Confidence>,
    pub derived_from: Vec<RevisionId>,
    pub memory: MemoryId,
    pub revision: RevisionId,
    pub root: TreeRoot,
    pub parent: Option<MemoryId>,
    pub depth: u8,
    pub title: String,
    pub content: String,
    pub direct_sources: u32,
    pub children: u32,
    pub last_linked_ms: u64,
    pub valid_from_ms: Option<u64>,
}
/// Model-free project index for a frozen session brief.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MemoryBrief {
    pub skill_drafts_pending: usize,
    pub project: super::ProjectId,
    pub mode: TreeMode,
    pub roots: Vec<BriefRoot>,
    pub truncated: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefRoot {
    pub root: TreeRoot,
    pub nodes: Vec<BriefNode>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BriefNode {
    pub basis: Basis,
    pub node: MemoryId,
    pub title: String,
    pub children: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    Node(usize),
    Root(TreeRoot),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum LinkRelation {
    DependsOn,
    Replaces,
    Related,
}
impl LinkRelation {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DependsOn => "depends_on",
            Self::Replaces => "replaces",
            Self::Related => "related",
        }
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CandidateLink {
    pub to: usize,
    pub relation: LinkRelation,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurationOp {
    Discard,
    Promote,
    Merge {
        links: Vec<CandidateLink>,
        node: usize,
        title: String,
        content: String,
    },
    Supersede {
        links: Vec<CandidateLink>,
        node: usize,
        title: String,
        content: String,
        valid_from: Option<String>,
    },
    Create {
        links: Vec<CandidateLink>,
        parent: Slot,
        title: String,
        content: String,
    },
    Expand {
        node: usize,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurationAnswer {
    pub per_source: Vec<Vec<CurationOp>>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CurationView {
    pub map: Vec<TreeNode>,
    pub detailed: Vec<TreeNode>,
    pub expandable: Vec<usize>,
    pub root_children: Vec<(TreeRoot, u32)>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SplitAnswer {
    pub node_title: String,
    pub node_content: String,
    pub children: Vec<(String, String, Vec<usize>)>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkOperation {
    Reflect,
    Create,
    Merge,
    Supersede,
    Split,
    Rebuild,
    Migrate,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParentRef {
    Root(TreeRoot),
    Node(MemoryId),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ParkReason {
    Expand(MemoryId),
    Split,
}

#[derive(Clone, Debug)]
pub enum LinkFrom {
    Node(MemoryId),
    Created(usize),
}
#[derive(Clone, Debug)]
pub struct CurationLink {
    pub from: LinkFrom,
    pub to: MemoryId,
    pub relation: LinkRelation,
    pub sources: Vec<super::SourceId>,
}
#[derive(Clone, Debug)]
pub struct NodeLink {
    pub node: MemoryId,
    pub title: String,
    pub relation: LinkRelation,
    pub outgoing: bool,
}
#[derive(Clone, Debug)]
pub struct CurationCommit {
    pub links: Vec<CurationLink>,
    pub operation_id: String,
    pub sources: Vec<super::SourceId>,
    pub promotes: Vec<super::SourceId>,
    pub merges: Vec<(MemoryId, RevisionId, String, String, Vec<super::SourceId>)>,
    /// Nodes in `merges` whose combined update changes the position, with its start time.
    pub supersedes: Vec<(MemoryId, u64)>,
    pub creates: Vec<(ParentRef, String, String, Vec<super::SourceId>)>,
    pub parked: Vec<(super::SourceId, ParkReason)>,
    pub stale: Vec<MemoryId>,
    pub split: Vec<MemoryId>,
    pub operation: LinkOperation,
}
/// Durable cross-store work; source content is read from the immutable original.
#[derive(Clone, Debug)]
pub struct Forward {
    pub forward_id: String,
    pub action: &'static str,
    pub source_id: super::SourceId,
    pub source: Option<super::capture::CaptureEvent>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeQueue {
    Stale,
    Split,
    Rebuild,
}

#[derive(Clone, Debug)]
pub struct MaintenanceCommit {
    pub operation_id: String,
    pub operation: LinkOperation,
    pub rewrites: Vec<(MemoryId, RevisionId, String, String, Vec<super::SourceId>)>,
    pub creates: Vec<(ParentRef, String, String, Vec<super::SourceId>)>,
    pub reparent: Vec<(MemoryId, usize)>,
    pub expected_reparent: Vec<(MemoryId, RevisionId)>,
    pub move_sources: Vec<(super::SourceId, MemoryId, usize)>,
    pub stale: Vec<MemoryId>,
    pub queue_item: Option<(TreeQueue, MemoryId)>,
}

#[derive(Clone, Debug)]
pub struct NodeSource {
    pub event: super::capture::CaptureEvent,
    pub operation: LinkOperation,
    pub revision: RevisionId,
    pub linked_at_ms: u64,
}
/// Live lesson provenance, derived from direct source links rather than persisted counters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LessonStats {
    pub sessions: Vec<String>,
    pub strategies: usize,
    pub pitfalls: usize,
}
/// Original lesson text used for deterministic placement after unusable curator operations.
#[derive(Clone, Debug)]
pub struct LessonPlacement {
    pub title: String,
    pub content: String,
}
#[derive(Clone, Debug)]
pub struct NodeView {
    pub replaced: Option<ReplacedPosition>,
    pub source_total: u32,
    pub skill_draft: Option<super::ui::SkillDraftRef>,
    pub derived_memories: Vec<super::ui::MemoryRef>,
    pub links: Vec<NodeLink>,
    pub lesson: Option<LessonStats>,
    pub node: TreeNode,
    pub path: Vec<String>,
    pub children: Vec<(MemoryId, String)>,
    pub history: Vec<NodeHistory>,
    pub sources: Vec<NodeSource>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NodeHistory {
    pub basis: Basis,
    pub confidence: Option<Confidence>,
    pub derived_from: Vec<RevisionId>,
    pub item: super::runtime::RuntimeRevision,
    pub valid_from_ms: Option<u64>,
    pub valid_until_ms: Option<u64>,
    pub valid_until_by: Option<RevisionId>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplacedPosition {
    pub title: String,
    pub valid_until_ms: u64,
}

#[derive(Clone, Debug)]
pub struct TreeHit {
    pub basis: Basis,
    pub confidence: Option<Confidence>,
    pub derived_from: Vec<RevisionId>,
    /// Current text with the deterministic validity note, dressed before delivery.
    pub content: String,
    pub valid_from_ms: Option<u64>,
    pub replaced: Option<ReplacedPosition>,
    pub root: TreeRoot,
    pub hit: super::runtime::RuntimeHit,
    pub path: Vec<String>,
    pub node: MemoryId,
    pub source_count: u32,
}
