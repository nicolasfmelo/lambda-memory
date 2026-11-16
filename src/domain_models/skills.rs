//! Source-bound skill drafts; installation is owned by the operator CLI.
use super::memory_tree::TreeNode;
use super::{MemoryId, RevisionId};

pub const SKILL_MIN_SESSIONS: usize = 3;
pub const REJECT_COOLDOWN_SESSIONS: usize = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SkillStatus {
    Drafted,
    Approved,
    Rejected,
    Installed,
    Stale,
}
impl SkillStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Drafted => "drafted",
            Self::Approved => "approved",
            Self::Rejected => "rejected",
            Self::Installed => "installed",
            Self::Stale => "stale",
        }
    }
    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "drafted" => Self::Drafted,
            "approved" => Self::Approved,
            "rejected" => Self::Rejected,
            "installed" => Self::Installed,
            "stale" => Self::Stale,
            _ => return None,
        })
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SkillCandidate {
    pub candidate_id: MemoryId,
    pub scope: &'static str,
    pub memory_id: MemoryId,
    pub revision_id: RevisionId,
    pub slug: String,
    pub status: SkillStatus,
    pub draft: Option<String>,
    pub sessions_at_draft: usize,
    pub sessions_at_decision: Option<usize>,
    pub created_at_ms: u64,
    pub decided_at_ms: Option<u64>,
    pub install_paths: Vec<String>,
    pub install_hashes: Vec<String>,
    pub pitfall_memories: Vec<MemoryId>,
    pub pitfall_revisions: Vec<RevisionId>,
    pub pitfall_source_counts: Vec<u32>,
    pub installed_pitfall_memories: Vec<MemoryId>,
}
#[derive(Clone, Debug)]
pub struct SkillDraftInput {
    pub node: TreeNode,
    pub path: Vec<String>,
    pub pitfalls: Vec<TreeNode>,
    pub sessions: Vec<String>,
    pub scope: &'static str,
}
#[derive(Clone, Debug)]
pub struct SkillDraftAnswer {
    pub name: String,
    pub description: String,
    pub steps: Vec<String>,
    pub pitfalls: Vec<String>,
}
#[derive(Clone, Debug)]
pub struct SkillDecision {
    pub decision: SkillStatus,
    pub install_paths: Vec<String>,
    pub install_hashes: Vec<String>,
}
