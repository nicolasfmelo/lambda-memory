//! Pure decision-timeline input bounds. Existence and versioning are checked by the store.
use crate::domain_models::runtime::RuntimeError;
use crate::domain_models::timeline::*;

pub const AUTO_POLICY: TimelineAutoPolicy = TimelineAutoPolicy {
    drafts: auto_drafts,
    validate: validate_draft,
};

fn unique<T: PartialEq>(items: &[T]) -> bool {
    items
        .iter()
        .enumerate()
        .all(|(i, item)| !items[..i].contains(item))
}

/// `^[a-z0-9][a-z0-9._-]{0,63}$`, checked by hand.
fn valid_tag(tag: &str) -> bool {
    let bytes = tag.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= TIMELINE_MAX_TAG_BYTES
        && matches!(bytes[0], b'a'..=b'z' | b'0'..=b'9')
        && bytes[1..]
            .iter()
            .all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'.' | b'_' | b'-'))
}

pub fn validate_draft(
    draft: &TimelineDraft,
    now_ms: u64,
    max_content_bytes: usize,
) -> Result<(), RuntimeError> {
    // A whitespace-only title carries no meaning, so it counts as empty.
    if draft.title.trim().is_empty()
        || draft.title.len() > TIMELINE_MAX_TITLE_BYTES
        || draft.title.chars().any(char::is_control)
    {
        return Err(RuntimeError::InvalidInput("timeline title"));
    }
    if draft.summary.trim().is_empty() || draft.summary.len() > max_content_bytes {
        return Err(RuntimeError::InvalidInput("timeline summary"));
    }
    if draft.occurred_at_ms < TIMELINE_MIN_OCCURRED_MS
        || draft.occurred_at_ms > now_ms.saturating_add(TIMELINE_FUTURE_SKEW_MS)
    {
        return Err(RuntimeError::InvalidInput("timeline occurred_at_ms"));
    }
    if draft.sources.is_empty()
        || draft.sources.len() > TIMELINE_MAX_LINKS
        || !unique(&draft.sources)
    {
        return Err(RuntimeError::InvalidInput("timeline sources"));
    }
    if draft.revisions.len() > TIMELINE_MAX_LINKS || !unique(&draft.revisions) {
        return Err(RuntimeError::InvalidInput("timeline revisions"));
    }
    if draft.memories.len() > TIMELINE_MAX_LINKS || !unique(&draft.memories) {
        return Err(RuntimeError::InvalidInput("timeline memories"));
    }
    if draft.tags.len() > TIMELINE_MAX_TAGS
        || !unique(&draft.tags)
        || !draft.tags.iter().all(|tag| valid_tag(tag))
    {
        return Err(RuntimeError::InvalidInput("timeline tags"));
    }
    if draft.supersedes.as_ref() == Some(&draft.entry) {
        return Err(RuntimeError::InvalidInput(
            "timeline entry cannot supersede itself",
        ));
    }
    if draft.expected_version == Some(0) {
        return Err(RuntimeError::InvalidInput("timeline expected_version"));
    }
    Ok(())
}

/// Deterministic byte-bounded summary, preferring the last complete sentence, then word.
pub fn auto_summary(content: &str, limit: usize) -> String {
    let content = content.trim();
    if content.len() <= limit {
        return content.to_owned();
    }
    let mut end = limit.min(content.len());
    while !content.is_char_boundary(end) {
        end -= 1;
    }
    let prefix = &content[..end];
    let sentence = prefix
        .char_indices()
        .filter(|(_, c)| matches!(c, '.' | '!' | '?'))
        .map(|(i, c)| i + c.len_utf8())
        .next_back();
    let word = prefix
        .char_indices()
        .filter(|(_, c)| c.is_whitespace())
        .map(|(i, _)| i)
        .next_back();
    prefix[..sentence.or(word).filter(|i| *i > 0).unwrap_or(end)]
        .trim()
        .to_owned()
}

/// Only curator create/migrate, supersede and merge call this policy. Maintenance never does.
/// Every position includes its unique revision ID, even after earlier positions are purged.
pub fn auto_drafts(
    project: &crate::domain_models::ProjectId,
    node: &crate::domain_models::memory_tree::TreeNode,
    operation: crate::domain_models::memory_tree::LinkOperation,
    sources: &[crate::domain_models::capture::CaptureEvent],
    current: Option<&TimelineEntry>,
    now_ms: u64,
    summary_limit: usize,
) -> Result<Vec<TimelineDraft>, RuntimeError> {
    use crate::domain_models::memory_tree::{LinkOperation, TreeRoot};
    if node.root != TreeRoot::Decisions {
        return Ok(vec![]);
    }
    let current = current.filter(|e| {
        e.is_auto() && e.status == TimelineStatus::Adopted && e.memories == [node.memory.clone()]
    });
    if !matches!(
        operation,
        LinkOperation::Create
            | LinkOperation::Migrate
            | LinkOperation::Supersede
            | LinkOperation::Merge
    ) || (current.is_none() && (node.children != 0 || operation == LinkOperation::Merge))
    {
        return Ok(vec![]);
    }
    let mut ordered = sources.to_vec();
    ordered.sort_by(|a, b| {
        b.recorded_at_ms
            .cmp(&a.recorded_at_ms)
            .then_with(|| b.source.as_str().cmp(a.source.as_str()))
    });
    let mut ids = Vec::new();
    for event in &ordered {
        if !ids.contains(&event.source) {
            ids.push(event.source.clone());
        }
        if ids.len() == TIMELINE_MAX_LINKS {
            break;
        }
    }
    let occurred_at_ms = node
        .valid_from_ms
        .or_else(|| ordered.first().map(|source| source.recorded_at_ms))
        .unwrap_or(now_ms)
        .max(TIMELINE_MIN_OCCURRED_MS)
        .min(
            now_ms
                .saturating_add(TIMELINE_FUTURE_SKEW_MS)
                .max(TIMELINE_MIN_OCCURRED_MS),
        );
    let mut changes = Vec::new();
    let mut draft = if operation == LinkOperation::Merge {
        let Some(current) = current else {
            return Ok(vec![]);
        };
        current.draft()
    } else {
        let supersedes = if operation == LinkOperation::Supersede {
            current.map(|entry| {
                let mut closed = entry.draft();
                closed.status = TimelineStatus::Superseded;
                changes.push(closed);
                entry.entry.clone()
            })
        } else {
            None
        };
        let entry = format!("auto:{}:{}", node.memory, node.revision);
        TimelineDraft {
            project: project.clone(),
            entry: TimelineEntryId::new(entry)
                .map_err(|_| RuntimeError::InvalidInput("auto timeline ID"))?,
            expected_version: None,
            title: String::new(),
            summary: String::new(),
            occurred_at_ms,
            status: TimelineStatus::Adopted,
            sources: vec![],
            revisions: vec![],
            memories: vec![node.memory.clone()],
            supersedes,
            tags: vec![TIMELINE_AUTO_TAG.into()],
        }
    };
    draft.occurred_at_ms = occurred_at_ms;
    draft.title = node.title.clone();
    draft.summary = auto_summary(&node.content, summary_limit);
    draft.revisions = vec![node.revision.clone()];
    draft.sources = ids;
    changes.push(draft);
    Ok(changes)
}
