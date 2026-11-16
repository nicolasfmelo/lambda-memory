//! Source-bound branch reflection vocabulary; slots are local to one immutable snapshot.
pub const INPUT_BYTES: usize = 48 * 1024;
#[derive(Clone, Copy)]
pub struct ReflectionPolicy {
    pub due: fn(u64, i64, &[MemoryId], &[MemoryId]) -> bool,
    pub bound: fn(ReflectionInput) -> ReflectionInput,
    pub validate: fn(&ReflectionInput, &ReflectionAnswer) -> bool,
    pub normalize: fn(&ReflectionInput, ReflectionAnswer) -> ReflectionAnswer,
}
pub fn in_branch(nodes: &[TreeNode], memory: &MemoryId, branch: &MemoryId) -> bool {
    let mut current = memory;
    for _ in 0..=32 {
        if current == branch {
            return true;
        }
        let Some(parent) = nodes
            .iter()
            .find(|n| &n.memory == current)
            .and_then(|n| n.parent.as_ref())
        else {
            break;
        };
        current = parent;
    }
    false
}
use super::{
    MemoryId,
    memory_tree::{Confidence, TreeNode},
};
#[derive(Clone, Debug)]
pub struct ReflectionNode {
    pub node: TreeNode,
    pub path: Vec<String>,
    pub inside: bool,
}
#[derive(Clone, Debug)]
pub struct ReflectionInput {
    pub branch: MemoryId,
    pub nodes: Vec<ReflectionNode>,
}
#[derive(Clone, Debug)]
pub struct Conclusion {
    pub title: String,
    pub content: String,
    pub confidence: Confidence,
    pub derived_from: Vec<usize>,
    pub parent: usize,
}
#[derive(Clone, Debug, Default)]
pub struct ReflectionAnswer {
    pub conclusions: Vec<Conclusion>,
    pub confidence_updates: Vec<(usize, Confidence)>,
    pub contradictions: Vec<(usize, usize)>,
}
#[derive(Clone, Debug)]
pub struct ReflectionWork {
    pub input: ReflectionInput,
    pub mature_lessons: Vec<MemoryId>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ReflectionBackfillPlan {
    pub branches: Vec<MemoryId>,
}
