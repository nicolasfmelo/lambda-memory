//! Pure decision-timeline input bounds. Existence and versioning are checked by the store.
use crate::domain_models::runtime::RuntimeError;
use crate::domain_models::timeline::*;

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
