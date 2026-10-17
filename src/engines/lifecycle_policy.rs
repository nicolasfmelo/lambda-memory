use crate::domain_models::lifecycle::{LifecycleRejection, SourceLifecycle};
/// Authority is caller-provided metadata, never inferred from confidence or prose.
pub fn admissible_at_capture(
    policy: &SourceLifecycle,
    event_ms: u64,
    arrival_ms: u64,
    previous_event_ms: Option<u64>,
) -> Result<(), LifecycleRejection> {
    if !policy.authoritative {
        return Err(LifecycleRejection::Unverified);
    }
    if policy.expires_at_ms.is_some_and(|t| t <= arrival_ms) {
        return Err(LifecycleRejection::Expired);
    }
    if previous_event_ms.is_some_and(|t| event_ms <= t) {
        return Err(LifecycleRejection::OlderOrEqual);
    }
    Ok(())
}
/// Legacy name retained for the frozen V4 comparison; callers must pass capture arrival time.
pub fn admissible(
    policy: &SourceLifecycle,
    event_ms: u64,
    arrival_ms: u64,
    previous_event_ms: Option<u64>,
) -> Result<(), LifecycleRejection> {
    admissible_at_capture(policy, event_ms, arrival_ms, previous_event_ms)
}
pub fn unexpired(policy: &SourceLifecycle, now_ms: u64) -> bool {
    policy.authoritative && policy.expires_at_ms.is_none_or(|t| t > now_ms)
}
pub use crate::domain_models::embeddings::cosine;
#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain_models::{OwnerId, ProjectId, SourceId};
    fn policy() -> SourceLifecycle {
        SourceLifecycle {
            owner: OwnerId::new("o").unwrap(),
            project: ProjectId::new("p").unwrap(),
            source: SourceId::new("s").unwrap(),
            authoritative: true,
            expires_at_ms: None,
        }
    }
    #[test]
    fn rejects_unverified_expired_and_out_of_order() {
        let mut p = policy();
        assert_eq!(admissible(&p, 20, 100, Some(10)), Ok(()));
        assert_eq!(
            admissible(&p, 10, 100, Some(20)),
            Err(LifecycleRejection::OlderOrEqual)
        );
        assert_eq!(
            admissible(&p, 20, 100, Some(20)),
            Err(LifecycleRejection::OlderOrEqual)
        );
        p.authoritative = false;
        assert_eq!(
            admissible(&p, 30, 100, None),
            Err(LifecycleRejection::Unverified)
        );
        p.authoritative = true;
        p.expires_at_ms = Some(100);
        assert_eq!(
            admissible(&p, 30, 100, None),
            Err(LifecycleRejection::Expired)
        );
    }
    #[test]
    fn cosine_rejects_invalid_spaces() {
        assert!(cosine(&[0.0], &[1.0]).is_none());
        assert!(cosine(&[1.0], &[1.0, 2.0]).is_none());
        assert_eq!(cosine(&[1.0, 0.0], &[1.0, 0.0]), Some(1.0));
    }
}
