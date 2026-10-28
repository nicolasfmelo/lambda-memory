//! Domain vocabulary for curated memory trees.
use super::{MemoryId, RevisionId};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TreeRoot {
    Decisions,
    Preferences,
    Architecture,
    OpenItems,
}
impl TreeRoot {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Decisions => "decisions",
            Self::Preferences => "preferences",
            Self::Architecture => "architecture",
            Self::OpenItems => "open_items",
        }
    }
    pub fn memory_id(&self) -> MemoryId {
        MemoryId::new(match self {
            Self::Decisions => "tree-root:decisions",
            Self::Preferences => "tree-root:preferences",
            Self::Architecture => "tree-root:architecture",
            Self::OpenItems => "tree-root:open-items",
        })
        .expect("fixed root identity")
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeMode {
    Flat,
    Tree,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TreeNode {
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
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Slot {
    Node(usize),
    Root(TreeRoot),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CurationOp {
    Discard,
    Merge {
        node: usize,
        title: String,
        content: String,
    },
    Create {
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
    Create,
    Merge,
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
pub struct CurationCommit {
    pub operation_id: String,
    pub sources: Vec<super::SourceId>,
    pub merges: Vec<(MemoryId, RevisionId, String, String, Vec<super::SourceId>)>,
    pub creates: Vec<(ParentRef, String, String, Vec<super::SourceId>)>,
    pub parked: Vec<(super::SourceId, ParkReason)>,
    pub stale: Vec<MemoryId>,
    pub split: Vec<MemoryId>,
    pub operation: LinkOperation,
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
#[derive(Clone, Debug)]
pub struct NodeView {
    pub node: TreeNode,
    pub path: Vec<String>,
    pub children: Vec<(MemoryId, String)>,
    pub history: Vec<super::runtime::RuntimeRevision>,
    pub sources: Vec<NodeSource>,
    pub next_cursor: Option<String>,
}

#[derive(Clone, Debug)]
pub struct TreeHit {
    pub hit: super::runtime::RuntimeHit,
    pub path: Vec<String>,
    pub node: MemoryId,
    pub source_count: u32,
}
