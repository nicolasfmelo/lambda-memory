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
    InvalidPromotion,
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
    /// Parking reasons derived only from operations that survived placement validation.
    pub park_reasons: Vec<(usize, ParkReason)>,
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
    validate_curation_rules_for_kind(StoreKind::Project, view, answer, sources, limits)
}

/// Apply the store's parent-root policy, dropping out-of-kind creates individually.
pub fn validate_curation_rules_for_kind(
    kind: StoreKind,
    view: &CurationView,
    answer: &CurationAnswer,
    sources: usize,
    limits: &RuntimeLimits,
) -> Result<ValidatedCuration, TreeRule> {
    validate_curation_sources(kind, view, answer, &vec![None; sources], limits)
}

/// Reserve one root child for either an ordinary create or a deterministic lesson fallback.
fn root_create_has_room(
    root: TreeRoot,
    view: &CurationView,
    limits: &RuntimeLimits,
    created: &mut HashMap<TreeRoot, usize>,
    split_requests: &mut Vec<TreeRoot>,
) -> bool {
    let count = created.entry(root).or_insert_with(|| {
        view.root_children
            .iter()
            .find(|(r, _)| *r == root)
            .map_or(0, |(_, count)| *count as usize)
    });
    if *count >= limits.tree_max_children.saturating_mul(4) {
        if !split_requests.contains(&root) {
            split_requests.push(root);
        }
        return false;
    }
    *count += 1;
    true
}

