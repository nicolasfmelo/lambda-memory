//! Operator promotion migration vocabulary, independent of wire and storage formats.
use super::capture::CaptureEvent;
use super::memory_tree::TreeNode;
use super::{MemoryId, RevisionId, SourceId};

#[derive(Clone, Debug)]
pub struct PromotionCandidate {
    pub node: TreeNode,
    pub path: Vec<String>,
    pub sources: Vec<CaptureEvent>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromotionPlan {
    pub nodes: Vec<MemoryId>,
    pub batches: usize,
    pub linked_sources: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromotionVerdict {
    User,
    Project,
}
impl PromotionVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Project => "project",
        }
    }
}
#[derive(Debug, Default)]
pub struct PromotionReport {
    pub classified: usize,
    pub user_nodes: Vec<MemoryId>,
    pub forwards: usize,
}
#[derive(Clone, Debug)]
pub struct PromotionReview {
    pub memory: MemoryId,
    pub revision: RevisionId,
    pub verdict: PromotionVerdict,
    pub decided_at_ms: u64,
    pub retired_at_ms: Option<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PromotionEligible {
    pub node: MemoryId,
    pub sources: Vec<SourceId>,
}
#[derive(Debug, Default)]
pub struct PromotionRetirement {
    pub eligible: Vec<PromotionEligible>,
    pub blocked: Vec<MemoryId>,
    pub retired: usize,
}
#[derive(Debug, Default)]
pub struct CopyInspection {
    pub active: Vec<SourceId>,
    pub consumed: Vec<SourceId>,
    pub pending: Vec<SourceId>,
    pub missing: Vec<SourceId>,
}
