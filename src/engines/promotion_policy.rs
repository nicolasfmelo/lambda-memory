//! Conservative, pure selection and validation for the operator classifier.
use crate::domain_models::memory_tree::{StoreKind, TreeNode, TreeRoot};
use crate::domain_models::promotion::PromotionVerdict;
use crate::domain_models::runtime::RuntimeError;
use crate::domain_models::{ProjectId, RevisionId};

pub const PROMPT_VERSION: &str = "runtime_promotion_v1";
pub const BATCH_SIZE: usize = 16;

pub fn candidates(
    project: &ProjectId,
    nodes: &[TreeNode],
    reviewed: &[RevisionId],
) -> Result<Vec<TreeNode>, RuntimeError> {
    if StoreKind::of(project) != StoreKind::Project {
        return Err(RuntimeError::InvalidInput(
            "promotion requires a project store",
        ));
    }
    let mut selected: Vec<_> = nodes
        .iter()
        .filter(|n| {
            n.basis == crate::domain_models::memory_tree::Basis::Observed
                && n.children == 0
                && matches!(n.root, TreeRoot::Preferences | TreeRoot::Decisions)
                && !reviewed.contains(&n.revision)
        })
        .cloned()
        .collect();
    selected.sort_by(|a, b| {
        (a.root != TreeRoot::Preferences)
            .cmp(&(b.root != TreeRoot::Preferences))
            .then_with(|| b.last_linked_ms.cmp(&a.last_linked_ms))
            .then_with(|| a.memory.cmp(&b.memory))
    });
    Ok(selected)
}

/// The boundary supplies explicit slots; duplicates, omissions and unknown slots fail closed.
pub fn validate_output(
    entries: &[(usize, PromotionVerdict)],
    count: usize,
) -> Result<Vec<PromotionVerdict>, RuntimeError> {
    let invalid = || RuntimeError::ProviderInvalidOutput;
    if count == 0 || count > BATCH_SIZE || entries.len() != count {
        return Err(invalid());
    }
    let mut slots = vec![None; count];
    for &(slot, verdict) in entries {
        let target = slots.get_mut(slot).ok_or_else(invalid)?;
        if target.replace(verdict).is_some() {
            return Err(invalid());
        }
    }
    slots.into_iter().map(|s| s.ok_or_else(invalid)).collect()
}