/// Source-aware placement. Lesson fallbacks are decoded from immutable evidence at the boundary.
pub fn validate_curation_sources(
    kind: StoreKind,
    view: &CurationView,
    answer: &CurationAnswer,
    lessons: &[Option<LessonPlacement>],
    limits: &RuntimeLimits,
) -> Result<ValidatedCuration, TreeRule> {
    if answer.per_source.len() != lessons.len() {
        return Err(TreeRule::MissingOperation);
    }
    let lesson_root = if kind == StoreKind::User {
        TreeRoot::GeneralLessons
    } else {
        TreeRoot::Lessons
    };
    let nodes: Vec<_> = view.map.iter().chain(&view.detailed).collect();
    let mut created = HashMap::new();
    let mut root_created = HashMap::new();
    let mut parked = Vec::new();
    let mut expands = HashMap::new();
    let mut root_split_requests = Vec::new();
    let mut split_requests = Vec::new();
    let mut per_source = Vec::with_capacity(lessons.len());
    let mut dropped = 0usize;
    for (source, ops) in answer.per_source.iter().enumerate() {
        // Check all operations before truncation so an out-of-kind or duplicate promotion
        // cannot be hidden beyond the usable operation budget.
        let lesson = lessons[source].as_ref();
        let promotes = ops
            .iter()
            .filter(|op| matches!(op, CurationOp::Promote))
            .count();
        if lesson.is_none() && (promotes > 1 || (kind == StoreKind::User && promotes != 0)) {
            return Err(TreeRule::InvalidPromotion);
        }
        let mut kept = Vec::new();
        for op in ops.iter().take(MAX_OPERATIONS) {
            // Validate targets before parking, counting or requesting maintenance: an invalid
            // placement must not suppress another usable operation for the same source.
            let target_root = match op {
                CurationOp::Merge { node, .. } | CurationOp::Supersede { node, .. } => {
                    Some(nodes.get(*node).ok_or(TreeRule::UnknownSlot)?.root)
                }
                CurationOp::Create {
                    parent: Slot::Node(node),
                    ..
                } => Some(nodes.get(*node).ok_or(TreeRule::UnknownSlot)?.root),
                CurationOp::Create {
                    parent: Slot::Root(root),
                    ..
                } => Some(*root),
                _ => None,
            };
            if (lesson.is_some()
                && (matches!(op, CurationOp::Promote)
                    || target_root.is_some_and(|root| root != lesson_root)))
                || (lesson.is_none()
                    && kind == StoreKind::Project
                    && target_root == Some(TreeRoot::Lessons))
            {
                dropped += 1;
                continue;
            }
            match op {
                CurationOp::Discard | CurationOp::Promote => kept.push(op.clone()),
                CurationOp::Merge {
                    node,
                    title,
                    content,
                    ..
                }
                | CurationOp::Supersede {
                    node,
                    title,
                    content,
                    ..
                } => {
                    nodes.get(*node).ok_or(TreeRule::UnknownSlot)?;
                    // A merge into a node seen only by title waits one cycle, like an expand.
                    if *node < view.map.len() && view.expandable.contains(node) {
                        expands
                            .entry(source)
                            .or_insert_with(|| nodes[*node].memory.clone());
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
                    ..
                } => {
                    if text_rule(title, content).is_err() {
                        dropped += 1;
                        continue;
                    }
                    match parent {
                        Slot::Root(root) => {
                            if !roots(kind).contains(root) || limits.tree_max_depth == 0 {
                                dropped += 1;
                                continue;
                            }
                            if !root_create_has_room(
                                *root,
                                view,
                                limits,
                                &mut root_created,
                                &mut root_split_requests,
                            ) {
                                if !parked.contains(&source) {
                                    parked.push(source);
                                }
                                continue;
                            }
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
                    let target = nodes.get(*node).ok_or(TreeRule::UnknownSlot)?;
                    expands
                        .entry(source)
                        .or_insert_with(|| target.memory.clone());
                    if !parked.contains(&source) {
                        parked.push(source);
                    }
                }
            }
        }
        if kept.is_empty() && !parked.contains(&source) {
            if let Some(lesson) = lesson {
                if root_create_has_room(
                    lesson_root,
                    view,
                    limits,
                    &mut root_created,
                    &mut root_split_requests,
                ) {
                    kept.push(CurationOp::Create {
                        links: vec![],
                        parent: Slot::Root(lesson_root),
                        title: lesson.title.clone(),
                        content: lesson.content.clone(),
                    });
                } else {
                    parked.push(source);
                }
            } else {
                kept.push(CurationOp::Discard);
            }
        }
        per_source.push(kept);
    }
    for &source in &parked {
        per_source[source].clear();
    }
    // Link errors never discard their operation. Resolve identity, not just slot: map and
    // detailed can contain the same node. Only operations that survived parking own links.
    let mut seen_links = HashSet::new();
    let mut create_index = 0usize;
    for op in per_source.iter_mut().flatten() {
        let supersede = matches!(op, CurationOp::Supersede { .. });
        let (from, forbidden, links) = match op {
            CurationOp::Merge { node, links, .. } | CurationOp::Supersede { node, links, .. } => (
                format!("node:{}", nodes[*node].memory),
                Some(&nodes[*node].memory),
                links,
            ),
            CurationOp::Create { parent, links, .. } => {
                create_index += 1;
                (
                    format!("create:{create_index}"),
                    match parent {
                        Slot::Node(i) => Some(&nodes[*i].memory),
                        _ => None,
                    },
                    links,
                )
            }
            _ => continue,
        };
        let mut kept = Vec::new();
        for link in links.drain(..) {
            let Some(to) = nodes.get(link.to) else {
                continue;
            };
            if forbidden == Some(&to.memory)
                || (link.relation == LinkRelation::Replaces && !supersede)
                || kept.len() == 3
            {
                continue;
            }
            if seen_links.insert((from.clone(), to.memory.clone(), link.relation)) {
                kept.push(link);
            }
        }
        *links = kept;
    }
    let park_reasons = parked
        .iter()
        .map(|&source| {
            (
                source,
                expands
                    .remove(&source)
                    .map_or(ParkReason::Split, ParkReason::Expand),
            )
        })
        .collect();
    Ok(ValidatedCuration {
        per_source,
        parked,
        park_reasons,
        split_requests,
        root_split_requests,
        dropped,
    })
}
const DAY_MS: u64 = 86_400_000;

/// Parse a source-stated calendar date; unusable dates never invalidate the operation.
pub fn valid_from_date(date: &str, recorded_at_ms: u64, current: Option<u64>) -> Option<u64> {
    let b = date.as_bytes();
    if b.len() != 10
        || b[4] != b'-'
        || b[7] != b'-'
        || b.iter()
            .enumerate()
            .any(|(i, c)| i != 4 && i != 7 && !c.is_ascii_digit())
    {
        return None;
    }
    let year: u64 = date[..4].parse().ok()?;
    let month: u64 = date[5..7].parse().ok()?;
    let day: u64 = date[8..].parse().ok()?;
    let leap = year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400));
    let days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    if year < 2000 || !(1..=12).contains(&month) || day == 0 || day > days[month as usize - 1] {
        return None;
    }
    let y = year - u64::from(month <= 2);
    let era = y / 400;
    let yoe = y % 400;
    let m = if month > 2 { month - 3 } else { month + 9 };
    let doy = (153 * m + 2) / 5 + day - 1;
    let epoch_days = era * 146097 + yoe * 365 + yoe / 4 - yoe / 100 + doy - 719468;
    let ms = epoch_days * DAY_MS;
    (ms <= recorded_at_ms.saturating_add(DAY_MS) && current.is_none_or(|start| ms >= start))
        .then_some(ms)
}

/// UTC calendar date without clock access or platform-dependent formatting.
pub fn validity_date(ms: u64) -> String {
    let z = ms / DAY_MS + 719468;
    let era = z / 146097;
    let doe = z % 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let mut year = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    year += u64::from(month <= 2);
    format!("{year:04}-{month:02}-{day:02}")
}

pub fn validity_note(valid_from_ms: Option<u64>, replaced: Option<&ReplacedPosition>) -> String {
    let Some(start) = valid_from_ms else {
        return String::new();
    };
    let since = validity_date(start);
    match replaced {
        Some(old) => format!(
            " (since {since}; replaced: {} (until {}))",
            old.title,
            validity_date(old.valid_until_ms)
        ),
        None => format!(" (since {since})"),
    }
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
/// A bounded, deterministic index of the direct children of each project root.
pub fn brief(
    project: crate::domain_models::ProjectId,
    mode: TreeMode,
    nodes: &[TreeNode],
) -> MemoryBrief {
    const NODES_PER_ROOT: usize = 12;
    let kind = StoreKind::of(&project);
    let mut result = MemoryBrief {
        project,
        mode,
        roots: Vec::new(),
        truncated: false,
        skill_drafts_pending: 0,
    };
    if mode == TreeMode::Flat {
        return result;
    }
    for &root in roots(kind) {
        let mut children: Vec<_> = nodes
            .iter()
            .filter(|node| node.root == root && node.depth == 1)
            .collect();
        children.sort_by(|a, b| {
            b.last_linked_ms
                .cmp(&a.last_linked_ms)
                .then_with(|| a.title.cmp(&b.title))
        });
        result.truncated |= children.len() > NODES_PER_ROOT;
        if !children.is_empty() {
            result.roots.push(BriefRoot {
                root,
                nodes: children
                    .into_iter()
                    .take(NODES_PER_ROOT)
                    .map(|node| BriefNode {
                        basis: node.basis,
                        node: node.memory.clone(),
                        title: node.title.clone(),
                        children: node.children,
                    })
                    .collect(),
            });
        }
    }
    result
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
