//! Pure validation and projections for memory tree curation.
use crate::domain_models::memory_tree::*;
use crate::domain_models::runtime::RuntimeLimits;
use crate::engines::capture_policy::contains_secret;
use std::collections::{HashMap, HashSet};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeError {
    InvalidOutput,
    Bounds,
}
/// Which validation rule a curator answer broke; recorded as the activity reason.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TreeRule {
    UnknownSlot,
    MissingOperation,
    TooManyOperations,
    InvalidText,
    SecretInText,
    Bounds,
}
impl TreeRule {
    pub fn error(self) -> TreeError {
        if self == Self::Bounds {
            TreeError::Bounds
        } else {
            TreeError::InvalidOutput
        }
    }
}
fn text_rule(title: &str, content: &str) -> Result<(), TreeRule> {
    if contains_secret(title) || contains_secret(content) {
        return Err(TreeRule::SecretInText);
    }
    validate_title(title)
        .and_then(|()| validate_content(content))
        .map_err(|_| TreeRule::InvalidText)
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidatedCuration {
    pub per_source: Vec<Vec<CurationOp>>,
    pub parked: Vec<usize>,
    pub split_requests: Vec<usize>,
    pub root_split_requests: Vec<TreeRoot>,
    /// Single operations dropped as unusable (empty or secret-bearing text, depth bound).
    pub dropped: usize,
}

pub fn validate_title(title: &str) -> Result<(), TreeError> {
    if title.trim().is_empty()
        || title.chars().count() > 80
        || title.chars().any(char::is_control)
        || contains_secret(title)
    {
        Err(TreeError::InvalidOutput)
    } else {
        Ok(())
    }
}
pub fn validate_content(content: &str) -> Result<(), TreeError> {
    if content.trim().is_empty() || content.len() > 1200 || contains_secret(content) {
        Err(TreeError::InvalidOutput)
    } else {
        Ok(())
    }
}
/// Operations kept per source; the curator is asked for one to five.
pub const MAX_OPERATIONS: usize = 5;
pub fn validate_curation(
    view: &CurationView,
    answer: &CurationAnswer,
    sources: usize,
    limits: &RuntimeLimits,
) -> Result<ValidatedCuration, TreeError> {
    validate_curation_rules(view, answer, sources, limits).map_err(TreeRule::error)
}
/// Like `validate_curation`, but says which rule failed. Only a structural mismatch (one answer
/// per source) or an unknown slot rejects the whole answer. A single unusable operation (empty
/// or secret-bearing text, a create beyond the depth bound) is dropped and counted, and a source
/// left with no usable operation counts as discarded. Several merges into one node are allowed;
/// the commit keeps the last text.
pub fn validate_curation_rules(
    view: &CurationView,
    answer: &CurationAnswer,
    sources: usize,
    limits: &RuntimeLimits,
) -> Result<ValidatedCuration, TreeRule> {
    if answer.per_source.len() != sources {
        return Err(TreeRule::MissingOperation);
    }
    let nodes: Vec<_> = view.map.iter().chain(&view.detailed).collect();
    let mut created = HashMap::new();
    let mut root_created = HashMap::new();
    let mut parked = Vec::new();
    let mut root_split_requests = Vec::new();
    let mut split_requests = Vec::new();
    let mut per_source = Vec::with_capacity(sources);
    let mut dropped = 0usize;
    for (source, ops) in answer.per_source.iter().enumerate() {
        let mut kept = Vec::new();
        for op in ops.iter().take(MAX_OPERATIONS) {
            match op {
                CurationOp::Discard => kept.push(op.clone()),
                CurationOp::Merge {
                    node,
                    title,
                    content,
                } => {
                    nodes.get(*node).ok_or(TreeRule::UnknownSlot)?;
                    // A merge into a node seen only by title waits one cycle, like an expand.
                    if *node < view.map.len() && view.expandable.contains(node) {
                        if !parked.contains(&source) {
                            parked.push(source);
                        }
                        continue;
                    }
                    if text_rule(title, content).is_err() {
                        dropped += 1;
                        continue;
                    }
                    kept.push(op.clone());
                }
                CurationOp::Create {
                    parent,
                    title,
                    content,
                } => {
                    if text_rule(title, content).is_err() {
                        dropped += 1;
                        continue;
                    }
                    match parent {
                        Slot::Root(root) => {
                            if limits.tree_max_depth == 0 {
                                dropped += 1;
                                continue;
                            }
                            let count = root_created.entry(*root).or_insert_with(|| {
                                view.root_children
                                    .iter()
                                    .find(|(r, _)| r == root)
                                    .map_or(0, |(_, count)| *count as usize)
                            });
                            if *count >= limits.tree_max_children.saturating_mul(4) {
                                if !parked.contains(&source) {
                                    parked.push(source);
                                }
                                if !root_split_requests.contains(root) {
                                    root_split_requests.push(*root);
                                }
                                continue;
                            }
                            *count += 1;
                        }
                        Slot::Node(slot) => {
                            let target = nodes.get(*slot).ok_or(TreeRule::UnknownSlot)?;
                            if target.depth >= limits.tree_max_depth {
                                dropped += 1;
                                continue;
                            }
                            let count = created
                                .entry(&target.memory)
                                .or_insert(target.children as usize);
                            if *count >= limits.tree_max_children {
                                if !parked.contains(&source) {
                                    parked.push(source);
                                }
                                if !split_requests.contains(slot) {
                                    split_requests.push(*slot);
                                }
                                continue;
                            }
                            *count += 1;
                        }
                    }
                    kept.push(op.clone());
                }
                CurationOp::Expand { node } => {
                    // Any expand of a known node just makes the source wait one cycle.
                    nodes.get(*node).ok_or(TreeRule::UnknownSlot)?;
                    if !parked.contains(&source) {
                        parked.push(source);
                    }
                }
            }
        }
        if kept.is_empty() {
            kept.push(CurationOp::Discard);
        }
        per_source.push(kept);
    }
    for &source in &parked {
        per_source[source].clear();
    }
    Ok(ValidatedCuration {
        per_source,
        parked,
        split_requests,
        root_split_requests,
        dropped,
    })
}
/// The map never exceeds this many nodes, so provider node slots stay bounded.
pub const TREE_MAP_MAX_NODES: usize = 96;
pub fn tree_map(nodes: &[TreeNode], max_bytes: usize) -> Vec<TreeNode> {
    let mut sorted: Vec<_> = nodes
        .iter()
        .filter(|n| (1..=2).contains(&n.depth))
        .collect();
    sorted.sort_by(|a, b| {
        b.last_linked_ms
            .cmp(&a.last_linked_ms)
            .then_with(|| b.direct_sources.cmp(&a.direct_sources))
    });
    let mut used = 0usize;
    sorted
        .into_iter()
        .take(TREE_MAP_MAX_NODES)
        .take_while(|n| {
            let size = n.title.len().saturating_add(16);
            if size > max_bytes.saturating_sub(used) {
                false
            } else {
                used += size;
                true
            }
        })
        .cloned()
        .collect()
}
pub fn source_terms(text: &str) -> Vec<String> {
    const STOP: &[&str] = &[
        "para", "como", "mais", "menos", "pelo", "pela", "pelos", "pelas", "este", "esta", "isso",
        "esse", "essa", "aquele", "aquela", "entre", "sobre", "quando", "onde", "qual", "quais",
        "cada", "todo", "toda", "todos", "todas", "muito", "muita", "vamos", "with", "from",
        "that", "this", "these", "those", "there", "their", "they", "them", "then", "than", "have",
        "were", "been", "being", "will", "would", "could", "should", "your", "ours", "into",
        "onto", "over", "under", "about", "after", "before", "while", "where", "when", "what",
        "which", "some", "such", "only", "also", "each", "both", "through", "because",
    ];
    let mut terms = Vec::new();
    for word in text.split(|c: char| !c.is_alphanumeric()) {
        let word = word.to_lowercase();
        if word.chars().count() >= 4 && !STOP.contains(&word.as_str()) && !terms.contains(&word) {
            terms.push(word);
            if terms.len() == 12 {
                break;
            }
        }
    }
    terms
}
pub fn root_index(children: &[TreeNode]) -> String {
    let mut titles: Vec<_> = children.iter().map(|n| n.title.as_str()).collect();
    titles.sort();
    let mut result = String::new();
    for title in titles {
        let extra = 2 + title.len() + usize::from(!result.is_empty());
        if extra > 1200 - result.len() {
            break;
        }
        if !result.is_empty() {
            result.push('\n');
        }
        result.push_str("- ");
        result.push_str(title);
    }
    result
}
pub fn validate_split(
    node: &TreeNode,
    sources: usize,
    answer: &SplitAnswer,
    limits: &RuntimeLimits,
) -> Result<(), TreeError> {
    validate_title(&answer.node_title)?;
    validate_content(&answer.node_content)?;
    if !(2..=4).contains(&answer.children.len()) {
        return Err(TreeError::InvalidOutput);
    }
    if node.depth >= limits.tree_max_depth
        || (node.children as usize).saturating_add(answer.children.len()) > limits.tree_max_children
    {
        return Err(TreeError::Bounds);
    }
    let mut seen = HashSet::new();
    for (title, content, indexes) in &answer.children {
        validate_title(title)?;
        validate_content(content)?;
        for &index in indexes {
            if index >= sources || !seen.insert(index) {
                return Err(TreeError::InvalidOutput);
            }
        }
    }
    if seen.len() != sources {
        return Err(TreeError::InvalidOutput);
    }
    Ok(())
}

/// Validate a partition of the root's existing children into new children.
pub fn validate_root_split(
    children: usize,
    answer: &SplitAnswer,
    limits: &RuntimeLimits,
) -> Result<(), TreeError> {
    validate_title(&answer.node_title)?;
    validate_content(&answer.node_content)?;
    if !(2..=4).contains(&answer.children.len()) {
        return Err(TreeError::InvalidOutput);
    }
    let mut seen = HashSet::new();
    for (title, content, indexes) in &answer.children {
        validate_title(title)?;
        validate_content(content)?;
        if indexes.len() > limits.tree_max_children {
            return Err(TreeError::Bounds);
        }
        for &index in indexes {
            if index >= children || !seen.insert(index) {
                return Err(TreeError::InvalidOutput);
            }
        }
    }
    if seen.len() != children {
        return Err(TreeError::InvalidOutput);
    }
    Ok(())
}
