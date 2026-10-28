use crate::domain_models::OwnerId;
use crate::domain_models::capture::*;
use crate::domain_models::procedural::ActionEpisode;

pub fn validate(event: &CaptureEvent, limits: &CaptureLimits) -> Result<(), CaptureError> {
    if limits.max_pending_per_owner == 0
        || limits.max_content_bytes == 0
        || event.content.trim().is_empty()
        || event.content.len() > limits.max_content_bytes
        || event.origin.trim().is_empty()
        || event.origin.len() > 2048
        || event.origin.chars().any(char::is_control)
    {
        return Err(CaptureError::InvalidEvidence);
    }
    Ok(())
}
/// Caller re-evaluates this gate between batches; no clock or scheduler here.
pub fn dreaming_batch(
    owner: &OwnerId,
    now_ms: u64,
    activity: &ActivitySnapshot,
    policy: &DreamingPolicy,
    pending: &[PendingEvidence],
) -> Option<DreamingBatch> {
    let last = activity.last_capture_ms.max(activity.last_recall_ms);
    if activity.capture_in_flight
        || activity.recall_in_flight
        || policy.max_batch == 0
        || policy.idle_for_ms == 0
        || now_ms < last
        || now_ms - last < policy.idle_for_ms
    {
        return None;
    }
    let mut eligible: Vec<_> = pending
        .iter()
        .filter(|item| {
            item.provisional && &item.event.owner == owner && item.event.recorded_at_ms <= now_ms
        })
        .collect();
    eligible.sort_by(|a, b| {
        (a.event.recorded_at_ms, &a.event.source).cmp(&(b.event.recorded_at_ms, &b.event.source))
    });
    let mut sources = Vec::new();
    for item in eligible {
        if !sources.contains(&item.event.source) {
            sources.push(item.event.source.clone());
        }
        if sources.len() == policy.max_batch {
            break;
        }
    }
    if sources.is_empty() {
        None
    } else {
        Some(DreamingBatch {
            owner: owner.clone(),
            sources,
            watermark_ms: now_ms,
        })
    }
}

const KEY_PREFIXES: [&str; 10] = [
    "sk-",
    "ghp_",
    "gho_",
    "github_pat_",
    "glpat-",
    "xoxa-",
    "xoxb-",
    "xoxp-",
    "AKIA",
    "AIza",
];
const PREFIX_MIN_TAIL: usize = 4;
const BEARER_MIN_TAIL: usize = 8;

/// Structural bounds of a sanitized action episode; the secret scan is `contains_secret`.
pub fn validate_action(episode: &ActionEpisode) -> Result<(), CaptureError> {
    let tool = episode.tool.as_bytes();
    let tool_ok = !tool.is_empty()
        && tool.len() <= 128
        && tool[0].is_ascii_alphabetic()
        && tool
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'));
    let session_ok = episode.session.len() == 16
        && episode
            .session
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    let head_ok = episode
        .error_head
        .as_ref()
        .is_none_or(|head| head.chars().count() <= 200);
    if !tool_ok
        || !session_ok
        || !head_ok
        || episode.intent.chars().count() > 160
        || episode.summary.chars().count() > 200
    {
        return Err(CaptureError::InvalidEvidence);
    }
    Ok(())
}
/// High-confidence secret patterns only: false negatives are the sanitizer's job, this is the last gate.
pub fn contains_secret(text: &str) -> bool {
    let token = |c: char| c.is_ascii_alphanumeric() || c == '_' || c == '-';
    for prefix in KEY_PREFIXES {
        for (at, _) in text.match_indices(prefix) {
            let boundary = text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| !c.is_alphanumeric());
            let tail = text[at + prefix.len()..]
                .chars()
                .take_while(|c| token(*c))
                .count();
            if boundary && tail >= PREFIX_MIN_TAIL {
                return true;
            }
        }
    }
    for (at, _) in text.match_indices("Bearer ") {
        let tail = text[at + "Bearer ".len()..]
            .chars()
            .take_while(|c| !c.is_whitespace())
            .count();
        if tail >= BEARER_MIN_TAIL {
            return true;
        }
    }
    for (at, _) in text.match_indices("://") {
        let rest = &text[at + 3..];
        let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
        if let Some((userinfo, _)) = authority.split_once('@')
            && !userinfo.chars().any(char::is_whitespace)
            && let Some((user, pass)) = userinfo.split_once(':')
            && !user.is_empty()
            && !pass.is_empty()
        {
            return true;
        }
    }
    false
}

/// Reject secret patterns in parsed conversation fields.
pub fn validate_exchange(prompt: &str, reply: &str) -> Result<(), CaptureError> {
    if contains_secret(prompt) || contains_secret(reply) {
        return Err(CaptureError::InvalidEvidence);
    }
    Ok(())
}
