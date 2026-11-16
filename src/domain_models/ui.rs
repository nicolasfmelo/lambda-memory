//! Model-free projections for the local dashboard.
use super::memory_tree::{Basis, LessonStats, ReplacedPosition, TreeMode, TreeNode, TreeRoot};
use super::skills::SkillStatus;
use super::{MemoryId, ProjectId};

#[derive(Clone, Debug)]
pub struct SkillDraftRef {
    pub candidate: MemoryId,
    pub status: SkillStatus,
}
#[derive(Clone, Debug)]
pub struct MemoryRef {
    pub node: MemoryId,
    pub title: String,
}
#[derive(Clone, Debug)]
pub struct OverviewRoot {
    pub root: TreeRoot,
    pub nodes: usize,
    pub latest: Option<MemoryRef>,
}
#[derive(Clone, Debug)]
pub struct OverviewDay {
    pub day: String,
    pub captured: usize,
    pub published: usize,
}
#[derive(Clone, Debug)]
pub struct MemoryOverview {
    pub project: ProjectId,
    pub mode: TreeMode,
    pub roots: Vec<OverviewRoot>,
    pub flat_active: usize,
    pub observed: usize,
    pub concluded: usize,
    pub lessons: usize,
    pub reinforced: usize,
    pub skill_drafts: Vec<(SkillStatus, usize)>,
    pub days: Vec<OverviewDay>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NodesSort {
    Recent,
    Sources,
}
#[derive(Clone, Debug)]
pub struct NodesQuery {
    pub project: ProjectId,
    pub root: TreeRoot,
    pub topic: Option<MemoryId>,
    pub basis: Option<Basis>,
    pub updated: bool,
    pub query: String,
    pub sort: NodesSort,
    pub after: Option<(u64, MemoryId)>,
    pub limit: usize,
}
#[derive(Clone, Debug)]
pub struct NodeTopic {
    pub memory: MemoryRef,
    pub count: usize,
}
#[derive(Clone, Debug)]
pub struct UiNode {
    pub node: TreeNode,
    pub summary: String,
    pub path: Vec<String>,
    pub topic: Option<MemoryRef>,
    pub replaced: Option<ReplacedPosition>,
    pub link_count: usize,
    pub lesson: Option<LessonStats>,
    pub skill_draft: Option<SkillDraftRef>,
}
#[derive(Clone, Debug)]
pub struct NodesPage {
    pub total: usize,
    pub topics: Vec<NodeTopic>,
    pub nodes: Vec<UiNode>,
    pub more: bool,
}
