use crate::domain_models::OwnerId;
use crate::domain_models::capture::*;

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
